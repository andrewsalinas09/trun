//! `trun check lint` and `trun check test` (docs/05-checks.md, "Developing checks").
//!
//! `test` replays a check against a recorded run: the run's events are fed into
//! the same `RunData` the live agent uses, and the check is evaluated on the same
//! schedule it would have had, printing when each alert would have opened and
//! cleared. That makes it quick to tune thresholds without waiting for a live
//! failure.

use crate::client::Client;
use crate::fmt;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::path::PathBuf;
use std::process::ExitCode;
use trun_checks::{CheckRunner, CheckSource, DEFAULTS, DEFAULTS_NAME, RunData};
use trun_hub::Paths;
use trun_proto::{AlertState, Event, EventKind, RunSummary, now_ms};

#[derive(Subcommand)]
pub enum CheckAction {
    /// Compile check files and show their META (default: this project's and global checks)
    Lint { files: Vec<PathBuf> },
    /// Replay a check against a recorded run
    Test {
        /// A .star file, or `builtin:defaults`
        file: String,
        /// Run id, id prefix, or name
        #[arg(long)]
        run: String,
        /// Also run the built-in default checks alongside it
        #[arg(long)]
        with_defaults: bool,
    },
}

pub async fn run(paths: &Paths, action: CheckAction) -> Result<ExitCode> {
    match action {
        CheckAction::Lint { files } => lint(paths, files),
        CheckAction::Test {
            file,
            run,
            with_defaults,
        } => test(paths, &file, &run, with_defaults).await,
    }
}

fn source(spec: &str) -> Result<CheckSource> {
    if spec == "builtin:defaults" {
        return Ok(CheckSource {
            name: DEFAULTS_NAME.into(),
            origin: "builtin:defaults.star".into(),
            text: DEFAULTS.into(),
        });
    }
    let p = PathBuf::from(spec);
    let text = std::fs::read_to_string(&p).with_context(|| format!("reading {spec}"))?;
    let name = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(CheckSource {
        name,
        origin: spec.to_string(),
        text,
    })
}

