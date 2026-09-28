//! Shared types for trun: runs, events, and HTTP API payloads.
//!
//! Everything that crosses a process boundary (hub <-> CLI, hub <-> UI, later
//! hub <-> remote agent) is defined here so every side agrees on the shape.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 7317;
pub const ADHOC_PROJECT: &str = "_adhoc";

/// Milliseconds since the Unix epoch.
pub type Millis = i64;

pub fn now_ms() -> Millis {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Run state
// ---------------------------------------------------------------------------

/// Where a run is in its life. Orthogonal to [`Health`] (see docs/decisions.md D8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Queued,
    Starting,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
    Preempted,
}

impl Lifecycle {
    pub fn is_terminal(self) -> bool {
        !matches!(
            self,
            Lifecycle::Queued | Lifecycle::Starting | Lifecycle::Running
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Queued => "queued",
            Lifecycle::Starting => "starting",
            Lifecycle::Running => "running",
            Lifecycle::Succeeded => "succeeded",
            Lifecycle::Failed => "failed",
            Lifecycle::Cancelled => "cancelled",
            Lifecycle::Lost => "lost",
            Lifecycle::Preempted => "preempted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Lifecycle::Queued,
            "starting" => Lifecycle::Starting,
            "running" => Lifecycle::Running,
            "succeeded" => Lifecycle::Succeeded,
            "failed" => Lifecycle::Failed,
            "cancelled" => Lifecycle::Cancelled,
            "lost" => Lifecycle::Lost,
            "preempted" => Lifecycle::Preempted,
            _ => return None,
        })
    }
}

/// Is the run making progress? Only meaningful while running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Health {
    Ok,
    Warn,
    Stalled,
    Failing,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Ok => "ok",
            Health::Warn => "warn",
            Health::Stalled => "stalled",
            Health::Failing => "failing",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => Health::Ok,
            "warn" => Health::Warn,
            "stalled" => Health::Stalled,
            "failing" => Health::Failing,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub fn as_i64(self) -> i64 {
        match self {
            Stream::Stdout => 1,
            Stream::Stderr => 2,
        }
    }

    pub fn from_i64(v: i64) -> Self {
        if v == 2 {
            Stream::Stderr
        } else {
            Stream::Stdout
        }
    }
}

/// A classified failure cause with the lines that justify it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnosis {
    /// Short machine-readable cause, e.g. `cuda-oom`, `import-error`, `spawn-failed`.
    pub cause: String,
    /// One-line human summary.
    pub summary: String,
    /// True when the run died within the early-death window (failed to really start).
    #[serde(default)]
    pub early: bool,
    /// Output lines supporting the diagnosis.
    #[serde(default)]
    pub evidence: Vec<EvidenceLine>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceLine {
    pub seq: u64,
    pub text: String,
}

/// Everything that happens to a run, in order. `seq` is per-run and monotonic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub run_id: String,
    pub seq: u64,
    pub ts: Millis,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// One line of output. `cr` marks a carriage-return redraw (progress bar) that
    /// replaces the previous `cr` line of the same stream rather than appending.
    Output {
        stream: Stream,
        text: String,
        #[serde(default)]
        cr: bool,
    },
    Lifecycle {
        state: Lifecycle,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Diagnosis(Diagnosis),
}

impl EventKind {
    pub fn name(&self) -> &'static str {
        match self {
            EventKind::Output { .. } => "output",
            EventKind::Lifecycle { .. } => "lifecycle",
            EventKind::Diagnosis(_) => "diagnosis",
        }
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// Compact view of a run, as kept in the hub's run index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
    pub id: String,
    pub project: String,
    pub name: String,
    pub host: String,
    pub cmd: Vec<String>,
    pub cwd: String,
    pub lifecycle: Lifecycle,
    pub health: Health,
    pub created_at: Millis,
    pub started_at: Option<Millis>,
    pub ended_at: Option<Millis>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub pid: Option<u32>,
    pub diagnosis: Option<Diagnosis>,
    /// Highest event seq recorded so far.
    pub last_seq: u64,
    /// Timestamp of the most recent output line, if any.
    pub last_output_at: Option<Millis>,
}

impl RunSummary {
    pub fn elapsed_ms(&self, now: Millis) -> Option<Millis> {
        let start = self.started_at?;
        Some(self.ended_at.unwrap_or(now) - start)
    }
}

// ---------------------------------------------------------------------------
// API payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthInfo {
    pub version: String,
    pub pid: u32,
    pub data_dir: String,
    pub started_at: Millis,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateRun {
    pub cmd: Vec<String>,
    pub cwd: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CancelRun {
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub seq: u64,
    pub ts: Millis,
    pub stream: Stream,
    pub text: String,
    #[serde(default)]
    pub cr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub lines: Vec<LogLine>,
    /// Pass as `since_seq` to continue after this page.
    pub next_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    pub run_count: u64,
    pub active_count: u64,
    pub last_run_at: Option<Millis>,
}

/// Messages on the fleet-wide SSE stream (`/api/stream`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FleetEvent {
    Run(RunSummary),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_json_shape_is_flat() {
        let e = Event {
            run_id: "r".into(),
            seq: 3,
            ts: 10,
            kind: EventKind::Output {
                stream: Stream::Stderr,
                text: "hi".into(),
                cr: false,
            },
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "output");
        assert_eq!(v["stream"], "stderr");
        assert_eq!(v["seq"], 3);
        let back: Event = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn lifecycle_round_trips() {
        for l in [
            Lifecycle::Queued,
            Lifecycle::Starting,
            Lifecycle::Running,
            Lifecycle::Succeeded,
            Lifecycle::Failed,
            Lifecycle::Cancelled,
            Lifecycle::Lost,
            Lifecycle::Preempted,
        ] {
            assert_eq!(Lifecycle::parse(l.as_str()), Some(l));
        }
        assert!(Lifecycle::Failed.is_terminal());
        assert!(!Lifecycle::Running.is_terminal());
    }
}
