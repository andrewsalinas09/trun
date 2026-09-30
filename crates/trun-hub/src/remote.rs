//! Remote hosts over SSH (docs/07-remote.md, D27).
//!
//! Each remote host runs its own trun daemon: it owns its runs, evaluates checks,
//! and keeps full history in its own SQLite store, so it keeps working when the
//! connection drops. The hub keeps one link per host:
//!
//! 1. `ssh <target> <trun> hub ensure --json` starts the remote daemon if needed and
//!    returns its port and token.
//! 2. `ssh -N -L 127.0.0.1:<local>:127.0.0.1:<remote> <target>` forwards a local port
//!    to it, so the hub talks to it with the ordinary HTTP API.
//! 3. The remote fleet stream and each run's event stream are *mirrored* into the
//!    local agent and store, keeping sequence numbers; after a disconnect, mirroring
//!    resumes from the highest event stored locally.
//!
//! The SSH client is configurable per host (`["ssh"]`, or `["wsl", "ssh"]` when the
//! working SSH setup lives in WSL), so `~/.ssh/config`, ProxyJump, agents and
//! Tailscale SSH all behave exactly as they do for the user.

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use tokio::task::JoinHandle;
use trun_agent::Agent;
use trun_proto::{
    Alert, AlertLevel, DaemonInfo, Event, FleetEvent, Health, HostConfig, HostState, HostStatus,
    RunSummary, now_ms,
};
use trun_store::Store;

const ENSURE_TIMEOUT: Duration = Duration::from_secs(60);
const FORWARD_READY_TIMEOUT: Duration = Duration::from_secs(20);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// How many recent remote runs to reconcile on connect.
const SYNC_RECENT: u32 = 300;

/// HTTP client for one remote daemon (through its SSH forward).
#[derive(Clone)]
pub struct RemoteClient {
    http: reqwest::Client,
    base: String,
    token: String,
    /// `Host` the remote daemon expects (its own loopback port): requests arrive
    /// through the forward addressed to our local port, which its DNS-rebinding
    /// guard would reject.
    host_header: String,
}

impl RemoteClient {
    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(m, format!("{}/api{path}", self.base))
            .header(reqwest::header::HOST, &self.host_header)
            .bearer_auth(&self.token)
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let r = self.req(reqwest::Method::GET, path).send().await?;
        decode(r).await
    }

    pub async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let r = self
            .req(reqwest::Method::POST, path)
            .json(body)
            .send()
            .await?;
        decode(r).await
    }

    /// Open an SSE stream; yields (event, data) pairs.
    pub async fn sse(&self, path: &str) -> Result<SseStream> {
        let r = self
            .req(reqwest::Method::GET, path)
            .header("accept", "text/event-stream")
            .send()
            .await?;
        if !r.status().is_success() {
            bail!("{} from remote {path}", r.status());
        }
        Ok(SseStream {
            inner: Box::pin(r.bytes_stream()),
            buf: String::new(),
        })
    }
}

async fn decode<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T> {
    let status = r.status();
    let body = r.text().await?;
    if !status.is_success() {
        let msg = serde_json::from_str::<trun_proto::ApiError>(&body)
            .map(|e| e.error)
            .unwrap_or(body);
        bail!("remote: {status}: {msg}");
    }
    Ok(serde_json::from_str(&body)?)
}

type Bytes =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>;

pub struct SseStream {
    inner: Bytes,
    buf: String,
}

impl SseStream {
    pub async fn next(&mut self) -> Result<Option<(String, String)>> {
        loop {
            if let Some(end) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..end + 2).collect();
                let (mut event, mut data) = (String::from("message"), Vec::new());
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
                    }
                }
                if !data.is_empty() {
                    return Ok(Some((event, data.join("\n"))));
                }
                continue;
            }
            match self.inner.next().await {
                Some(chunk) => self
                    .buf
                    .push_str(&String::from_utf8_lossy(&chunk?).replace("\r\n", "\n")),
                None => return Ok(None),
            }
        }
    }
}

/// How long a forwarded request waits for a link that is still connecting.
const CONNECT_WAIT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct LinkState {
    status: Option<HostStatus>,
    error: Option<String>,
    info: Option<DaemonInfo>,
    last_seen: Option<i64>,
    client: Option<RemoteClient>,
    /// Remote clock minus ours, measured when the link came up.
    clock_offset: Option<i64>,
}

