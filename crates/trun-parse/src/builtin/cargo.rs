//! cargo build / test output: a build step with crate progress, one step per test
//! binary with per-test progress, failures, and cumulative test counts as metrics.

use super::LineParser;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;
use trun_proto::{Directive, LogLevel, Num, StepStatus};

static COMPILING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+(Compiling|Checking|Documenting) (\S+) v").unwrap());
/// Only printed on a TTY (e.g. `trun run --pty`): `Building [====>   ] 45/120: foo`
static BUILDING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+Building \[[^\]]*\]\s+(\d+)/(\d+)").unwrap());
static FINISHED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s+Finished ").unwrap());
static BUILD_FAILED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^error: could not compile").unwrap());
static RUNNING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+Running ([^`].*?)(?: \([^)]*\))?\s*$").unwrap());
static DOC_TESTS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s+Doc-tests (\S+)").unwrap());
static RUNNING_N: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^running (\d+) tests?$").unwrap());
static TEST_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^test (\S+)(?: - .*?)? \.\.\. (ok|FAILED|ignored|bench:.*)$").unwrap()
});
static RESULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored").unwrap()
});

#[derive(Default)]
pub struct Cargo {
    build_open: bool,
    compiled: f64,
    test_parent: bool,
    current_test: Option<String>,
    test_total: Option<f64>,
    test_done: f64,
    binaries: usize,
    passed: f64,
    failed: f64,
    ignored: f64,
}

impl Cargo {
    fn ensure_build(&mut self, out: &mut Vec<Directive>) {
        if !self.build_open {
            self.build_open = true;
            out.push(Directive::StepBegin {
                id: "build".into(),
                name: Some("cargo build".into()),
                parent: None,
            });
        }
    }

    fn begin_test_binary(&mut self, name: &str, out: &mut Vec<Directive>) {
        if let Some(prev) = self.current_test.take() {
            out.push(Directive::StepEnd {
                id: Some(prev),
                status: StepStatus::Ok,
            });
        }
        if !self.test_parent {
            self.test_parent = true;
            out.push(Directive::StepBegin {
                id: "test".into(),
                name: Some("cargo test".into()),
                parent: None,
            });
        }
        self.binaries += 1;
        let id = format!("test-{}", self.binaries);
        out.push(Directive::StepBegin {
            id: id.clone(),
            name: Some(name.to_string()),
            parent: Some("test".into()),
        });
        self.current_test = Some(id);
        self.test_total = None;
        self.test_done = 0.0;
    }
}

impl LineParser for Cargo {
    fn name(&self) -> &'static str {
        "cargo"
    }

    fn parse(&mut self, line: &str, _cr: bool, out: &mut Vec<Directive>) {
        let line = line.trim_end();
        if COMPILING.is_match(line) {
            self.ensure_build(out);
            self.compiled += 1.0;
            out.push(Directive::Progress {
                id: Some("build".into()),
                current: self.compiled,
                total: None,
                unit: Some("crates".into()),
            });
        } else if let Some(c) = BUILDING.captures(line) {
            self.ensure_build(out);
            let (cur, tot) = (c[1].parse().unwrap_or(0.0), c[2].parse().ok());
            out.push(Directive::Progress {
                id: Some("build".into()),
                current: cur,
                total: tot,
                unit: Some("crates".into()),
            });
        } else if FINISHED.is_match(line) {
            if self.build_open {
                self.build_open = false;
                out.push(Directive::StepEnd {
                    id: Some("build".into()),
                    status: StepStatus::Ok,
                });
            }
        } else if BUILD_FAILED.is_match(line) {
            if self.build_open {
                self.build_open = false;
                out.push(Directive::StepEnd {
                    id: Some("build".into()),
                    status: StepStatus::Failed,
                });
            }
        } else if let Some(c) = RUNNING.captures(line) {
            self.begin_test_binary(&c[1], out);
        } else if let Some(c) = DOC_TESTS.captures(line) {
            self.begin_test_binary(&format!("doc-tests {}", &c[1]), out);
        } else if let Some(c) = RUNNING_N.captures(line) {
            if let Some(id) = &self.current_test {
                self.test_total = c[1].parse().ok();
                self.test_done = 0.0;
                out.push(Directive::Progress {
                    id: Some(id.clone()),
                    current: 0.0,
                    total: self.test_total,
                    unit: Some("tests".into()),
                });
            }
        } else if let Some(c) = TEST_LINE.captures(line) {
            if &c[2] == "FAILED" {
                out.push(Directive::Log {
                    level: LogLevel::Error,
                    text: format!("test {} FAILED", &c[1]),
                });
            }
            if let Some(id) = &self.current_test {
                self.test_done += 1.0;
                out.push(Directive::Progress {
                    id: Some(id.clone()),
                    current: self.test_done,
                    total: self.test_total,
                    unit: Some("tests".into()),
                });
            }
        } else if let Some(c) = RESULT.captures(line) {
            let (p, f, i): (f64, f64, f64) = (
                c[2].parse().unwrap_or(0.0),
                c[3].parse().unwrap_or(0.0),
                c[4].parse().unwrap_or(0.0),
            );
            self.passed += p;
            self.failed += f;
            self.ignored += i;
            let status = if &c[1] == "ok" {
                StepStatus::Ok
            } else {
                StepStatus::Failed
            };
            if let Some(id) = self.current_test.take() {
                out.push(Directive::StepEnd {
                    id: Some(id),
                    status,
                });
            }
            let values = BTreeMap::from([
                ("tests_passed".to_string(), Num(self.passed)),
                ("tests_failed".to_string(), Num(self.failed)),
                ("tests_ignored".to_string(), Num(self.ignored)),
            ]);
            out.push(Directive::Metric { values, step: None });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<Directive> {
        let mut p = Cargo::default();
        let mut out = vec![];
        for l in lines {
            p.parse(l, false, &mut out);
        }
        out
    }

    #[test]
    fn build_then_tests() {
        let out = run(&[
            "   Compiling serde v1.0.200",
            "   Compiling trun v0.1.0 (C:\\x)",
            "    Finished `test` profile [unoptimized + debuginfo] target(s) in 3.2s",
            "     Running unittests src/lib.rs (target/debug/deps/trun-abc)",
            "",
            "running 2 tests",
            "test a::works ... ok",
            "test b::breaks ... FAILED",
            "",
            "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
            "   Doc-tests trun",
            "running 0 tests",
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out",
        ]);
        let kinds: Vec<String> = out
            .iter()
            .map(|d| match d {
                Directive::StepBegin { id, .. } => format!("begin:{id}"),
                Directive::StepEnd { id, status } => {
                    format!("end:{}:{status:?}", id.as_deref().unwrap_or("?"))
                }
                Directive::Progress { id, current, .. } => {
                    format!("p:{}:{current}", id.as_deref().unwrap_or("?"))
                }
                Directive::Log { .. } => "log".into(),
                Directive::Metric { .. } => "metric".into(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                "begin:build",
                "p:build:1",
                "p:build:2",
                "end:build:Ok",
                "begin:test",
                "begin:test-1",
                "p:test-1:0",
                "p:test-1:1",
                "log",
                "p:test-1:2",
                "end:test-1:Failed",
                "metric",
                "begin:test-2",
                "p:test-2:0",
                "end:test-2:Ok",
                "metric",
            ]
        );
    }

    #[test]
    fn cargo_run_is_not_a_test_binary() {
        assert!(run(&["     Running `target/debug/trun`"]).is_empty());
    }
}
