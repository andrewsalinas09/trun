//! The agent owns the runs on one host (docs/02-architecture.md).
//!
//! It is embedded in the hub process for now; the event flow is already the one
//! remote agents will use: every event gets a per-run `seq`, is kept in a bounded
//! in-memory ring for live subscribers, broadcast, and appended to the store.
//!
//! Per run: output lines go through the parsers (`::` protocol + built-ins), the
//! side channel feeds directives directly, and [`structure::Structure`] turns
//! directives into steps, progress rates, and latest metrics.

mod naming;
pub mod project;
mod sidechannel;
mod structure;

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use trun_parse::{Diagnoser, ExitInfo, ParserSet};
use trun_proto::{
    CreateRun, Directive, Event, EventKind, Health, Lifecycle, LogLevel, RunSummary, Stream, now_ms,
};
use trun_store::Store;
use trun_supervise::{ExitStatusInfo, Killer, SpawnSpec};

use sidechannel::SideMsg;
use structure::Structure;

/// Events kept in memory per active run, for subscribers joining mid-stream.
const RING_CAP: usize = 4096;
/// After the main process exits, how long to keep reading pipes held open by
/// leftover children before giving up on them.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Graceful cancel → hard kill after this long.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
/// Finished runs stay in memory this long so late subscribers get a seamless replay.
const RETAIN_FINISHED: Duration = Duration::from_secs(60);
/// How often an active run republishes its summary (progress, metrics, activity).
const SUMMARY_PUBLISH_EVERY: Duration = Duration::from_secs(1);
/// Coalesced progress events go out on this tick.
const PROGRESS_TICK: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct Agent {
    inner: Arc<Inner>,
}

struct Inner {
    host: String,
    hub_url: String,
    store: Store,
    runs: Mutex<HashMap<String, Arc<RunHandle>>>,
    fleet: broadcast::Sender<RunSummary>,
}

pub struct RunHandle {
    state: Mutex<RunState>,
    live: broadcast::Sender<Arc<Event>>,
    killer: Mutex<Option<Killer>>,
    cancel_requested: AtomicBool,
}

struct RunState {
    summary: RunSummary,
    structure: Structure,
    ring: VecDeque<Arc<Event>>,
    next_seq: u64,
}

impl RunState {
    /// The summary with the latest derived structure folded in.
    fn snapshot(&self) -> RunSummary {
        let mut s = self.summary.clone();
        s.steps = self.structure.steps.clone();
        s.metrics = self.structure.metrics.clone();
        s.expect_silence_ms = self.structure.expect_silence_ms;
        s.last_activity_at = [s.last_output_at, self.structure.last_activity_at]
            .into_iter()
            .flatten()
            .max();
        s
    }
}

/// What a new subscriber gets: events still in memory, then the live feed.
pub struct Subscription {
    /// Events with seq > requested `since_seq` that are still in the ring.
    pub backlog: Vec<Arc<Event>>,
    /// Lowest seq held in memory. Events in `(since_seq, ring_start)` must be read
    /// from the store.
    pub ring_start: u64,
    pub live: broadcast::Receiver<Arc<Event>>,
    pub finished: bool,
}

