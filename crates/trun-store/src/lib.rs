//! SQLite storage (docs/02-architecture.md, D20).
//!
//! ```text
//! <data_dir>/hub.db               run index + project registry
//! <data_dir>/projects/<name>.db   everything for one project
//! ```
//!
//! Each database file has exactly one writer thread that commits in small batches;
//! reads open their own short-lived read-only connections, so readers never block
//! the writer (WAL mode). The hub index is only updated *after* the project file has
//! committed, so anything visible in `hub.db` is already durable in the project file.

mod schema;
mod writer;

use anyhow::{Context, Result};
use regex::Regex;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use trun_proto::{
    Event, EventKind, Lifecycle, LogLine, MetricPoint, MetricSeries, Num, ProjectInfo, RunSummary,
    Stream, now_ms,
};
use writer::{Op, WriterHandle};

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    data_dir: PathBuf,
    hub: WriterHandle,
    projects: Mutex<HashMap<String, WriterHandle>>,
}

#[derive(Debug, Clone, Default)]
pub struct RunQuery {
    pub active_only: bool,
    pub project: Option<String>,
    pub name_glob: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct LogQuery {
    pub since_seq: u64,
    /// Only lines with `seq < before_seq` (for paging backwards).
    pub before_seq: Option<u64>,
    /// Return only the last N matching lines (instead of the first N after `since_seq`).
    pub tail: Option<u32>,
    pub limit: Option<u32>,
    pub grep: Option<Regex>,
    pub stream: Option<Stream>,
}

impl Store {
    pub fn open(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(data_dir.join("projects"))
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let hub_path = data_dir.join("hub.db");
        let conn = open_rw(&hub_path)?;
        schema::migrate_hub(&conn)?;
        let hub = writer::spawn("hub".into(), conn, None);
        Ok(Store {
            inner: Arc::new(Inner {
                data_dir,
                hub,
                projects: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    fn project_path(&self, project: &str) -> PathBuf {
        self.inner
            .data_dir
            .join("projects")
            .join(format!("{}.db", file_stem(project)))
    }

    fn project_writer(&self, project: &str) -> Result<WriterHandle> {
        let mut map = self.inner.projects.lock().unwrap();
        if let Some(w) = map.get(project) {
            return Ok(w.clone());
        }
        let path = self.project_path(project);
        let conn = open_rw(&path)?;
        schema::migrate_project(&conn)?;
        let w = writer::spawn(project.to_string(), conn, Some(self.inner.hub.clone()));
        self.inner.hub.send(Op::RegisterProject {
            name: project.to_string(),
            file: path_string(&path),
        });
        map.insert(project.to_string(), w.clone());
        Ok(w)
    }

    /// Insert or update a run. Written to the project file, then to the hub index.
    pub fn upsert_run(&self, run: &RunSummary) -> Result<()> {
        self.project_writer(&run.project)?
            .send(Op::UpsertRun(Box::new(run.clone())));
        Ok(())
    }

    pub fn append_events(&self, project: &str, events: Vec<Event>) -> Result<()> {
        if !events.is_empty() {
            self.project_writer(project)?.send(Op::Events(events));
        }
        Ok(())
    }

    /// Wait until everything sent so far is committed (project files, then hub).
    pub fn flush(&self) {
        let writers: Vec<WriterHandle> = self
            .inner
            .projects
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect();
        for w in writers {
            w.flush();
        }
        self.inner.hub.flush();
    }

    // ---- reads (blocking; call from spawn_blocking in async code) ----

    fn hub_ro(&self) -> Result<Connection> {
        open_ro(&self.inner.data_dir.join("hub.db"))
    }

    fn project_ro(&self, project: &str) -> Result<Option<Connection>> {
        let path = self.project_path(project);
        if !path.exists() {
            return Ok(None);
        }
        open_ro(&path).map(Some)
    }

    pub fn list_runs(&self, q: &RunQuery) -> Result<Vec<RunSummary>> {
        let conn = self.hub_ro()?;
        let mut sql = String::from("SELECT summary FROM runs WHERE 1=1");
        let mut args: Vec<String> = vec![];
        if q.active_only {
            sql.push_str(" AND lifecycle IN ('queued','starting','running')");
        }
        if let Some(p) = &q.project {
            sql.push_str(" AND project = ?");
            args.push(p.clone());
        }
        if let Some(g) = &q.name_glob {
            sql.push_str(" AND name GLOB ?");
            args.push(g.clone());
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ?");
        args.push(q.limit.unwrap_or(200).to_string());
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            r.get::<_, String>(0)
        })?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn get_run(&self, id: &str) -> Result<Option<RunSummary>> {
        let conn = self.hub_ro()?;
        let s: Option<String> = conn
            .query_row("SELECT summary FROM runs WHERE id = ?", [id], |r| r.get(0))
            .optional()?;
        s.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    /// Resolve a user-supplied run reference: exact id, unique id prefix, or name
    /// (most recent run with that name).
    pub fn resolve_run(&self, reference: &str) -> Result<Option<RunSummary>> {
        if let Some(r) = self.get_run(reference)? {
            return Ok(Some(r));
        }
        let conn = self.hub_ro()?;
        let upper = reference.to_ascii_uppercase();
        let mut stmt = conn.prepare("SELECT summary FROM runs WHERE id LIKE ? || '%' LIMIT 2")?;
        let hits: Vec<String> = stmt
            .query_map([&upper], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if hits.len() == 1 {
            return Ok(Some(serde_json::from_str(&hits[0])?));
        }
        let by_name: Option<String> = conn
            .query_row(
                "SELECT summary FROM runs WHERE name = ? ORDER BY created_at DESC LIMIT 1",
                [reference],
                |r| r.get(0),
            )
            .optional()?;
        by_name
            .map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    pub fn logs(&self, project: &str, run_id: &str, q: &LogQuery) -> Result<Vec<LogLine>> {
        let Some(conn) = self.project_ro(project)? else {
            return Ok(vec![]);
        };
        let before = q.before_seq.map(|b| b as i64).unwrap_or(i64::MAX);
        let stream = q.stream.map(|s| s.as_i64()).unwrap_or(0);
        let map_row = |r: &rusqlite::Row<'_>| {
            Ok(LogLine {
                seq: r.get::<_, i64>(0)? as u64,
                ts: r.get(1)?,
                stream: Stream::from_i64(r.get(2)?),
                cr: r.get::<_, i64>(3)? != 0,
                text: r.get(4)?,
            })
        };
        const WHERE: &str = "run_id = ?1 AND seq > ?2 AND seq < ?3 AND (?4 = 0 OR stream = ?4)";

        // Fast path: last N lines without a regex is a reverse index scan.
        if let (Some(n), None) = (q.tail, &q.grep) {
            let mut stmt = conn.prepare(&format!(
                "SELECT seq, ts, stream, cr, text FROM output WHERE {WHERE} ORDER BY seq DESC LIMIT ?5"
            ))?;
            let mut lines: Vec<LogLine> = stmt
                .query_map(
                    params![run_id, q.since_seq as i64, before, stream, n as i64],
                    map_row,
                )?
                .collect::<Result<_, _>>()?;
            lines.reverse();
            return Ok(lines);
        }

        let mut stmt = conn.prepare(&format!(
            "SELECT seq, ts, stream, cr, text FROM output WHERE {WHERE} ORDER BY seq ASC"
        ))?;
        let rows = stmt.query_map(params![run_id, q.since_seq as i64, before, stream], map_row)?;
        let keep = |l: &LogLine| {
            q.stream.is_none_or(|s| s == l.stream)
                && q.grep.as_ref().is_none_or(|re| re.is_match(&l.text))
        };
        if let Some(n) = q.tail {
            let mut ring = std::collections::VecDeque::with_capacity(n as usize);
            for l in rows {
                let l = l?;
                if keep(&l) {
                    if ring.len() == n as usize {
                        ring.pop_front();
                    }
                    ring.push_back(l);
                }
            }
            return Ok(ring.into_iter().collect());
        }
        let limit = q.limit.unwrap_or(1000) as usize;
        let mut out = Vec::new();
        for l in rows {
            let l = l?;
            if keep(&l) {
                out.push(l);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// All events (output and others) with `seq` in `(since_seq, until_seq)`, in order.
    pub fn events(
        &self,
        project: &str,
        run_id: &str,
        since_seq: u64,
        until_seq: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Vec<Event>> {
        let Some(conn) = self.project_ro(project)? else {
            return Ok(vec![]);
        };
        let until = until_seq.map(|u| u as i64).unwrap_or(i64::MAX);
        let mut stmt = conn.prepare(
            "SELECT seq, ts, 'output' AS kind, stream, cr, text, NULL FROM output
               WHERE run_id = ?1 AND seq > ?2 AND seq < ?3
             UNION ALL
             SELECT seq, ts, kind, NULL, NULL, NULL, payload FROM events
               WHERE run_id = ?1 AND seq > ?2 AND seq < ?3
             ORDER BY seq ASC LIMIT ?4",
        )?;
        let limit = limit.map(i64::from).unwrap_or(-1);
        let rows = stmt.query_map(params![run_id, since_seq as i64, until, limit], |r| {
            let seq = r.get::<_, i64>(0)? as u64;
            let ts: i64 = r.get(1)?;
            let kind: String = r.get(2)?;
            let ek = if kind == "output" {
                Some(EventKind::Output {
                    stream: Stream::from_i64(r.get(3)?),
                    cr: r.get::<_, i64>(4)? != 0,
                    text: r.get(5)?,
                })
            } else {
                let payload: String = r.get(6)?;
                serde_json::from_str::<EventKind>(&payload).ok()
            };
            Ok(ek.map(|kind| Event {
                run_id: run_id.to_string(),
                seq,
                ts,
                kind,
            }))
        })?;
        let mut out = Vec::new();
        for r in rows {
            if let Some(e) = r? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// The most recent `limit` events of the given kinds (e.g. `log`, `note`), oldest first.
    pub fn events_of_kind(
        &self,
        project: &str,
        run_id: &str,
        kinds: &[&str],
        limit: u32,
    ) -> Result<Vec<Event>> {
        let Some(conn) = self.project_ro(project)? else {
            return Ok(vec![]);
        };
        let placeholders = vec!["?"; kinds.len()].join(",");
        let sql = format!(
            "SELECT seq, ts, payload FROM events WHERE run_id = ? AND kind IN ({placeholders}) ORDER BY seq DESC LIMIT ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut args: Vec<rusqlite::types::Value> = vec![run_id.to_string().into()];
        args.extend(
            kinds
                .iter()
                .map(|k| rusqlite::types::Value::from(k.to_string())),
        );
        args.push((limit as i64).into());
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
            Ok((
                r.get::<_, i64>(0)? as u64,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out: Vec<Event> = Vec::new();
        for row in rows {
            let (seq, ts, payload) = row?;
            if let Ok(kind) = serde_json::from_str::<EventKind>(&payload) {
                out.push(Event {
                    run_id: run_id.to_string(),
                    seq,
                    ts,
                    kind,
                });
            }
        }
        out.reverse();
        Ok(out)
    }

    /// Metric series for a run. Series longer than `max_points` are downsampled
    /// per bucket to its min and max point (spikes survive); NaN/inf points are
    /// always kept.
    pub fn metric_series(
        &self,
        project: &str,
        run_id: &str,
        names: &[String],
        max_points: usize,
    ) -> Result<Vec<MetricSeries>> {
        let Some(conn) = self.project_ro(project)? else {
            return Ok(vec![]);
        };
        let mut stmt = conn.prepare_cached(
            "SELECT step, ts, value, special FROM metrics WHERE run_id = ?1 AND name = ?2 ORDER BY seq ASC",
        )?;
        let mut out = Vec::new();
        for name in names {
            let points: Vec<MetricPoint> = stmt
                .query_map(params![run_id, name], |r| {
                    Ok(MetricPoint {
                        step: r.get(0)?,
                        ts: r.get(1)?,
                        value: Num(decode_num(r.get(2)?, r.get(3)?)),
                    })
                })?
                .collect::<Result<_, _>>()?;
            let total = points.len() as u64;
            let (points, downsampled) = downsample(points, max_points.max(4));
            out.push(MetricSeries {
                name: name.clone(),
                points,
                total,
                downsampled,
            });
        }
        Ok(out)
    }

    pub fn projects(&self) -> Result<Vec<ProjectInfo>> {
        let conn = self.hub_ro()?;
        let mut stmt = conn.prepare(
            "SELECT p.name,
                    COUNT(r.id),
                    SUM(CASE WHEN r.lifecycle IN ('queued','starting','running') THEN 1 ELSE 0 END),
                    MAX(r.created_at)
             FROM projects p LEFT JOIN runs r ON r.project = p.name
             GROUP BY p.name ORDER BY MAX(r.created_at) DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ProjectInfo {
                name: r.get(0)?,
                run_count: r.get::<_, i64>(1)? as u64,
                active_count: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                last_run_at: r.get(3)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Called at hub start-up: runs that were active when the previous hub died can no
    /// longer be supervised. Mark them `lost` and return the updated summaries.
    pub fn mark_orphans_lost(&self) -> Result<Vec<RunSummary>> {
        let orphans = self.list_runs(&RunQuery {
            active_only: true,
            limit: Some(100_000),
            ..Default::default()
        })?;
        let now = now_ms();
        let mut out = Vec::new();
        // Mirrored remote runs are owned by their host's daemon, not this hub; they
        // resync when the host link reconnects.
        for mut r in orphans.into_iter().filter(|r| r.via.is_none()) {
            r.lifecycle = Lifecycle::Lost;
            r.ended_at = Some(now);
            self.upsert_run(&r)?;
            out.push(r);
        }
        self.flush();
        Ok(out)
    }

    /// Highest event sequence actually stored for a run (0 if none). Replication
    /// resumes from here; a summary's `last_seq` may be ahead of stored events.
    pub fn max_event_seq(&self, project: &str, run_id: &str) -> Result<u64> {
        let Some(conn) = self.project_ro(project)? else {
            return Ok(0);
        };
        let max: Option<i64> = conn.query_row(
            "SELECT MAX(m) FROM (SELECT MAX(seq) AS m FROM output WHERE run_id = ?1
                                 UNION ALL SELECT MAX(seq) FROM events WHERE run_id = ?1)",
            [run_id],
            |r| r.get(0),
        )?;
        Ok(max.unwrap_or(0) as u64)
    }

    // ---- remote hosts (hub.db) ----

    pub fn hosts(&self) -> Result<Vec<trun_proto::HostConfig>> {
        let conn = self.hub_ro()?;
        let mut stmt = conn.prepare("SELECT config FROM hosts ORDER BY name")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    /// Insert or replace a host (synchronous: config changes are rare).
    pub fn put_host(&self, h: &trun_proto::HostConfig) -> Result<()> {
        let conn = open_rw(&self.inner.data_dir.join("hub.db"))?;
        conn.execute(
            "INSERT INTO hosts (name, config, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET config = excluded.config",
            params![h.name, serde_json::to_string(h)?, now_ms()],
        )?;
        Ok(())
    }

    pub fn delete_host(&self, name: &str) -> Result<bool> {
        let conn = open_rw(&self.inner.data_dir.join("hub.db"))?;
        Ok(conn.execute("DELETE FROM hosts WHERE name = ?1", [name])? > 0)
    }
}

/// Project name → safe file stem. Distinct names stay distinct: anything outside
/// `[A-Za-z0-9._-]` is hex-escaped.
pub fn file_stem(project: &str) -> String {
    let mut s = String::new();
    for c in project.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            s.push(c);
        } else {
            s.push_str(&format!("%{:X}", c as u32));
        }
    }
    if s.is_empty() || s.starts_with('.') {
        format!("_{s}")
    } else {
        s
    }
}

/// f64 → (REAL column, special code). SQLite cannot store NaN.
pub(crate) fn encode_num(v: f64) -> (Option<f64>, i64) {
    if v.is_nan() {
        (None, 1)
    } else if v == f64::INFINITY {
        (None, 2)
    } else if v == f64::NEG_INFINITY {
        (None, 3)
    } else {
        (Some(v), 0)
    }
}

fn decode_num(value: Option<f64>, special: i64) -> f64 {
    match special {
        1 => f64::NAN,
        2 => f64::INFINITY,
        3 => f64::NEG_INFINITY,
        _ => value.unwrap_or(f64::NAN),
    }
}

/// Min/max-per-bucket downsampling that keeps every non-finite point.
fn downsample(points: Vec<MetricPoint>, max_points: usize) -> (Vec<MetricPoint>, bool) {
    if points.len() <= max_points {
        return (points, false);
    }
    let buckets = (max_points / 2).max(1);
    let size = points.len().div_ceil(buckets);
    let mut out = Vec::with_capacity(max_points + 8);
    for chunk in points.chunks(size) {
        let finite = || {
            chunk
                .iter()
                .enumerate()
                .filter(|(_, p)| p.value.0.is_finite())
        };
        let lo = finite()
            .min_by(|a, b| a.1.value.0.total_cmp(&b.1.value.0))
            .map(|(i, _)| i);
        let hi = finite()
            .max_by(|a, b| a.1.value.0.total_cmp(&b.1.value.0))
            .map(|(i, _)| i);
        let mut keep: Vec<usize> = chunk
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.value.0.is_finite())
            .map(|(i, _)| i)
            .chain(lo)
            .chain(hi)
            .collect();
        keep.sort_unstable();
        keep.dedup();
        out.extend(keep.into_iter().map(|i| chunk[i].clone()));
    }
    (out, true)
}

fn path_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn open_rw(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

fn open_ro(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

#[cfg(test)]
mod tests;
