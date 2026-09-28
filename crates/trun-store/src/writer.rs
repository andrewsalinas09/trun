//! One writer thread per database file, committing in small batches.

use rusqlite::{Connection, params};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use trun_proto::{Event, EventKind, RunSummary, now_ms};

/// Max time an op waits before its batch commits.
const BATCH_WINDOW: Duration = Duration::from_millis(50);
const BATCH_MAX_OPS: usize = 1000;

pub enum Op {
    /// Project file: authoritative run summary (forwarded to the hub index after commit).
    UpsertRun(Box<RunSummary>),
    /// Project file: append events.
    Events(Vec<Event>),
    /// Hub file: update the run index.
    IndexRun(Box<RunSummary>),
    /// Hub file: record that a project database exists.
    RegisterProject { name: String, file: String },
    /// Reply once everything before this op is committed.
    Flush(Sender<()>),
}

#[derive(Clone)]
pub struct WriterHandle {
    tx: Sender<Op>,
}

impl WriterHandle {
    pub fn send(&self, op: Op) {
        if self.tx.send(op).is_err() {
            tracing::error!("store writer thread is gone; dropping write");
        }
    }

    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        self.send(Op::Flush(tx));
        let _ = rx.recv_timeout(Duration::from_secs(30));
    }
}

/// `forward_to`: for project writers, the hub writer that receives index updates.
pub fn spawn(label: String, conn: Connection, forward_to: Option<WriterHandle>) -> WriterHandle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("store-{label}"))
        .spawn(move || run(label, conn, rx, forward_to))
        .expect("spawn store writer thread");
    WriterHandle { tx }
}

fn run(label: String, mut conn: Connection, rx: Receiver<Op>, forward_to: Option<WriterHandle>) {
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        let deadline = Instant::now() + BATCH_WINDOW;
        while batch.len() < BATCH_MAX_OPS {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(op) => batch.push(op),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let mut flushes = Vec::new();
        let mut indexed: Vec<RunSummary> = Vec::new();
        if let Err(e) = apply(&mut conn, batch, &mut flushes, &mut indexed) {
            tracing::error!(store = %label, error = %e, "store batch failed");
        }
        if let Some(hub) = &forward_to {
            // Only the latest summary per run matters.
            let mut seen = std::collections::HashSet::new();
            for r in indexed.into_iter().rev() {
                if seen.insert(r.id.clone()) {
                    hub.send(Op::IndexRun(Box::new(r)));
                }
            }
            // Flush semantics: project flushes also wait for the hub to catch up.
            for f in flushes {
                let hub = hub.clone();
                std::thread::spawn(move || {
                    hub.flush();
                    let _ = f.send(());
                });
            }
        } else {
            for f in flushes {
                let _ = f.send(());
            }
        }
    }
}

fn apply(
    conn: &mut Connection,
    batch: Vec<Op>,
    flushes: &mut Vec<Sender<()>>,
    indexed: &mut Vec<RunSummary>,
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        for op in batch {
            match op {
                Op::Events(events) => {
                    // Only project files have these tables, so prepare here (cached per connection).
                    let mut ins_out = tx.prepare_cached(
                        "INSERT OR REPLACE INTO output (run_id, seq, ts, stream, cr, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    )?;
                    let mut ins_evt = tx.prepare_cached(
                        "INSERT OR REPLACE INTO events (run_id, seq, ts, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
                    )?;
                    for e in events {
                        match &e.kind {
                            EventKind::Output { stream, text, cr } => {
                                ins_out.execute(params![
                                    e.run_id,
                                    e.seq as i64,
                                    e.ts,
                                    stream.as_i64(),
                                    *cr as i64,
                                    text
                                ])?;
                            }
                            other => {
                                let payload = serde_json::to_string(other).unwrap_or_default();
                                ins_evt.execute(params![
                                    e.run_id,
                                    e.seq as i64,
                                    e.ts,
                                    other.name(),
                                    payload
                                ])?;
                            }
                        }
                    }
                }
                Op::UpsertRun(run) => {
                    let json = serde_json::to_string(&*run).unwrap_or_default();
                    tx.execute(
                        "INSERT INTO runs (id, summary) VALUES (?1, ?2)
                         ON CONFLICT(id) DO UPDATE SET summary = excluded.summary",
                        params![run.id, json],
                    )?;
                    indexed.push(*run);
                }
                Op::IndexRun(run) => {
                    let json = serde_json::to_string(&*run).unwrap_or_default();
                    tx.execute(
                        "INSERT INTO runs (id, project, name, host, lifecycle, health, created_at, ended_at, summary)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                         ON CONFLICT(id) DO UPDATE SET
                           project = excluded.project, name = excluded.name, host = excluded.host,
                           lifecycle = excluded.lifecycle, health = excluded.health,
                           ended_at = excluded.ended_at, summary = excluded.summary",
                        params![
                            run.id,
                            run.project,
                            run.name,
                            run.host,
                            run.lifecycle.as_str(),
                            run.health.as_str(),
                            run.created_at,
                            run.ended_at,
                            json
                        ],
                    )?;
                }
                Op::RegisterProject { name, file } => {
                    tx.execute(
                        "INSERT INTO projects (name, file, created_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(name) DO UPDATE SET file = excluded.file",
                        params![name, file, now_ms()],
                    )?;
                }
                Op::Flush(reply) => flushes.push(reply),
            }
        }
    }
    tx.commit()
}
