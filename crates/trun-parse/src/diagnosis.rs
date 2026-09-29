//! Classify why a run failed from its exit status and the tail of its output.
//!
//! The goal is a one-line answer to "why did it fail?" plus the evidence lines,
//! so neither a human nor an AI agent has to read the whole log to find out.

use regex::Regex;
use std::sync::LazyLock;
use trun_proto::{Diagnosis, EvidenceLine};

/// Runs that fail within this window are flagged `early` ("failed to start").
pub const EARLY_DEATH_MS: i64 = 60_000;

/// How a process ended, as seen by the supervisor.
#[derive(Debug, Clone, Default)]
pub struct ExitInfo {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    /// The process could not be started at all (e.g. command not found).
    pub spawn_error: Option<String>,
    /// We killed it on purpose (cancel); no diagnosis wanted.
    pub cancelled: bool,
    pub elapsed_ms: i64,
}

struct Rule {
    cause: &'static str,
    re: Regex,
    /// Human summary prefix; the matched line is appended.
    label: &'static str,
}

/// Ordered most-specific first: the first rule with a match wins.
static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let r = |cause, label, pat: &str| Rule {
        cause,
        label,
        re: Regex::new(pat).expect("valid rule regex"),
    };
    vec![
        r(
            "cuda-oom",
            "CUDA out of memory",
            r"(?i)CUDA out of memory|torch\.(cuda\.)?OutOfMemoryError|CUDA error: out of memory|CUBLAS_STATUS_ALLOC_FAILED",
        ),
        r(
            "disk-full",
            "Disk full",
            r"(?i)No space left on device|ENOSPC|disk quota exceeded",
        ),
        r(
            "oom",
            "Out of memory",
            r"(?i)\bMemoryError\b|std::bad_alloc|memory allocation of \d+ bytes failed|\bout of memory\b|Cannot allocate memory",
        ),
        r(
            "segfault",
            "Segmentation fault",
            r"(?i)Segmentation fault|SIGSEGV|\bsegfault\b|access violation|STATUS_ACCESS_VIOLATION",
        ),
        r(
            "import-error",
            "Missing module or library",
            r"ModuleNotFoundError|ImportError|Cannot find module|error while loading shared libraries|cannot open shared object file|DLL load failed|could not find `[^`]+` in",
        ),
        r(
            "permission-denied",
            "Permission denied",
            r"(?i)Permission denied|PermissionError|Access is denied|EACCES",
        ),
        r(
            "file-not-found",
            "File not found",
            r"(?i)FileNotFoundError|No such file or directory|cannot find the (file|path) specified|ENOENT",
        ),
        r(
            "address-in-use",
            "Port already in use",
            r"(?i)address already in use|EADDRINUSE|Only one usage of each socket address",
        ),
        r(
            "connection-error",
            "Connection failed",
            r"(?i)Connection refused|ECONNREFUSED|Connection reset|Name or service not known|getaddrinfo failed|Temporary failure in name resolution",
        ),
        r(
            "test-failures",
            "Test failures",
            r"test result: FAILED|^FAILED |\d+ failed[,\s]|Tests:\s+\d+ failed|FAILURES!|^--- FAIL:",
        ),
        r("rust-panic", "Rust panic", r"panicked at"),
        r(
            "python-exception",
            "Python exception",
            r"^Traceback \(most recent call last\):",
        ),
        r(
            "node-error",
            "Uncaught error",
            r"^(Uncaught |)(TypeError|ReferenceError|SyntaxError|RangeError|Error): |UnhandledPromiseRejection",
        ),
        r(
            "assertion",
            "Assertion failed",
            r"(?i)assertion failed|AssertionError|assert(ion)? .* failed",
        ),
    ]
});

/// A generic "error:" line, used as a fallback summary.
static GENERIC_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(fatal|error|exception)\b[:\s]").expect("valid regex"));

/// Last line of a Python traceback: `SomeError: message`.
static PY_EXC_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][\w.]*(Error|Exception|Interrupt|Exit)\b(:.*)?$").expect("valid regex")
});