impl Agent {
    pub fn new(store: Store, hub_url: String) -> Self {
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        let (fleet, _) = broadcast::channel(1024);
        Agent {
            inner: Arc::new(Inner {
                host,
                hub_url,
                store,
                runs: Mutex::new(HashMap::new()),
                fleet,
            }),
        }
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// Fleet-wide summary updates (lifecycle changes plus periodic refreshes).
    pub fn fleet(&self) -> broadcast::Receiver<RunSummary> {
        self.inner.fleet.subscribe()
    }

    fn handle(&self, id: &str) -> Option<Arc<RunHandle>> {
        self.inner.runs.lock().unwrap().get(id).cloned()
    }

    /// Live summary for a run held in memory (active or recently finished).
    pub fn summary(&self, id: &str) -> Option<RunSummary> {
        let h = self.handle(id)?;
        let s = h.state.lock().unwrap().snapshot();
        Some(s)
    }

    pub fn active_summaries(&self) -> Vec<RunSummary> {
        let handles: Vec<Arc<RunHandle>> =
            self.inner.runs.lock().unwrap().values().cloned().collect();
        handles
            .iter()
            .map(|h| h.state.lock().unwrap().snapshot())
            .collect()
    }

    pub fn subscribe(&self, id: &str, since_seq: u64) -> Option<Subscription> {
        let h = self.handle(id)?;
        // Holding the state lock while subscribing guarantees no event slips between
        // the backlog snapshot and the live receiver (emit also holds this lock).
        let st = h.state.lock().unwrap();
        let live = h.live.subscribe();
        let ring_start = st.ring.front().map(|e| e.seq).unwrap_or(st.next_seq);
        let backlog = st
            .ring
            .iter()
            .filter(|e| e.seq > since_seq)
            .cloned()
            .collect();
        let finished = st.summary.lifecycle.is_terminal();
        Some(Subscription {
            backlog,
            ring_start,
            live,
            finished,
        })
    }

    /// Start a run. Returns once the run is registered; the process starts async.
    pub fn start(&self, req: CreateRun) -> Result<RunSummary> {
        if req.cmd.is_empty() {
            anyhow::bail!("empty command");
        }
        let cwd = PathBuf::from(&req.cwd);
        if !cwd.is_dir() {
            anyhow::bail!("working directory does not exist: {}", req.cwd);
        }
        let resolved = project::resolve(req.project.as_deref(), &cwd);
        let name = req
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| naming::derive_name(&req.cmd));
        let id = ulid::Ulid::new().to_string();
        let summary = RunSummary {
            id: id.clone(),
            project: resolved.name,
            name,
            host: self.inner.host.clone(),
            cmd: req.cmd.clone(),
            cwd: req.cwd.clone(),
            lifecycle: Lifecycle::Starting,
            health: Health::Ok,
            created_at: now_ms(),
            config_root: resolved.root.map(|r| r.to_string_lossy().into_owned()),
            pty: req.pty,
            ..Default::default()
        };
        let (live, _) = broadcast::channel(RING_CAP);
        let handle = Arc::new(RunHandle {
            state: Mutex::new(RunState {
                summary: summary.clone(),
                structure: Structure::default(),
                ring: VecDeque::new(),
                next_seq: 1,
            }),
            live,
            killer: Mutex::new(None),
            cancel_requested: AtomicBool::new(false),
        });
        self.inner
            .runs
            .lock()
            .unwrap()
            .insert(id.clone(), handle.clone());
        self.publish(&summary);

        let agent = self.clone();
        tokio::spawn(async move {
            agent.drive(handle, req).await;
        });
        Ok(summary)
    }

