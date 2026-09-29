//! Checks and stall detection (docs/05-checks.md).
//!
//! * [`data`]: what a check can see about a run, as time series.
//! * [`engine`]: the Starlark check engine.
//! * [`alerts`]: alert hysteresis and health.
//! * [`CheckRunner`]: which checks apply to a run, when each is due, hot reload,
//!   and turning actions into alert changes. Used live by the agent and offline by
//!   `trun check test` (replay).

pub mod alerts;
pub mod data;
pub mod engine;

pub use alerts::{AlertChange, AlertTracker};
pub use data::{RunData, Series};
pub use engine::{Action, ActionKind, CheckSource, Compiled, Engine, Meta};

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use trun_proto::Millis;

/// The built-in default checks.
pub const DEFAULTS: &str = include_str!("defaults.star");
pub const DEFAULTS_NAME: &str = "defaults";

static ENGINE: LazyLock<Engine> = LazyLock::new(Engine::new);

pub fn engine() -> &'static Engine {
    &ENGINE
}

/// Where a run's checks come from. Precedence by file stem: explicit files, then
/// the project's `.trun/checks`, then `$TRUN_HOME/checks`, then the built-ins.
#[derive(Debug, Clone, Default)]
pub struct Discovery {
    pub project_dir: Option<PathBuf>,
    pub global_dir: Option<PathBuf>,
    pub extra: Vec<PathBuf>,
    pub defaults: bool,
}

type Fingerprint = Vec<(PathBuf, Option<std::time::SystemTime>, u64)>;

impl Discovery {
    fn files(&self) -> Vec<PathBuf> {
        let mut out = self.extra.clone();
        for dir in [&self.project_dir, &self.global_dir].into_iter().flatten() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok().map(|e| e.path()))
                        .filter(|p| p.extension().is_some_and(|e| e == "star"))
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            out.extend(v);
        }
        out
    }

    fn fingerprint(&self) -> Fingerprint {
        self.files()
            .into_iter()
            .map(|p| {
                let m = std::fs::metadata(&p).ok();
                let (mt, len) = (
                    m.as_ref().and_then(|m| m.modified().ok()),
                    m.map(|m| m.len()).unwrap_or(0),
                );
                (p, mt, len)
            })
            .collect()
    }

    /// Resolve the effective check sources (one per name).
    pub fn sources(&self) -> Vec<Result<CheckSource, (String, String)>> {
        let mut by_name: BTreeMap<String, Result<CheckSource, (String, String)>> = BTreeMap::new();
        for p in self.files() {
            let name = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() || name.starts_with('.') || by_name.contains_key(&name) {
                continue;
            }
            let origin = p.to_string_lossy().into_owned();
            let r = std::fs::read_to_string(&p)
                .map(|text| CheckSource {
                    name: name.clone(),
                    origin: origin.clone(),
                    text,
                })
                .map_err(|e| (name.clone(), format!("{origin}: {e}")));
            by_name.insert(name, r);
        }
        if self.defaults && !by_name.contains_key(DEFAULTS_NAME) {
            by_name.insert(
                DEFAULTS_NAME.into(),
                Ok(CheckSource {
                    name: DEFAULTS_NAME.into(),
                    origin: "builtin:defaults.star".into(),
                    text: DEFAULTS.into(),
                }),
            );
        }
        by_name.into_values().collect()
    }
}

/// `*` and `?` glob on the run name.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

struct Active {
    compiled: Compiled,
    next_due: Millis,
}

/// What one tick produced.
#[derive(Debug, Default)]
pub struct TickOutcome {
    pub changes: Vec<AlertChange>,
    /// New (not previously reported) errors: (check, message).
    pub errors: Vec<(String, String)>,
    pub notify: Vec<String>,
    /// A check asked to stop the run.
    pub kill: Option<String>,
    /// Checks evaluated this tick (for replay output).
    pub evaluated: Vec<String>,
}

/// Runs the applicable checks for one run on schedule.
pub struct CheckRunner {
    discovery: Discovery,
    run_name: String,
    active: Vec<Active>,
    fingerprint: Fingerprint,
    reported: HashSet<String>,
    pub tracker: AlertTracker,
    /// Re-scan check files at most this often (0 = never, for replays).
    rescan_ms: Millis,
    last_scan: Millis,
    pending_errors: Vec<(String, String)>,
}

impl CheckRunner {
    pub fn new(discovery: Discovery, run_name: &str, rescan_ms: Millis) -> Self {
        let mut r = CheckRunner {
            discovery,
            run_name: run_name.to_string(),
            active: Vec::new(),
            fingerprint: Vec::new(),
            reported: HashSet::new(),
            tracker: AlertTracker::default(),
            rescan_ms,
            last_scan: 0,
            pending_errors: Vec::new(),
        };
        r.reload(0);
        r
    }

