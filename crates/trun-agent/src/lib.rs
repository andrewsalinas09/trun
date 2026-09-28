//! The agent owns the runs on one host (docs/02-architecture.md).
//!
//! In M1 the agent is embedded in the hub process; the event flow is already the
//! one remote agents will use: every event gets a per-run `seq`, is kept in a
//! bounded in-memory ring for live subscribers, broadcast, and appended to the store.

mod naming;
pub mod project;

use anyhow::Result;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, oneshot};
use trun_parse::{Diagnoser, ExitInfo};
use trun_proto::{CreateRun, Event, EventKind, Health, Lifecycle, RunSummary, Stream, now_ms};
use trun_store::Store;
use trun_supervise::{ExitStatusInfo, Killer, SpawnSpec};

/// Events kept in memory per active run, for subscribers joining mid-stream.
const RING_CAP: usize = 4096;
/// After the main process exits, how long to keep reading pipes held open by
/// leftover children before giving up on them.
const PIPE_DRAIN_GRACE: Duration = Duration::from_secs(2);
/// Graceful cancel → hard kill after this long.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
/// Finished runs stay in memory this long so late subscribers get a seamless replay.
const RETAIN_FINISHED: Duration = Duration::from_secs(60);
/// How often an active run republishes its summary (last output time, seq).
const SUMMARY_PUBLISH_EVERY: Duration = Duration::from_secs(2);

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
    ring: VecDeque<Arc<Event>>,
    next_seq: u64,
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

    /// Live summary for a run held in memory (active or recently finished).
    pub fn summary(&self, id: &str) -> Option<RunSummary> {
        let h = self.inner.runs.lock().unwrap().get(id).cloned()?;
        let s = h.state.lock().unwrap().summary.clone();
        Some(s)
    }

    pub fn active_summaries(&self) -> Vec<RunSummary> {
        let handles: Vec<Arc<RunHandle>> =
            self.inner.runs.lock().unwrap().values().cloned().collect();
        handles
            .iter()
            .map(|h| h.state.lock().unwrap().summary.clone())
            .collect()
    }

    pub fn subscribe(&self, id: &str, since_seq: u64) -> Option<Subscription> {
        let h = self.inner.runs.lock().unwrap().get(id).cloned()?;
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
        let project = project::resolve_project(req.project.as_deref(), &cwd);
        let name = req
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| naming::derive_name(&req.cmd));
        let id = ulid::Ulid::new().to_string();
        let summary = RunSummary {
            id: id.clone(),
            project,
            name,
            host: self.inner.host.clone(),
            cmd: req.cmd.clone(),
            cwd: req.cwd.clone(),
            lifecycle: Lifecycle::Starting,
            health: Health::Ok,
            created_at: now_ms(),
            started_at: None,
            ended_at: None,
            exit_code: None,
            signal: None,
            pid: None,
            diagnosis: None,
            last_seq: 0,
            last_output_at: None,
        };
        let (live, _) = broadcast::channel(RING_CAP);
        let handle = Arc::new(RunHandle {
            state: Mutex::new(RunState {
                summary: summary.clone(),
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
        let Some(h) = self.inner.runs.lock().unwrap().get(id).cloned() else {
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

    fn update_summary(&self, h: &RunHandle, f: impl FnOnce(&mut RunSummary)) -> RunSummary {
        let s = {
            let mut st = h.state.lock().unwrap();
            f(&mut st.summary);
            st.summary.clone()
        };
        self.publish(&s);
        s
    }

    async fn drive(&self, h: Arc<RunHandle>, req: CreateRun) {
        let (run_id, project) = {
            let st = h.state.lock().unwrap();
            (st.summary.id.clone(), st.summary.project.clone())
        };
        let mut env: Vec<(String, String)> = vec![
            ("TRUN_RUN_ID".into(), run_id.clone()),
            ("TRUN_PROJECT".into(), project),
            ("TRUN_HUB".into(), self.inner.hub_url.clone()),
        ];
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
        };

        let started = std::time::Instant::now();
        let (mut proc, stdout, stderr) = match trun_supervise::spawn(&spec) {
            Ok(x) => x,
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
        let pid = proc.pid();
        *h.killer.lock().unwrap() = Some(proc.killer());
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
        tracing::info!(run = %run_id, pid, cmd = ?req.cmd, "run started");

        let (line_tx, mut line_rx) = mpsc::channel(1024);
        let p_out = tokio::spawn(trun_supervise::pump(
            stdout,
            Stream::Stdout,
            line_tx.clone(),
        ));
        let p_err = tokio::spawn(trun_supervise::pump(stderr, Stream::Stderr, line_tx));

        let (status_tx, mut status_rx) = oneshot::channel();
        tokio::spawn(async move {
            let _ = status_tx.send(proc.wait().await);
        });

        let mut diagnoser = Diagnoser::default();
        let mut status: Option<ExitStatusInfo> = None;
        let mut drain_deadline: Option<tokio::time::Instant> = None;
        let mut publish_tick = tokio::time::interval(SUMMARY_PUBLISH_EVERY);
        let mut published_seq = 0;
        let mut lines_open = true;

        loop {
            tokio::select! {
                line = line_rx.recv(), if lines_open => match line {
                    Some(l) => {
                        let clean = trun_parse::strip_ansi(&l.text).into_owned();
                        let seq = self.emit(&h, EventKind::Output { stream: l.stream, text: l.text, cr: l.cr });
                        if !l.cr {
                            diagnoser.push(seq, &clean);
                        }
                    }
                    None => {
                        lines_open = false;
                        if status.is_some() { break; }
                    }
                },
                r = &mut status_rx, if status.is_none() => {
                    let st = match r {
                        Ok(Ok(st)) => st,
                        Ok(Err(e)) => {
                            tracing::error!(run = %run_id, error = %e, "wait failed");
                            ExitStatusInfo { code: None, signal: None }
                        }
                        Err(_) => ExitStatusInfo::default(),
                    };
                    status = Some(st);
                    if !lines_open { break; }
                    drain_deadline = Some(tokio::time::Instant::now() + PIPE_DRAIN_GRACE);
                }
                _ = async { tokio::time::sleep_until(drain_deadline.unwrap()).await }, if drain_deadline.is_some() => {
                    tracing::debug!(run = %run_id, "pipes still open after exit (leftover child processes); stop reading");
                    break;
                }
                _ = publish_tick.tick() => {
                    let seq = h.state.lock().unwrap().summary.last_seq;
                    if seq != published_seq {
                        published_seq = seq;
                        let s = h.state.lock().unwrap().summary.clone();
                        self.publish(&s);
                    }
                }
            }
        }
        p_out.abort();
        p_err.abort();

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