    /// Request cancellation. Returns false if the run is unknown or already finished.
    pub fn cancel(&self, id: &str, force: bool) -> bool {
        let Some(h) = self.handle(id) else {
            return false;
        };
        if h.state.lock().unwrap().summary.lifecycle.is_terminal() {
            return false;
        }
        h.cancel_requested.store(true, Ordering::SeqCst);
        let killer = h.killer.lock().unwrap().clone();
        if let Some(k) = killer {
            if force {
                k.kill();
            } else {
                k.terminate();
                let h2 = h.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(CANCEL_GRACE).await;
                    if !h2.state.lock().unwrap().summary.lifecycle.is_terminal() {
                        k.kill();
                    }
                });
            }
        }
        true
    }

    fn publish(&self, summary: &RunSummary) {
        if let Err(e) = self.inner.store.upsert_run(summary) {
            tracing::error!(run = %summary.id, error = %e, "failed to persist run summary");
        }
        let _ = self.inner.fleet.send(summary.clone());
    }

    fn publish_snapshot(&self, h: &RunHandle) -> RunSummary {
        let s = h.state.lock().unwrap().snapshot();
        self.publish(&s);
        s
    }

    /// Append an event: sequence it, ring it, broadcast it, persist it.
    fn emit(&self, h: &RunHandle, kind: EventKind) -> u64 {
        let mut st = h.state.lock().unwrap();
        let seq = st.next_seq;
        st.next_seq += 1;
        let ts = now_ms();
        let is_output = matches!(kind, EventKind::Output { .. });
        let ev = Arc::new(Event {
            run_id: st.summary.id.clone(),
            seq,
            ts,
            kind,
        });
        st.summary.last_seq = seq;
        if is_output {
            st.summary.last_output_at = Some(ts);
        }
        if st.ring.len() == RING_CAP {
            st.ring.pop_front();
        }
        st.ring.push_back(ev.clone());
        let _ = h.live.send(ev.clone());
        if let Err(e) = self
            .inner
            .store
            .append_events(&st.summary.project, vec![(*ev).clone()])
        {
            tracing::error!(error = %e, "failed to persist event");
        }
        seq
    }

    /// Apply a directive to the run's structure and emit resulting events.
    fn apply(&self, h: &RunHandle, d: Directive) {
        let kinds = h.state.lock().unwrap().structure.apply(d, now_ms());
        for k in kinds {
            self.emit(h, k);
        }
    }

    fn update_summary(&self, h: &RunHandle, f: impl FnOnce(&mut RunSummary)) -> RunSummary {
        {
            let mut st = h.state.lock().unwrap();
            f(&mut st.summary);
        }
        self.publish_snapshot(h)
    }

    async fn drive(&self, h: Arc<RunHandle>, req: CreateRun) {
        let (run_id, project) = {
            let st = h.state.lock().unwrap();
            (st.summary.id.clone(), st.summary.project.clone())
        };

        // Side channel first, so its endpoint can go into the child's environment.
        let (side_tx, mut side_rx) = mpsc::channel::<SideMsg>(1024);
        let side = match sidechannel::open(&run_id, side_tx) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(run = %run_id, error = %e, "could not open the TRUN_EVENTS side channel");
                None
            }
        };

        let mut env: Vec<(String, String)> = vec![
            ("TRUN_RUN_ID".into(), run_id.clone()),
            ("TRUN_PROJECT".into(), project),
            ("TRUN_HUB".into(), self.inner.hub_url.clone()),
        ];
        if let Some(s) = &side {
            env.push(("TRUN_EVENTS".into(), s.endpoint.clone()));
        }
        // Unbuffered Python output is the single most common "why is it silent?" fix.
        if std::env::var_os("PYTHONUNBUFFERED").is_none()
            && !req.env.iter().any(|(k, _)| k == "PYTHONUNBUFFERED")
        {
            env.push(("PYTHONUNBUFFERED".into(), "1".into()));
        }
        env.extend(req.env.iter().cloned());
        let spec = SpawnSpec {
            cmd: req.cmd.clone(),
            cwd: PathBuf::from(&req.cwd),
            env,
            pty: req.pty,
        };

        let started = std::time::Instant::now();
        let proc = match trun_supervise::start(&spec) {
            Ok(p) => p,
            Err(e) => {
                let exit = ExitInfo {
                    spawn_error: Some(e.to_string()),
                    ..Default::default()
                };
                self.finish(
                    &h,
                    Lifecycle::Failed,
                    ExitStatusInfo::default(),
                    Diagnoser::default().diagnose(&exit),
                    None,
                );
                return;
            }
        };
        let pid = proc.pid;
        *h.killer.lock().unwrap() = Some(proc.killer.clone());
        self.emit(
            &h,
            EventKind::Lifecycle {
                state: Lifecycle::Running,
                exit_code: None,
                signal: None,
                pid: Some(pid),
                reason: None,
            },
        );
        self.update_summary(&h, |s| {
            s.lifecycle = Lifecycle::Running;
            s.started_at = Some(now_ms());
            s.pid = Some(pid);
        });
        tracing::info!(run = %run_id, pid, cmd = ?req.cmd, pty = req.pty, "run started");

        let (line_tx, mut line_rx) = mpsc::channel(1024);
        let mut pumps = vec![tokio::spawn(trun_supervise::pump(
            proc.stdout,
            Stream::Stdout,
            line_tx.clone(),
        ))];
        if let Some(stderr) = proc.stderr {
            pumps.push(tokio::spawn(trun_supervise::pump(
                stderr,
                Stream::Stderr,
                line_tx,
            )));
        } else {
            drop(line_tx);
        }
        let mut exit_rx = proc.exit;

        let mut parsers = ParserSet::new();
        let mut diagnoser = Diagnoser::default();
        let mut status: Option<ExitStatusInfo> = None;
        let mut drain_deadline: Option<tokio::time::Instant> = None;
        let mut publish_tick = tokio::time::interval(SUMMARY_PUBLISH_EVERY);
        let mut progress_tick = tokio::time::interval(PROGRESS_TICK);
        let mut published_seq = 0;
        let mut lines_open = true;
        let mut side_open = side.is_some();
        let mut invalid_side = 0u32;

        loop {
            tokio::select! {
                line = line_rx.recv(), if lines_open => match line {
                    Some(l) => {
                        let feed = parsers.feed(&l.text, l.cr);
                        if !feed.hide {
                            let clean = trun_parse::strip_ansi(&l.text).into_owned();
                            let text = trun_parse::sanitize_display(&l.text).into_owned();
                            let seq = self.emit(&h, EventKind::Output { stream: l.stream, text, cr: l.cr });
                            if !l.cr {
                                diagnoser.push(seq, &clean);
                            }
                        }
                        for d in feed.directives {
                            self.apply(&h, d);
                        }
                    }
                    None => {
                        lines_open = false;
                        if status.is_some() { break; }
                    }
                },
                msg = side_rx.recv(), if side_open => match msg {
                    Some(SideMsg::Directive(d)) => {
                        parsers.note_external(&d);
                        self.apply(&h, d);
                    }
                    Some(SideMsg::Invalid(why)) => {
                        // Report the first few, then stay quiet: a broken emitter
                        // must not flood the log.
                        invalid_side += 1;
                        if invalid_side <= 5 {
                            self.emit(&h, EventKind::Log { level: LogLevel::Warn, text: format!("trun: bad TRUN_EVENTS message: {why}") });
                        }
                    }
                    None => side_open = false,
                },
                r = &mut exit_rx, if status.is_none() => {
                    let st = match r {
                        Ok(Ok(st)) => st,
                        Ok(Err(e)) => {
                            tracing::error!(run = %run_id, error = %e, "wait failed");
                            ExitStatusInfo::default()
                        }
                        Err(_) => ExitStatusInfo::default(),
                    };
                    status = Some(st);
                    if !lines_open { break; }
                    drain_deadline = Some(tokio::time::Instant::now() + PIPE_DRAIN_GRACE);
                }
                _ = async { tokio::time::sleep_until(drain_deadline.unwrap()).await }, if drain_deadline.is_some() => {
                    tracing::debug!(run = %run_id, "output still open after exit (leftover child processes); stop reading");
                    break;
                }
                _ = progress_tick.tick() => {
                    let kinds = h.state.lock().unwrap().structure.flush_progress(now_ms());
                    for k in kinds {
                        self.emit(&h, k);
                    }
                }
                _ = publish_tick.tick() => {
                    let (seq, activity) = {
                        let st = h.state.lock().unwrap();
                        (st.summary.last_seq, st.structure.last_activity_at)
                    };
                    let marker = seq.wrapping_add(activity.unwrap_or(0) as u64);
                    if marker != published_seq {
                        published_seq = marker;
                        self.publish_snapshot(&h);
                    }
                }
            }
        }
        for p in pumps {
            p.abort();
        }
        // Late side-channel messages (e.g. a final metric) that already arrived.
        while let Ok(SideMsg::Directive(d)) = side_rx.try_recv() {
            self.apply(&h, d);
        }
        drop(side);

        let status = status.unwrap_or_default();
        let cancelled = h.cancel_requested.load(Ordering::SeqCst);
        let lifecycle = if cancelled {
            Lifecycle::Cancelled
        } else if status.success() {
            Lifecycle::Succeeded
        } else {
            Lifecycle::Failed
        };
        let exit = ExitInfo {
            exit_code: status.code,
            signal: status.signal,
            spawn_error: None,
            cancelled,
            elapsed_ms: started.elapsed().as_millis() as i64,
        };
        let diagnosis = diagnoser.diagnose(&exit);
        let reason = cancelled.then(|| "cancelled by request".to_string());
        self.finish(&h, lifecycle, status, diagnosis, reason);
    }

    fn finish(
        &self,
        h: &Arc<RunHandle>,
        lifecycle: Lifecycle,
        status: ExitStatusInfo,
        diagnosis: Option<trun_proto::Diagnosis>,
        reason: Option<String>,
    ) {
        let closing = h
            .state
            .lock()
            .unwrap()
            .structure
            .finalize(lifecycle, now_ms());
        for k in closing {
            self.emit(h, k);
        }
        if let Some(d) = &diagnosis {
            self.emit(h, EventKind::Diagnosis(d.clone()));
        }
        self.emit(
            h,
            EventKind::Lifecycle {
                state: lifecycle,
                exit_code: status.code,
                signal: status.signal,
                pid: None,
                reason,
            },
        );
        let s = self.update_summary(h, |s| {
            s.lifecycle = lifecycle;
            s.ended_at = Some(now_ms());
            s.exit_code = status.code;
            s.signal = status.signal;
            s.diagnosis = diagnosis;
        });
        tracing::info!(run = %s.id, lifecycle = s.lifecycle.as_str(), code = ?s.exit_code, "run finished");

        let agent = self.clone();
        let id = s.id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(RETAIN_FINISHED).await;
            agent.inner.runs.lock().unwrap().remove(&id);
        });
    }
}