/// Keeps the last N output lines of a run and produces a [`Diagnosis`] at exit.
#[derive(Debug)]
pub struct Diagnoser {
    tail: std::collections::VecDeque<EvidenceLine>,
    cap: usize,
}

impl Default for Diagnoser {
    fn default() -> Self {
        Self::new(200)
    }
}

impl Diagnoser {
    pub fn new(cap: usize) -> Self {
        Self {
            tail: std::collections::VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, seq: u64, text: &str) {
        if self.tail.len() == self.cap {
            self.tail.pop_front();
        }
        self.tail.push_back(EvidenceLine {
            seq,
            text: text.to_string(),
        });
    }

    /// Returns `None` for successful or deliberately cancelled runs.
    pub fn diagnose(&self, exit: &ExitInfo) -> Option<Diagnosis> {
        if exit.cancelled {
            return None;
        }
        let early = exit.elapsed_ms < EARLY_DEATH_MS;

        if let Some(err) = &exit.spawn_error {
            return Some(Diagnosis {
                cause: "spawn-failed".into(),
                summary: format!("Could not start the command: {err}"),
                early: true,
                evidence: vec![],
            });
        }
        if exit.exit_code == Some(0) && exit.signal.is_none() {
            return None;
        }

        // Output-based rules first: they are more specific than the exit status.
        for rule in RULES.iter() {
            let hits: Vec<&EvidenceLine> = self
                .tail
                .iter()
                .filter(|l| rule.re.is_match(&l.text))
                .collect();
            if hits.is_empty() {
                continue;
            }
            let summary_line = if rule.cause == "python-exception" {
                self.python_exception_line()
                    .unwrap_or_else(|| hits[hits.len() - 1].clone())
            } else {
                hits[hits.len() - 1].clone()
            };
            let mut evidence: Vec<EvidenceLine> = hits
                .iter()
                .rev()
                .take(4)
                .rev()
                .map(|l| (*l).clone())
                .collect();
            if !evidence.iter().any(|e| e.seq == summary_line.seq) {
                evidence.push(summary_line.clone());
            }
            return Some(Diagnosis {
                cause: rule.cause.into(),
                summary: format!("{}: {}", rule.label, clip(summary_line.text.trim(), 240)),
                // Failing tests are a result, not a failure to start.
                early: early && !matches!(rule.cause, "test-failures" | "assertion"),
                evidence,
            });
        }

        // Then signals / well-known exit codes.
        if let Some(sig) = exit.signal {
            let (cause, what) = match sig {
                9 => ("killed", "Killed by SIGKILL (often the OOM killer)"),
                11 => ("segfault", "Segmentation fault (SIGSEGV)"),
                6 => ("aborted", "Aborted (SIGABRT)"),
                15 => ("terminated", "Terminated by SIGTERM"),
                2 => ("interrupted", "Interrupted (SIGINT)"),
                _ => ("signal", "Killed by a signal"),
            };
            return Some(self.with_fallback(cause, format!("{what} [signal {sig}]"), early));
        }
        match exit.exit_code {
            Some(137) => {
                return Some(self.with_fallback(
                    "killed",
                    "Exit 137: killed (often the OOM killer)".into(),
                    early,
                ));
            }
            Some(139) => {
                return Some(self.with_fallback(
                    "segfault",
                    "Exit 139: segmentation fault".into(),
                    early,
                ));
            }
            // Windows NTSTATUS codes surface as negative i32 exit codes.
            Some(-1073741819) => {
                return Some(self.with_fallback(
                    "segfault",
                    "Access violation (0xC0000005)".into(),
                    early,
                ));
            }
            Some(-1073741571) => {
                return Some(self.with_fallback(
                    "stack-overflow",
                    "Stack overflow (0xC00000FD)".into(),
                    early,
                ));
            }
            Some(-1073741515) => {
                return Some(self.with_fallback(
                    "import-error",
                    "A required DLL was not found (0xC0000135)".into(),
                    early,
                ));
            }
            Some(127) => {
                return Some(self.with_fallback(
                    "command-not-found",
                    "Exit 127: command not found".into(),
                    early,
                ));
            }
            _ => {}
        }

        let code = exit
            .exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into());
        Some(self.with_fallback("unknown", format!("Exited with code {code}"), early))
    }