struct Link {
    config: HostConfig,
    state: Arc<Mutex<LinkState>>,
    task: JoinHandle<()>,
}

/// All host links of this hub.
#[derive(Clone)]
pub struct Links {
    links: Arc<Mutex<HashMap<String, Link>>>,
    agent: Agent,
    store: Store,
}

impl Links {
    pub fn new(agent: Agent, store: Store) -> Self {
        Links {
            links: Arc::default(),
            agent,
            store,
        }
    }

    /// Start links for every configured host.
    pub fn start_all(&self) {
        match self.store.hosts() {
            Ok(hosts) => {
                for h in hosts {
                    self.start(h);
                }
            }
            Err(e) => tracing::error!(error = %e, "cannot read configured hosts"),
        }
    }

    fn start(&self, config: HostConfig) {
        let state: Arc<Mutex<LinkState>> = Arc::default();
        state.lock().unwrap().status = Some(HostStatus::Connecting);
        let task = tokio::spawn(run_link(
            config.clone(),
            state.clone(),
            self.agent.clone(),
            self.store.clone(),
        ));
        if let Some(old) = self.links.lock().unwrap().insert(
            config.name.clone(),
            Link {
                config,
                state,
                task,
            },
        ) {
            old.task.abort();
        }
    }

    pub fn add(&self, config: HostConfig) -> Result<()> {
        self.store.put_host(&config)?;
        self.start(config);
        Ok(())
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        if let Some(l) = self.links.lock().unwrap().remove(name) {
            l.task.abort(); // kill_on_drop ends the ssh processes
        }
        self.store.delete_host(name)
    }

    pub fn client(&self, name: &str) -> Option<RemoteClient> {
        self.links
            .lock()
            .unwrap()
            .get(name)
            .and_then(|l| l.state.lock().unwrap().client.clone())
    }

