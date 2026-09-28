//! `trun` — run, watch, and inspect long-running commands (docs/03-cli.md).

mod client;
mod daemon;
mod fmt;
mod printer;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use client::Client;
use printer::Printer;
use std::io::Write;
use std::process::ExitCode;
use trun_hub::Paths;
use trun_proto::{
    CancelRun, CreateRun, DEFAULT_PORT, Event, EventKind, LogPage, ProjectInfo, RunSummary, now_ms,
};

/// Exit code for trun's own failures (usage, connection), per docs/03-cli.md.
const EXIT_TRUN_ERROR: u8 = 10;

#[derive(Parser)]
#[command(
    name = "trun",
    version,
    about = "Live visibility into long-running tasks, for humans and AI agents"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a command under trun supervision
    Run(RunArgs),
    /// List runs
    Ls(LsArgs),
    /// Show a digest of one run
    Status { run: String },
    /// Print a run's output
    Logs(LogsArgs),
    /// Cancel a run (graceful, then forced after 10s)
    Cancel {
        run: String,
        /// Kill immediately
        #[arg(long)]
        force: bool,
    },
    /// List projects
    Projects,
    /// Open the web UI
    Ui {
        /// Open this run directly
        run: Option<String>,
    },
    /// Manage the hub daemon
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Human name (default: derived from the command)
    #[arg(long)]
    name: Option<String>,
    /// Project (default: .trun/config.toml name, else git repo name, else _adhoc)
    #[arg(long)]
    project: Option<String>,
    /// Working directory (default: current)
    #[arg(long)]
    cwd: Option<std::path::PathBuf>,
    /// Extra environment variable, KEY=VALUE (repeatable)
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,
    /// Return immediately and print the run id instead of streaming output
    #[arg(short, long)]
    detach: bool,
    /// The command to run (after --)
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

#[derive(Args)]
struct LsArgs {
    /// Only queued/starting/running runs
    #[arg(short, long)]
    active: bool,
    #[arg(long)]
    project: Option<String>,
    /// Name glob, e.g. 'train*'
    #[arg(long)]
    name: Option<String>,
    #[arg(short = 'n', long, default_value_t = 30)]
    limit: u32,
    /// Print JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct LogsArgs {
    run: String,
    /// Keep streaming until the run ends
    #[arg(short, long)]
    follow: bool,
    /// Only lines matching this regex
    #[arg(long)]
    grep: Option<String>,
    /// Last N lines (default 200 unless --since-seq is given)
    #[arg(long)]
    tail: Option<u32>,
    /// Lines after this sequence number
    #[arg(long)]
    since_seq: Option<u64>,
    /// stdout or stderr
    #[arg(long)]
    stream: Option<String>,
    /// Prefix each line with its sequence number
    #[arg(long)]
    seq: bool,
}