    /// Attach the most relevant error-looking line (or the last line) as evidence.
    fn with_fallback(&self, cause: &str, mut summary: String, early: bool) -> Diagnosis {
        let line = self
            .tail
            .iter()
            .rev()
            .find(|l| GENERIC_ERROR.is_match(&l.text))
            .or_else(|| self.tail.iter().rev().find(|l| !l.text.trim().is_empty()))
            .cloned();
        if let Some(l) = &line
            && cause == "unknown"
        {
            summary = format!("{summary}: {}", clip(l.text.trim(), 240));
        }
        Diagnosis {
            cause: cause.into(),
            summary,
            early,
            evidence: line.into_iter().collect(),
        }
    }

    fn python_exception_line(&self) -> Option<EvidenceLine> {
        self.tail
            .iter()
            .rev()
            .find(|l| PY_EXC_LINE.is_match(l.text.trim()))
            .cloned()
    }
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(lines: &[&str], exit: ExitInfo) -> Option<Diagnosis> {
        let mut d = Diagnoser::default();
        for (i, l) in lines.iter().enumerate() {
            d.push(i as u64 + 1, l);
        }
        d.diagnose(&exit)
    }

    fn failed(code: i32) -> ExitInfo {
        ExitInfo {
            exit_code: Some(code),
            elapsed_ms: 120_000,
            ..Default::default()
        }
    }

    #[test]
    fn success_has_no_diagnosis() {
        assert!(
            diag(
                &["ok"],
                ExitInfo {
                    exit_code: Some(0),
                    ..Default::default()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn cancelled_has_no_diagnosis() {
        assert!(
            diag(
                &["x"],
                ExitInfo {
                    exit_code: Some(1),
                    cancelled: true,
                    ..Default::default()
                }
            )
            .is_none()
        );
    }

    #[test]
    fn python_traceback_uses_exception_line() {
        let d = diag(
            &[
                "starting",
                "Traceback (most recent call last):",
                "  File \"train.py\", line 3, in <module>",
                "ValueError: bad shape (3, 4)",
            ],
            failed(1),
        )
        .unwrap();
        assert_eq!(d.cause, "python-exception");
        assert!(d.summary.contains("ValueError: bad shape"), "{}", d.summary);
    }

    #[test]
    fn import_error_beats_traceback() {
        let d = diag(
            &[
                "Traceback (most recent call last):",
                "ModuleNotFoundError: No module named 'torch'",
            ],
            ExitInfo {
                exit_code: Some(1),
                elapsed_ms: 800,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(d.cause, "import-error");
        assert!(d.early);
    }

    #[test]
    fn cuda_oom() {
        let d = diag(
            &["torch.OutOfMemoryError: CUDA out of memory. Tried to allocate 2.00 GiB"],
            failed(1),
        )
        .unwrap();
        assert_eq!(d.cause, "cuda-oom");
    }

    #[test]
    fn sigkill_without_output_hint() {
        let d = diag(
            &["epoch 3"],
            ExitInfo {
                signal: Some(9),
                elapsed_ms: 99_000,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(d.cause, "killed");
        assert!(!d.early);
    }

    #[test]
    fn windows_access_violation() {
        let d = diag(&["working"], failed(-1073741819)).unwrap();
        assert_eq!(d.cause, "segfault");
    }

    #[test]
    fn spawn_failure() {
        let d = diag(
            &[],
            ExitInfo {
                spawn_error: Some("program not found".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(d.cause, "spawn-failed");
        assert!(d.early);
    }

    #[test]
    fn unknown_uses_error_line() {
        let d = diag(
            &["doing stuff", "error: config missing key 'lr'", "bye"],
            failed(2),
        )
        .unwrap();
        assert_eq!(d.cause, "unknown");
        assert!(d.summary.contains("config missing key"), "{}", d.summary);
    }

    #[test]
    fn cargo_test_failures() {
        let d = diag(
            &[
                "test foo ... FAILED",
                "test result: FAILED. 3 passed; 1 failed",
            ],
            failed(101),
        )
        .unwrap();
        assert_eq!(d.cause, "test-failures");
    }
}