    /// The client for `name`, waiting up to [`CONNECT_WAIT`] while the link is still
    /// coming up (right after the hub starts, or while it reconnects), so a request
    /// doesn't fail just because it raced the tunnel. `Err` explains why not.
    pub async fn connected(&self, name: &str) -> std::result::Result<RemoteClient, String> {
        let deadline = tokio::time::Instant::now() + CONNECT_WAIT;
        loop {
            let (client, status, error) = {
                let links = self.links.lock().unwrap();
                let Some(l) = links.get(name) else {
                    return Err(format!("unknown host '{name}' (see `trun hosts`)"));
                };
                let st = l.state.lock().unwrap();
                (st.client.clone(), st.status, st.error.clone())
            };
            if let Some(c) = client {
                return Ok(c);
            }
            let connecting = matches!(status, None | Some(HostStatus::Connecting));
            if !connecting || tokio::time::Instant::now() >= deadline {
                return Err(match error {
                    Some(e) => format!("host '{name}' is not connected right now: {e}"),
                    None => format!("host '{name}' is not connected right now"),
                });
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Bring a summary from `host` onto this hub's clock.
    pub fn localize(&self, host: &str, s: &mut RunSummary) {
        let off = self
            .links
            .lock()
            .unwrap()
            .get(host)
            .and_then(|l| l.state.lock().unwrap().clock_offset)
            .unwrap_or(0);
        s.shift_times(-off);
    }

    pub fn has(&self, name: &str) -> bool {
        self.links.lock().unwrap().contains_key(name)
    }

    pub fn states(&self) -> Vec<HostState> {
        let links = self.links.lock().unwrap();
        let mut v: Vec<HostState> = links
            .values()
            .map(|l| {
                let st = l.state.lock().unwrap();
                HostState {
                    config: l.config.clone(),
                    status: st.status.unwrap_or(HostStatus::Connecting),
                    error: st.error.clone(),
                    // Never hand out the remote token.
                    info: st.info.clone().map(|mut i| {
                        i.token.clear();
                        i
                    }),
                    last_seen: st.last_seen,
                    active_runs: self.agent.mirrored_active(&l.config.name).len() as u32,
                    clock_offset_ms: st.clock_offset,
                }
            })
            .collect();
        v.sort_by(|a, b| a.config.name.cmp(&b.config.name));
        v
    }
}

/// The SSH client argv. `wsl ssh …` gets `--exec` so WSL doesn't hand the command
/// line to a shell (which would expand `~` and `$VARS` on the wrong machine).
pub fn ssh_argv(config: &HostConfig) -> Vec<String> {
    let mut argv = if config.ssh.is_empty() {
        vec!["ssh".to_string()]
    } else {
        config.ssh.clone()
    };
    let is_wsl = argv.first().is_some_and(|p| {
        let stem = std::path::Path::new(p)
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase());
        stem.as_deref() == Some("wsl")
    });
    if is_wsl && argv.get(1).is_some_and(|a| a != "--exec" && a != "-e") {
        argv.insert(1, "--exec".into());
    }
    argv
}

pub fn ssh_command(config: &HostConfig, extra: &[&str]) -> Command {
    let argv = ssh_argv(config);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=15"])
        .args(extra);
    cmd.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    {
        // The hub has no console; don't let ssh/wsl pop one up.
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

/// Turn raw ssh stderr into an actionable message.
pub fn explain_ssh_error(config: &HostConfig, stderr: &str) -> String {
    let s = stderr.trim();
    let hint = if s.contains("Tailscale SSH requires an additional check") {
        Some(format!(
            "Tailscale SSH is in check mode and needs a browser approval: run `ssh {}` once interactively",
            config.target
        ))
    } else if s.contains("Permission denied") {
        Some("SSH authentication failed; the hub can't prompt for passwords (BatchMode), so use keys or an agent".into())
    } else if s.contains("Could not resolve hostname") {
        Some(format!(
            "unknown host '{}': add it to ~/.ssh/config, or if it is configured inside WSL use `--ssh \"wsl ssh\"`",
            config.target
        ))
    } else if s.contains("timed out")
        || s.contains("Connection refused")
        || s.contains("No route to host")
    {
        Some("host unreachable (is it on and on the network?)".into())
    } else if s.contains("not found") || s.contains("No such file") {
        Some(format!(
            "trun is not installed at '{}' on the host: use `trun hosts add {} --install`",
            config.trun_path, config.name
        ))
    } else {
        None
    };
    let last = s
        .lines()
        .rfind(|l| !l.starts_with("Warning: Permanently added"))
        .unwrap_or("")
        .to_string();
    match hint {
        Some(h) if last.is_empty() => h,
        Some(h) => format!("{h} ({last})"),
        None if last.is_empty() => "ssh failed".into(),
        None => last,
    }
}

async fn run_link(config: HostConfig, state: Arc<Mutex<LinkState>>, agent: Agent, store: Store) {
    let mut backoff = Duration::from_secs(2);
    loop {
        state.lock().unwrap().status = Some(HostStatus::Connecting);
        let started = std::time::Instant::now();
        let err = match connect_and_mirror(&config, &state, &agent, &store).await {
            Ok(()) => "connection closed".to_string(),
            Err(e) => format!("{e:#}"),
        };
        {
            let mut st = state.lock().unwrap();
            st.status = Some(HostStatus::Offline);
            st.client = None;
            st.error = Some(err.clone());
        }
        tracing::warn!(host = %config.name, error = %err, "host link down");
        mark_connection_lost(&agent, &config.name, &err);
        // A link that stayed up a while reconnects quickly; repeated failures back off.
        if started.elapsed() > Duration::from_secs(60) {
            backoff = Duration::from_secs(2);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// While a host is unreachable, its active runs can't be observed: say so on them
/// (health stalled) so `trun wait` and the UI don't report stale "healthy" data.
fn mark_connection_lost(agent: &Agent, host: &str, why: &str) {
    for mut s in agent.mirrored_active(host) {
        let key = format!("hub:connection:{host}");
        if s.alerts.iter().any(|a| a.key == key) {
            continue;
        }
        let now = now_ms();
        s.alerts.push(Alert {
            key,
            check: "hub".into(),
            level: AlertLevel::Stalled,
            message: format!("host '{host}' unreachable: {why}"),
            opened_at: now,
            last_at: now,
            count: 1,
        });
        s.health = s.health.max_with(Health::Stalled);
        agent.mirror_summary(host, s);
    }
}

trait HealthMax {
    fn max_with(self, other: Health) -> Health;
}

impl HealthMax for Health {
    fn max_with(self, other: Health) -> Health {
        let rank = |h: Health| match h {
            Health::Ok => 0,
            Health::Warn => 1,
            Health::Stalled => 2,
            Health::Failing => 3,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

async fn ensure_remote(config: &HostConfig) -> Result<DaemonInfo> {
    let mut cmd = ssh_command(
        config,
        &[&config.target, &config.trun_path, "hub", "ensure", "--json"],
    );
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = tokio::time::timeout(ENSURE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            anyhow!(
                "ssh {} timed out after {}s",
                config.target,
                ENSURE_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("running {}", ssh_argv(config).join(" ")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        bail!("{}", explain_ssh_error(config, &stderr));
    }
    let line = stdout
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .ok_or_else(|| {
            anyhow!(
                "unexpected output from remote `trun hub ensure`: {}",
                stdout.trim().chars().take(200).collect::<String>()
            )
        })?;
    serde_json::from_str(line).context("parsing remote daemon info")
}

fn free_local_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

async fn connect_and_mirror(
    config: &HostConfig,
    state: &Arc<Mutex<LinkState>>,
    agent: &Agent,
    store: &Store,
) -> Result<()> {
    let info = ensure_remote(config).await?;
    let local_port = free_local_port()?;
    let forward = format!("127.0.0.1:{local_port}:127.0.0.1:{}", info.port);
    let mut tunnel = ssh_command(
        config,
        &[
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
            "-L",
            &forward,
            &config.target,
        ],
    );
    tunnel.stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = tunnel.spawn().context("starting the ssh tunnel")?;
    let mut tunnel_err = child.stderr.take();

    let client = RemoteClient {
        http: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()?,
        base: format!("http://127.0.0.1:{local_port}"),
        token: info.token.clone(),
        host_header: format!("127.0.0.1:{}", info.port),
    };
    // Wait for the forward to carry requests.
    let deadline = tokio::time::Instant::now() + FORWARD_READY_TIMEOUT;
    let mut last_err: String;
    loop {
        if let Some(status) = child.try_wait()? {
            let mut msg = String::new();
            if let Some(mut e) = tunnel_err.take() {
                use tokio::io::AsyncReadExt;
                let _ = e.read_to_string(&mut msg).await;
            }
            bail!(
                "ssh tunnel exited ({status}): {}",
                explain_ssh_error(config, &msg)
            );
        }
        match client.get_json::<Vec<RunSummary>>("/runs?limit=1").await {
            Ok(_) => break,
            Err(e) => last_err = format!("{e:#}"),
        }
        if tokio::time::Instant::now() > deadline {
            bail!(
                "the ssh tunnel to {} did not come up within {}s (last error: {})",
                config.target,
                FORWARD_READY_TIMEOUT.as_secs(),
                last_err
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let offset = measure_offset(&client).await;
    if offset.abs() > 2000 {
        tracing::warn!(host = %config.name, offset_ms = offset, "remote clock differs from ours; timestamps are adjusted");
    }
    {
        let mut st = state.lock().unwrap();
        st.clock_offset = Some(offset);
        st.status = Some(HostStatus::Online);
        st.error = None;
        st.info = Some(info.clone());
        st.last_seen = Some(now_ms());
        st.client = Some(client.clone());
    }
    tracing::info!(host = %config.name, remote = %info.hostname, version = %info.version, "host link up");

    let replicating: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mirror = Mirror {
        host: config.name.clone(),
        client: client.clone(),
        agent: agent.clone(),
        store: store.clone(),
        replicating,
        shift: -offset,
    };

    // Subscribe to the fleet first, then reconcile, so nothing slips between.
    let mut fleet = client.sse("/stream").await?;
    let recent: Vec<RunSummary> = client
        .get_json(&format!("/runs?limit={SYNC_RECENT}"))
        .await?;
    for s in recent {
        mirror.observe(s).await;
    }

    loop {
        tokio::select! {
            status = child.wait() => {
                let mut msg = String::new();
                if let Some(mut e) = tunnel_err.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = e.read_to_string(&mut msg).await;
                }
                bail!("ssh tunnel closed ({}): {}", status.map(|s| s.to_string()).unwrap_or_default(), explain_ssh_error(config, &msg));
            }
            msg = fleet.next() => match msg.map_err(|e| {
                anyhow!("lost the connection to the host's trun daemon (ssh tunnel interrupted?): {}", root_cause(&e))
            })? {
                Some((ev, data)) if ev == "run" => {
                    state.lock().unwrap().last_seen = Some(now_ms());
                    if let Ok(FleetEvent::Run(s)) = serde_json::from_str::<FleetEvent>(&data) {
                        mirror.observe(s).await;
                    }
                }
                Some(_) => {}
                None => return Ok(()),
            },
        }
    }
}

fn root_cause(e: &anyhow::Error) -> String {
    e.chain().last().map(|c| c.to_string()).unwrap_or_default()
}

/// Remote clock minus ours: the best (lowest round-trip) of a few health probes.
async fn measure_offset(client: &RemoteClient) -> i64 {
    let mut best: Option<(i64, i64)> = None; // (rtt, offset)
    for _ in 0..5 {
        let t0 = now_ms();
        let Ok(h) = client.get_json::<trun_proto::HealthInfo>("/health").await else {
            continue;
        };
        let t1 = now_ms();
        if h.now == 0 {
            continue; // an older daemon without a clock in /health
        }
        let (rtt, off) = (t1 - t0, h.now - (t0 + t1) / 2);
        if best.is_none_or(|(r, _)| rtt < r) {
            best = Some((rtt, off));
        }
    }
    best.map(|(_, o)| o).unwrap_or(0)
}

/// Mirrors one host's runs into the local agent and store.
#[derive(Clone)]
struct Mirror {
    host: String,
    client: RemoteClient,
    agent: Agent,
    store: Store,
    replicating: Arc<Mutex<HashSet<String>>>,
    /// Added to remote timestamps (minus the remote clock offset).
    shift: i64,
}

impl Mirror {
    /// A remote summary arrived: mirror it, and start replicating its events if the
    /// local copy is behind.
    async fn observe(&self, mut s: RunSummary) {
        s.shift_times(self.shift);
        let (project, id, remote_seq) = (s.project.clone(), s.id.clone(), s.last_seq);
        self.agent.mirror_summary(&self.host, s);
        if self.replicating.lock().unwrap().contains(&id) {
            return;
        }
        let store = self.store.clone();
        let (p2, i2) = (project.clone(), id.clone());
        let local = tokio::task::spawn_blocking(move || store.max_event_seq(&p2, &i2))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(0);
        if local >= remote_seq {
            return;
        }
        self.replicating.lock().unwrap().insert(id.clone());
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.replicate(&id, local).await {
                tracing::debug!(host = %me.host, run = %id, error = %e, "replication interrupted");
            }
            me.replicating.lock().unwrap().remove(&id);
        });
    }

    async fn replicate(&self, id: &str, since: u64) -> Result<()> {
        let mut sse = self
            .client
            .sse(&format!("/runs/{id}/events?since_seq={since}"))
            .await?;
        while let Some((ev, data)) = sse.next().await? {
            match ev.as_str() {
                "event" => {
                    if let Ok(mut e) = serde_json::from_str::<Event>(&data) {
                        e.ts += self.shift;
                        self.agent.mirror_event(e);
                    }
                }
                "end" => {
                    if let Ok(mut s) = serde_json::from_str::<RunSummary>(&data) {
                        s.shift_times(self.shift);
                        self.agent.mirror_summary(&self.host, s);
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(ssh: &[&str]) -> HostConfig {
        HostConfig {
            name: "pi".into(),
            target: "pi".into(),
            ssh: ssh.iter().map(|s| s.to_string()).collect(),
            trun_path: ".local/bin/trun".into(),
        }
    }

    #[test]
    fn wsl_gets_exec() {
        assert_eq!(
            ssh_argv(&cfg(&["wsl", "ssh"])),
            vec!["wsl", "--exec", "ssh"]
        );
        assert_eq!(
            ssh_argv(&cfg(&["wsl.exe", "-e", "ssh"])),
            vec!["wsl.exe", "-e", "ssh"]
        );
        assert_eq!(ssh_argv(&cfg(&[])), vec!["ssh"]);
        assert_eq!(
            ssh_argv(&cfg(&["C:\\Windows\\System32\\OpenSSH\\ssh.exe"])).len(),
            1
        );
    }

    #[test]
    fn explains_common_failures() {
        let c = cfg(&["ssh"]);
        assert!(
            explain_ssh_error(
                &c,
                "# Tailscale SSH requires an additional check.\n# To authenticate, visit: https://x"
            )
            .contains("browser approval")
        );
        assert!(
            explain_ssh_error(
                &c,
                "ssh: Could not resolve hostname pi: Name or service not known"
            )
            .contains("wsl ssh")
        );
        assert!(
            explain_ssh_error(&c, "andre@andrew: Permission denied (publickey).")
                .contains("authentication")
        );
        assert!(
            explain_ssh_error(
                &c,
                "bash: line 1: .local/bin/trun: No such file or directory"
            )
            .contains("--install")
        );
    }
}