    /// A runner over explicit sources (replays): no discovery, no hot reload, and
    /// `applies` is ignored (you asked for these checks). Compile errors are returned.
    pub fn with_sources(
        sources: Vec<CheckSource>,
        run_name: &str,
    ) -> Result<Self, Vec<(String, String)>> {
        let mut errors = Vec::new();
        let mut active = Vec::new();
        for s in sources {
            match engine().compile(&s) {
                Ok(c) => active.push(Active {
                    compiled: c,
                    next_due: 0,
                }),
                Err(e) => errors.push((s.name.clone(), e)),
            }
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(CheckRunner {
            discovery: Discovery::default(),
            run_name: run_name.to_string(),
            active,
            fingerprint: Vec::new(),
            reported: HashSet::new(),
            tracker: AlertTracker::default(),
            rescan_ms: 0,
            last_scan: 0,
            pending_errors: Vec::new(),
        })
    }

    /// Shortest `every` among active checks, seconds.
    pub fn min_every(&self) -> f64 {
        self.active
            .iter()
            .map(|a| a.compiled.meta.every)
            .fold(f64::INFINITY, f64::min)
    }

    /// Names of the checks currently active (compiled and applicable).
    pub fn active_names(&self) -> Vec<String> {
        self.active
            .iter()
            .map(|a| a.compiled.name.clone())
            .collect()
    }

    fn reload(&mut self, now: Millis) {
        self.fingerprint = self.discovery.fingerprint();
        let mut next: Vec<Active> = Vec::new();
        for src in self.discovery.sources() {
            match src.and_then(|s| engine().compile(&s).map_err(|e| (s.name.clone(), e))) {
                Ok(c) => {
                    if c.meta
                        .applies
                        .as_deref()
                        .is_some_and(|g| !glob_match(g, &self.run_name))
                    {
                        continue;
                    }
                    // Keep the schedule of checks that were already running.
                    let due = self
                        .active
                        .iter()
                        .find(|a| a.compiled.name == c.name)
                        .map(|a| a.next_due)
                        .unwrap_or(now);
                    next.push(Active {
                        compiled: c,
                        next_due: due,
                    });
                }
                Err((name, e)) => {
                    // A broken edit keeps the previous version running.
                    if let Some(pos) = self.active.iter().position(|a| a.compiled.name == name) {
                        next.push(self.active.remove(pos));
                    }
                    self.pending_errors.push((name, e));
                }
            }
        }
        // Alerts of checks that disappeared are cleared by the caller via tick().
        self.active = next;
    }

    pub fn tick(&mut self, data: &RunData, now: Millis) -> TickOutcome {
        let mut out = TickOutcome::default();
        if self.rescan_ms > 0 && now - self.last_scan >= self.rescan_ms {
            self.last_scan = now;
            if self.discovery.fingerprint() != self.fingerprint {
                let before: Vec<String> = self.active_names();
                self.reload(now);
                for gone in before
                    .iter()
                    .filter(|n| !self.active.iter().any(|a| &a.compiled.name == *n))
                {
                    out.changes.extend(self.tracker.clear_check(gone));
                }
            }
        }
        for (check, e) in std::mem::take(&mut self.pending_errors) {
            if self.reported.insert(format!("{check}\u{0}{e}")) {
                out.errors.push((check, e));
            }
        }
        let elapsed = data.elapsed(now);
        for a in &mut self.active {
            if now < a.next_due || elapsed < a.compiled.meta.grace {
                continue;
            }
            a.next_due = now + (a.compiled.meta.every * 1000.0) as Millis;
            out.evaluated.push(a.compiled.name.clone());
            match engine().evaluate(&a.compiled, data, now) {
                Ok(actions) => {
                    for act in &actions {
                        match act.kind {
                            ActionKind::Notify => out.notify.push(act.message.clone()),
                            ActionKind::Kill => out.kill = Some(act.message.clone()),
                            ActionKind::Fail if a.compiled.meta.kill => {
                                out.kill = Some(act.message.clone())
                            }
                            _ => {}
                        }
                    }
                    out.changes.extend(self.tracker.evaluate(
                        &a.compiled.name,
                        a.compiled.meta.clear_after,
                        &actions,
                        now,
                    ));
                }
                Err(e) => {
                    if self.reported.insert(format!("{}\u{0}{e}", a.compiled.name)) {
                        out.errors.push((a.compiled.name.clone(), e));
                    }
                }
            }
        }
        out
    }
}

/// Load a check file for linting.
pub fn lint(path: &Path) -> Result<Compiled, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    engine().compile(&CheckSource {
        name,
        origin: path.to_string_lossy().into_owned(),
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use trun_proto::{AlertState, Event, EventKind, Health, Lifecycle, Num};

    fn ev(seq: u64, ts: Millis, kind: EventKind) -> Event {
        Event {
            run_id: "R".into(),
            seq,
            ts,
            kind,
        }
    }

    fn running(d: &mut RunData) {
        d.ingest(&ev(
            1,
            0,
            EventKind::Lifecycle {
                state: Lifecycle::Running,
                exit_code: None,
                signal: None,
                pid: None,
                reason: None,
            },
        ));
    }

    #[test]
    fn defaults_compile() {
        let c = engine()
            .compile(&CheckSource {
                name: "defaults".into(),
                origin: "builtin".into(),
                text: DEFAULTS.into(),
            })
            .unwrap();
        assert_eq!(c.meta.every, 5.0);
    }

    #[test]
    fn defaults_detect_silence_and_honor_expect_and_config() {
        let mut r = CheckRunner::new(
            Discovery {
                defaults: true,
                ..Default::default()
            },
            "x",
            0,
        );
        let mut d = RunData::new("R", "x", "p");
        running(&mut d);
        d.config.insert("silence".into(), 10.0);
        d.ingest(&ev(
            2,
            1000,
            EventKind::Output {
                stream: trun_proto::Stream::Stdout,
                text: "hi".into(),
                cr: false,
            },
        ));
        assert!(r.tick(&d, 5_000).changes.is_empty());
        let out = r.tick(&d, 12_000);
        assert_eq!(out.changes.len(), 1, "{:?}", out);
        assert_eq!(out.changes[0].state, AlertState::Opened);
        assert!(
            out.changes[0]
                .alert
                .message
                .starts_with("no output for 11s"),
            "{}",
            out.changes[0].alert.message
        );
        assert_eq!(r.tracker.health(), Health::Stalled);

        // ::expect silence raises the limit; the alert clears after 3 quiet evaluations.
        d.expect_silence_ms = Some(60_000);
        let mut cleared = false;
        for t in [17_000, 22_000, 27_000] {
            cleared |= r
                .tick(&d, t)
                .changes
                .iter()
                .any(|c| c.state == AlertState::Cleared);
        }
        assert!(cleared);
        assert_eq!(r.tracker.health(), Health::Ok);
    }

    #[test]
    fn defaults_fail_on_nan() {
        let mut r = CheckRunner::new(
            Discovery {
                defaults: true,
                ..Default::default()
            },
            "x",
            0,
        );
        let mut d = RunData::new("R", "x", "p");
        running(&mut d);
        d.ingest(&ev(
            2,
            1000,
            EventKind::Metric {
                values: BTreeMap::from([("loss".to_string(), Num(f64::NAN))]),
                step: Some(1),
            },
        ));
        let out = r.tick(&d, 2000);
        assert!(
            out.changes
                .iter()
                .any(|c| c.alert.level == trun_proto::AlertLevel::Fail
                    && c.alert.message.contains("loss")),
            "{out:?}"
        );
        assert_eq!(r.tracker.health(), Health::Failing);
    }

    #[test]
    fn project_file_overrides_and_hot_reloads() {
        let dir = std::env::temp_dir().join(format!("trun-checks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("mine.star");
        std::fs::write(
            &f,
            "META = {\"applies\": \"train*\", \"every\": 1}\ndef check(run):\n    warn(\"v1\")\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("other.star"),
            "META = {\"applies\": \"eval*\"}\ndef check(run):\n    fail(\"no\")\n",
        )
        .unwrap();
        let disc = Discovery {
            project_dir: Some(dir.clone()),
            defaults: true,
            ..Default::default()
        };
        let mut r = CheckRunner::new(disc, "train-1", 1);
        assert_eq!(
            r.active_names(),
            vec!["defaults".to_string(), "mine".to_string()],
            "applies filters 'other'"
        );
        let d = RunData::new("R", "train-1", "p");
        assert_eq!(r.tick(&d, 10).changes[0].alert.message, "v1");

        // A broken edit reports an error and keeps v1 running.
        std::fs::write(&f, "def check(run):\n    warn(\"v2\"\n").unwrap();
        let out = r.tick(&d, 2000);
        assert_eq!(out.errors.len(), 1, "{out:?}");
        assert!(r.active_names().contains(&"mine".to_string()));
        // A fixed edit swaps in v2.
        std::fs::write(
            &f,
            "META = {\"every\": 1}\ndef check(run):\n    warn(\"v2 now\", key=\"k\")\n",
        )
        .unwrap();
        let out = r.tick(&d, 4000);
        assert!(
            out.changes.iter().any(|c| c.alert.message == "v2 now"),
            "{out:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn globs() {
        assert!(glob_match("train*", "train-v3"));
        assert!(!glob_match("train*", "eval"));
    }
}
