//! pytest: test count and progress, failures, and the final tally as metrics.
//!
//! Activates on `=== test session starts ===` so ordinary output that happens to
//! contain dots or percentages is never misread.

use super::LineParser;
use regex::Regex;
use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;
use trun_proto::{Directive, LogLevel, Num, StepStatus};

static START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^=+ test session starts =+$").unwrap());
static COLLECTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^collected (\d+) items?(?: / (\d+) deselected)?(?: / (\d+) selected)?").unwrap()
});
/// Default (quiet-ish) mode: `tests/test_x.py ..F.s     [ 45%]`
static FILE_MARKS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+\.py) ([.FEsxXp]+)\s*(?:\[\s*\d+%\])?$").unwrap());
/// Verbose: `tests/test_x.py::test_a PASSED   [ 10%]`
static VERBOSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\S+::\S+) (PASSED|FAILED|ERROR|SKIPPED|XFAIL|XPASS)\b").unwrap()
});
/// pytest-xdist verbose: `[gw0] [ 10%] PASSED tests/test_x.py::test_a`
static XDIST: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[gw\d+\] \[\s*\d+%\] (PASSED|FAILED|ERROR|SKIPPED|XFAIL|XPASS) (\S+)").unwrap()
});
static SHORT_FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(FAILED|ERROR) (\S+)").unwrap());
static SUMMARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^=+ (.+?) in [\d.]+s(?: \([^)]*\))? =+$").unwrap());
static COUNT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d+) (passed|failed|errors?|skipped|xfailed|xpassed|deselected|warnings?)")
        .unwrap()
});

#[derive(Default)]
pub struct Pytest {
    active: bool,
    total: Option<f64>,
    done: f64,
    failed_seen: HashSet<String>,
}

impl Pytest {
    fn tick(&mut self, n: usize, out: &mut Vec<Directive>) {
        self.done += n as f64;
        out.push(Directive::Progress {
            id: Some("tests".into()),
            current: self.done,
            total: self.total,
            unit: Some("tests".into()),
        });
    }

    fn failed(&mut self, test: &str, out: &mut Vec<Directive>) {
        if self.failed_seen.insert(test.to_string()) {
            out.push(Directive::Log {
                level: LogLevel::Error,
                text: format!("FAILED {test}"),
            });
        }
    }
}

impl LineParser for Pytest {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn parse(&mut self, line: &str, cr: bool, out: &mut Vec<Directive>) {
        if cr {
            return;
        }
        let line = line.trim_end();
        if START.is_match(line) {
            *self = Pytest {
                active: true,
                ..Default::default()
            };
            out.push(Directive::StepBegin {
                id: "tests".into(),
                name: Some("pytest".into()),
                parent: None,
            });
            return;
        }
        if !self.active {
            return;
        }
        if let Some(c) = COLLECTED.captures(line) {
            let collected: f64 = c[1].parse().unwrap_or(0.0);
            let selected = c.get(3).and_then(|m| m.as_str().parse().ok());
            let deselected: f64 = c
                .get(2)
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0.0);
            self.total = Some(selected.unwrap_or(collected - deselected));
            out.push(Directive::Progress {
                id: Some("tests".into()),
                current: 0.0,
                total: self.total,
                unit: Some("tests".into()),
            });
        } else if let Some(c) = VERBOSE.captures(line) {
            if matches!(&c[2], "FAILED" | "ERROR") {
                self.failed(&c[1], out);
            }
            self.tick(1, out);
        } else if let Some(c) = XDIST.captures(line) {
            if matches!(&c[1], "FAILED" | "ERROR") {
                self.failed(&c[2], out);
            }
            self.tick(1, out);
        } else if let Some(c) = FILE_MARKS.captures(line) {
            self.tick(c[2].len(), out);
        } else if let Some(c) = SUMMARY.captures(line) {
            let mut counts: BTreeMap<String, Num> = BTreeMap::new();
            let mut bad = 0.0;
            for m in COUNT.captures_iter(&c[1]) {
                let n: f64 = m[1].parse().unwrap_or(0.0);
                let key = match &m[2] {
                    "error" | "errors" => "errors",
                    "warning" | "warnings" => "warnings",
                    other => other,
                };
                if matches!(key, "failed" | "errors") {
                    bad += n;
                }
                counts.insert(format!("tests_{key}"), Num(n));
            }
            if !counts.is_empty() {
                out.push(Directive::Metric {
                    values: counts,
                    step: None,
                });
            }
            let status = if bad > 0.0 {
                StepStatus::Failed
            } else {
                StepStatus::Ok
            };
            out.push(Directive::StepEnd {
                id: Some("tests".into()),
                status,
            });
            self.active = false;
        } else if let Some(c) = SHORT_FAILED.captures(line) {
            self.failed(&c[2], out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<Directive> {
        let mut p = Pytest::default();
        let mut out = vec![];
        for l in lines {
            p.parse(l, false, &mut out);
        }
        out
    }

    #[test]
    fn default_mode_session() {
        let out = run(&[
            "============================= test session starts ==============================",
            "platform linux -- Python 3.12.1, pytest-8.0.0",
            "collected 6 items",
            "",
            "tests/test_a.py ..F                                                      [ 50%]",
            "tests/test_b.py .s.                                                      [100%]",
            "=========================== short test summary info ============================",
            "FAILED tests/test_a.py::test_three - assert 1 == 2",
            "==================== 1 failed, 4 passed, 1 skipped in 0.12s ====================",
        ]);
        assert!(matches!(&out[0], Directive::StepBegin { id, .. } if id == "tests"));
        let last_progress = out.iter().rev().find_map(|d| match d {
            Directive::Progress { current, total, .. } => Some((*current, *total)),
            _ => None,
        });
        assert_eq!(last_progress, Some((6.0, Some(6.0))));
        assert!(out.iter().any(|d| matches!(d, Directive::Log { text, .. } if text == "FAILED tests/test_a.py::test_three")));
        let metrics = out.iter().find_map(|d| match d {
            Directive::Metric { values, .. } => Some(values.clone()),
            _ => None,
        });
        let m = metrics.unwrap();
        assert_eq!(m["tests_failed"], Num(1.0));
        assert_eq!(m["tests_passed"], Num(4.0));
        assert!(matches!(
            out.last(),
            Some(Directive::StepEnd {
                status: StepStatus::Failed,
                ..
            })
        ));
    }

    #[test]
    fn verbose_and_dedup() {
        let out = run(&[
            "=== test session starts ===",
            "collected 3 items / 1 deselected / 2 selected",
            "tests/t.py::test_a PASSED  [ 50%]",
            "tests/t.py::test_b FAILED  [100%]",
            "FAILED tests/t.py::test_b - boom",
            "=== 1 failed, 1 passed, 1 deselected in 0.05s ===",
        ]);
        assert_eq!(
            out.iter()
                .filter(|d| matches!(d, Directive::Log { .. }))
                .count(),
            1
        );
        assert!(out.iter().any(|d| matches!(d, Directive::Progress { total: Some(t), current, .. } if *t == 2.0 && *current == 2.0)));
    }

    #[test]
    fn inactive_without_session_header() {
        assert!(
            run(&[
                "collected 5 items",
                "tests/x.py ...",
                "=== 3 passed in 0.1s ==="
            ])
            .is_empty()
        );
    }
}