#[derive(Subcommand)]
enum HubAction {
    /// Start the hub (in the background unless --foreground)
    Start {
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running hub
    Stop,
    /// Show hub status
    Status,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(dispatch(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("trun: {e:#}");
            ExitCode::from(EXIT_TRUN_ERROR)
        }
    }
}

async fn dispatch(cli: Cli) -> Result<ExitCode> {
    let paths = Paths::resolve()?;
    match cli.cmd {
        Cmd::Hub { action } => hub(&paths, action).await,
        Cmd::Run(args) => run(&paths, args).await,
        Cmd::Ls(args) => ls(&paths, args).await,
        Cmd::Status { run } => status(&paths, &run).await,
        Cmd::Logs(args) => logs(&paths, args).await,
        Cmd::Cancel { run, force } => {
            let c = Client::connect(&paths, false).await?;
            let r: RunSummary = c
                .post(
                    &format!("/runs/{}/cancel", enc(&run)),
                    &CancelRun {
                        force,
                        reason: None,
                    },
                )
                .await?;
            eprintln!("cancelling {} ({})", r.id, r.name);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Projects => {
            let c = Client::connect(&paths, false).await?;
            let ps: Vec<ProjectInfo> = c.get("/projects").await?;
            println!("{:<28} {:>6} {:>7}  LAST RUN", "PROJECT", "RUNS", "ACTIVE");
            for p in ps {
                let last = p.last_run_at.map(fmt::ago).unwrap_or_else(|| "-".into());
                println!(
                    "{:<28} {:>6} {:>7}  {}",
                    fmt::truncate(&p.name, 28),
                    p.run_count,
                    p.active_count,
                    last
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Ui { run } => {
            let c = Client::connect(&paths, true).await?;
            let mut url = format!("{}/?t={}", c.base, c.token());
            if let Some(r) = run {
                let s: RunSummary = c.get(&format!("/runs/{}", enc(&r))).await?;
                url.push_str(&format!("#/runs/{}", s.id));
            }
            if open::that_detached(&url).is_err() {
                println!("{url}");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

async fn hub(paths: &Paths, action: HubAction) -> Result<ExitCode> {
    match action {
        HubAction::Start { port, foreground } => {
            if foreground {
                init_logging();
                trun_hub::serve(trun_hub::HubConfig {
                    paths: paths.clone(),
                    port,
                })
                .await?;
                return Ok(ExitCode::SUCCESS);
            }
            let c = Client::connect(paths, true).await?;
            let h = c.health().await?;
            println!("hub running at {} (pid {})", c.base, h.pid);
            Ok(ExitCode::SUCCESS)
        }
        HubAction::Stop => {
            let c = match Client::connect(paths, false).await {
                Ok(c) => c,
                Err(_) => {
                    println!("no hub running");
                    return Ok(ExitCode::SUCCESS);
                }
            };
            c.post_empty("/shutdown").await?;
            for _ in 0..50 {
                if c.health().await.is_err() {
                    println!("hub stopped");
                    return Ok(ExitCode::SUCCESS);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            anyhow::bail!("hub did not stop within 5s")
        }
        HubAction::Status => match Client::connect(paths, false).await {
            Ok(c) => {
                let h = c.health().await?;
                let active: Vec<RunSummary> = c.get("/runs?active=true").await?;
                println!(
                    "hub {} at {} (pid {}, up {})",
                    h.version,
                    c.base,
                    h.pid,
                    fmt::duration(now_ms() - h.started_at)
                );
                println!("data: {}", h.data_dir);
                println!("active runs: {}", active.len());
                Ok(ExitCode::SUCCESS)
            }
            Err(_) => {
                println!("no hub running");
                Ok(ExitCode::from(1))
            }
        },
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("TRUN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

async fn run(paths: &Paths, args: RunArgs) -> Result<ExitCode> {
    let cwd = match args.cwd {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    let cwd = std::fs::canonicalize(&cwd)
        .with_context(|| format!("working directory {}", cwd.display()))?;
    let cwd = strip_verbatim(&cwd);
    let env = args
        .env
        .iter()
        .map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .context(format!("--env expects KEY=VALUE, got '{kv}'"))
        })
        .collect::<Result<Vec<_>>>()?;

    let c = Client::connect(paths, true).await?;
    let req = CreateRun {
        cmd: args.command,
        cwd,
        name: args.name,
        project: args.project,
        env,
    };
    let r: RunSummary = c.post("/runs", &req).await?;

    if args.detach {
        println!("{}", r.id);
        eprintln!(
            "trun: started {} ({}) in project {} — follow with `trun logs -f {}`",
            r.id, r.name, r.project, r.id
        );
        return Ok(ExitCode::SUCCESS);
    }
    eprintln!(
        "trun: run {} ({}) · project {} · `trun ui {}` to watch",
        r.id, r.name, r.project, r.id
    );
    follow(&c, &r.id, 0, true, false).await
}

/// `canonicalize` on Windows yields `\\?\C:\...`; child processes and humans prefer `C:\...`.
fn strip_verbatim(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        _ => s.into_owned(),
    }
}

/// Stream a run's events to the terminal until it ends. With `cancel_on_ctrl_c`,
/// the first Ctrl-C cancels the run and the second forces it.
async fn follow(
    c: &Client,
    id: &str,
    since: u64,
    cancel_on_ctrl_c: bool,
    show_seq: bool,
) -> Result<ExitCode> {
    let mut printer = Printer::new(show_seq);
    let mut last = since;
    let mut ctrl_c_count = 0;
    let mut retries = 0;
    loop {
        let mut sse = match c
            .sse(&format!("/runs/{}/events?since_seq={last}", enc(id)))
            .await
        {
            Ok(s) => s,
            Err(e) if retries < 5 => {
                retries += 1;
                tracing::debug!("reconnecting: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => return Err(e),
        };
        loop {
            let msg = tokio::select! {
                m = sse.next() => m,
                _ = tokio::signal::ctrl_c(), if cancel_on_ctrl_c => {
                    ctrl_c_count += 1;
                    let force = ctrl_c_count > 1;
                    printer.finish_provisional();
                    eprintln!("trun: {} run {id} (Ctrl-C again to force)", if force { "killing" } else { "cancelling" });
                    let _ = c.post::<_, RunSummary>(&format!("/runs/{}/cancel", enc(id)), &CancelRun { force, reason: None }).await;
                    continue;
                }
            };
            match msg {
                Ok(Some(m)) if m.event == "event" => {
                    let ev: Event = serde_json::from_str(&m.data)?;
                    last = ev.seq;
                    if let EventKind::Output { .. } = ev.kind {
                        printer.print(&ev);
                    }
                }
                Ok(Some(m)) if m.event == "end" => {
                    let s: RunSummary = serde_json::from_str(&m.data)?;
                    printer.finish_provisional();
                    print_final(&s);
                    let code = match s.lifecycle {
                        trun_proto::Lifecycle::Succeeded => 0,
                        _ => match (s.exit_code, s.signal) {
                            (Some(c), _) => c.clamp(1, 255),
                            (None, Some(sig)) => (128 + sig).clamp(1, 255),
                            _ => 1,
                        },
                    };
                    return Ok(ExitCode::from(code as u8));
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break, // disconnected: reconnect from `last`
            }
        }
        retries += 1;
        if retries > 20 {
            anyhow::bail!("lost connection to the hub");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

fn print_final(s: &RunSummary) {
    let took = s
        .elapsed_ms(now_ms())
        .map(|d| format!(" after {}", fmt::duration(d)))
        .unwrap_or_default();
    let mut line = format!("trun: {} {}{took}", fmt::symbol(s.lifecycle), fmt::state(s));
    if let Some(d) = &s.diagnosis {
        line.push_str(&format!(
            "\ntrun:   {} — {}{}",
            d.cause,
            d.summary,
            if d.early { " [died early]" } else { "" }
        ));
    }
    let _ = std::io::stdout().flush();
    eprintln!("{line}");
}

async fn ls(paths: &Paths, args: LsArgs) -> Result<ExitCode> {
    let c = Client::connect(paths, false).await?;
    let mut q = format!("/runs?limit={}", args.limit);
    if args.active {
        q.push_str("&active=true");
    }
    if let Some(p) = &args.project {
        q.push_str(&format!("&project={}", enc(p)));
    }
    if let Some(n) = &args.name {
        q.push_str(&format!("&name={}", enc(n)));
    }
    let runs: Vec<RunSummary> = c.get(&q).await?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&runs)?);
        return Ok(ExitCode::SUCCESS);
    }
    if runs.is_empty() {
        println!("no runs");
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{:<26}  {:<22} {:<16} {:<20} {:>9}  STARTED",
        "ID", "NAME", "PROJECT", "STATE", "DURATION"
    );
    let now = now_ms();
    for r in runs {
        let dur = r
            .elapsed_ms(now)
            .map(fmt::duration)
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<26}  {:<22} {:<16} {:<20} {:>9}  {}",
            r.id,
            fmt::truncate(&r.name, 22),
            fmt::truncate(&r.project, 16),
            format!(
                "{} {}",
                fmt::symbol(r.lifecycle),
                fmt::truncate(&fmt::state(&r), 18)
            ),
            dur,
            fmt::ago(r.created_at)
        );
    }
    Ok(ExitCode::SUCCESS)
}

async fn status(paths: &Paths, reference: &str) -> Result<ExitCode> {
    let c = Client::connect(paths, false).await?;
    let r: RunSummary = c.get(&format!("/runs/{}", enc(reference))).await?;
    let page: LogPage = c.get(&format!("/runs/{}/logs?tail=10", r.id)).await?;
    let now = now_ms();
    println!(
        "run {} \"{}\" · project {} · host {}",
        r.id, r.name, r.project, r.host
    );
    let took = r
        .elapsed_ms(now)
        .map(|d| format!(" · {}", fmt::duration(d)))
        .unwrap_or_default();
    println!(
        "state: {} {}{} · health {}",
        fmt::symbol(r.lifecycle),
        fmt::state(&r),
        took,
        r.health.as_str()
    );
    println!("cmd:   {}", fmt::command(&r.cmd));
    println!("cwd:   {}", r.cwd);
    match &r.diagnosis {
        Some(d) => {
            println!(
                "diagnosis: {} — {}{}",
                d.cause,
                d.summary,
                if d.early { " [died early]" } else { "" }
            );
            for e in &d.evidence {
                println!("  [{}] {}", e.seq, fmt::truncate(e.text.trim_end(), 160));
            }
        }
        None => println!("diagnosis: –"),
    }
    match r.last_output_at {
        Some(t) => println!("last output {} (seq {}):", fmt::ago(t), r.last_seq),
        None => println!("no output yet"),
    }
    for l in page.lines {
        println!("  [{}] {}", l.seq, fmt::truncate(l.text.trim_end(), 200));
    }
    Ok(ExitCode::SUCCESS)
}

async fn logs(paths: &Paths, args: LogsArgs) -> Result<ExitCode> {
    let c = Client::connect(paths, false).await?;
    let r: RunSummary = c.get(&format!("/runs/{}", enc(&args.run))).await?;
    let mut q = String::new();
    match (args.since_seq, args.tail) {
        (Some(s), t) => {
            q.push_str(&format!("since_seq={s}&limit=100000"));
            if let Some(t) = t {
                q.push_str(&format!("&tail={t}"));
            }
        }
        (None, t) => q.push_str(&format!("tail={}", t.unwrap_or(200))),
    }
    if let Some(g) = &args.grep {
        q.push_str(&format!("&grep={}", enc(g)));
    }
    if let Some(s) = &args.stream {
        q.push_str(&format!("&stream={}", enc(s)));
    }
    let page: LogPage = c.get(&format!("/runs/{}/logs?{q}", r.id)).await?;
    let mut printer = Printer::new(args.seq);
    for l in &page.lines {
        printer.print_line(l.stream, &l.text, false, l.seq);
    }
    printer.finish_provisional();
    if args.follow && !r.lifecycle.is_terminal() {
        let since = page
            .next_seq
            .max(args.since_seq.unwrap_or(0))
            .max(if page.lines.is_empty() { r.last_seq } else { 0 });
        if args.grep.is_some() {
            eprintln!("trun: note: --grep is not applied to followed output");
        }
        return follow(&c, &r.id, since, false, args.seq).await;
    }
    Ok(ExitCode::SUCCESS)
}