fn lint(paths: &Paths, files: Vec<PathBuf>) -> Result<ExitCode> {
    let mut specs: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    if specs.is_empty() {
        let cwd = std::env::current_dir()?;
        let mut dirs = Vec::new();
        for dir in cwd.ancestors() {
            if dir.join(".trun").is_dir() {
                dirs.push(dir.join(".trun").join("checks"));
                break;
            }
        }
        dirs.push(paths.home.join("checks"));
        for d in dirs {
            if let Ok(rd) = std::fs::read_dir(&d) {
                let mut v: Vec<String> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|e| e == "star"))
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                v.sort();
                specs.extend(v);
            }
        }
        specs.push("builtin:defaults".into());
    }
    let mut failed = 0;
    for spec in &specs {
        let src = match source(spec) {
            Ok(s) => s,
            Err(e) => {
                failed += 1;
                println!("✗ {spec}\n  {e:#}");
                continue;
            }
        };
        match trun_checks::engine().compile(&src) {
            Ok(c) => {
                let m = &c.meta;
                println!(
                    "✓ {:<14} applies={} every={} grace={}{}   {}",
                    c.name,
                    m.applies.as_deref().unwrap_or("*"),
                    trun_checks::engine::fmt_secs(m.every),
                    trun_checks::engine::fmt_secs(m.grace),
                    if m.kill { " kill" } else { "" },
                    spec
                );
            }
            Err(e) => {
                failed += 1;
                println!("✗ {spec}\n{}", indent(&e));
            }
        }
    }
    Ok(if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// All events of a run: its SSE stream from the start, until `end` (or a lull,
/// for a run that is still going).
async fn fetch_events(c: &Client, id: &str) -> Result<Vec<Event>> {
    let mut sse = c.sse(&format!("/runs/{id}/events?since_seq=0")).await?;
    let mut out = Vec::new();
    loop {
        let next = tokio::time::timeout(std::time::Duration::from_secs(2), sse.next()).await;
        match next {
            Ok(Ok(Some(m))) if m.event == "event" => {
                out.push(serde_json::from_str::<Event>(&m.data)?)
            }
            Ok(Ok(Some(m))) if m.event == "end" => break,
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Err(_) => break,
            Ok(Err(e)) => return Err(e),
        }
    }
    Ok(out)
}

async fn test(paths: &Paths, spec: &str, reference: &str, with_defaults: bool) -> Result<ExitCode> {
    let mut sources = vec![source(spec)?];
    if with_defaults && sources[0].name != DEFAULTS_NAME {
        sources.push(source("builtin:defaults")?);
    }
    let c = Client::connect(paths, false).await?;
    let run: RunSummary = c.get(&format!("/runs/{}", crate::enc(reference))).await?;
    let events = fetch_events(&c, &run.id).await?;
    let Some(first) = events.first() else {
        bail!("run {} has no recorded events", run.id)
    };

    let mut runner = match CheckRunner::with_sources(sources, &run.name) {
        Ok(r) => r,
        Err(errs) => {
            for (name, e) in errs {
                println!("✗ {name}\n{}", indent(&e));
            }
            return Ok(ExitCode::from(1));
        }
    };
    let mut data = RunData::new(&run.id, &run.name, &run.project);
    data.config = trun_hub::config::Config::load(&paths.home).check_thresholds;

    let start = events
        .iter()
        .find(|e| {
            matches!(
                e.kind,
                EventKind::Lifecycle {
                    state: trun_proto::Lifecycle::Running,
                    ..
                }
            )
        })
        .map(|e| e.ts)
        .unwrap_or(first.ts);
    // A finished run replays to its end; a live one up to now.
    let end = run.ended_at.unwrap_or_else(now_ms).max(start);
    let step_ms = (runner.min_every().clamp(1.0, 60.0) * 1000.0) as i64;
    println!(
        "replaying {} against run {} \"{}\" ({}, {} events, every {})",
        runner.active_names().join(" + "),
        run.id,
        run.name,
        fmt::duration(end - start),
        events.len(),
        trun_checks::engine::fmt_secs(step_ms as f64 / 1000.0)
    );
    println!(
        "note: process, host and GPU samples aren't recorded yet, so proc_*/host_*/disk_free/gpu_* series are empty in replays"
    );

    let mut i = 0;
    let (mut evals, mut opened, mut errors) = (0usize, 0usize, 0usize);
    let mut t = start;
    loop {
        while i < events.len() && events[i].ts <= t {
            data.ingest(&events[i]);
            i += 1;
        }
        let out = runner.tick(&data, t);
        evals += out.evaluated.len();
        let at = format!("+{}", fmt::duration(t - start));
        for (check, e) in &out.errors {
            errors += 1;
            println!("  {at:>8}  error    {check}:\n{}", indent(&indent(e)));
        }
        for ch in &out.changes {
            let what = match ch.state {
                AlertState::Opened => {
                    opened += 1;
                    "opened "
                }
                AlertState::Updated => "updated",
                AlertState::Cleared => "cleared",
            };
            println!(
                "  {at:>8}  {what}  [{}] {}",
                ch.alert.level.as_str(),
                ch.alert.message
            );
        }
        for n in &out.notify {
            println!("  {at:>8}  notify   {n}");
        }
        if let Some(k) = &out.kill {
            println!("  {at:>8}  KILL     {k}");
        }
        if t >= end {
            break;
        }
        t = (t + step_ms).min(end);
    }
    let still = runner.tracker.open_alerts();
    println!(
        "summary: {evals} evaluations · {opened} alert(s) opened · {} open at the end · health {}{}",
        still.len(),
        runner.tracker.health().as_str(),
        if errors > 0 {
            format!(" · {errors} error(s)")
        } else {
            String::new()
        }
    );
    Ok(if errors > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}
