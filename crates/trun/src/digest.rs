//! The run digest: a compact, predictable summary shared by `trun status`,
//! `trun wait`, and (M5) the MCP `get_status` tool (docs/08-mcp.md).

use crate::fmt;
use std::fmt::Write as _;
use trun_proto::{LogLine, Millis, RunSummary};

pub fn render(r: &RunSummary, tail: &[LogLine], now: Millis) -> String {
    let mut o = String::new();
    let _ = writeln!(
        o,
        "run {} \"{}\" · project {} · host {}",
        r.id, r.name, r.project, r.host
    );
    let took = r
        .elapsed_ms(now)
        .map(|d| format!(" · {}", fmt::duration(d)))
        .unwrap_or_default();
    let health = if r.lifecycle.is_terminal() && r.alerts.is_empty() {
        String::new()
    } else {
        format!(" · health {}", r.health.as_str().to_uppercase())
    };
    let _ = writeln!(
        o,
        "state: {} {}{took}{health}",
        fmt::symbol(r.lifecycle),
        fmt::state(r)
    );
    let _ = writeln!(o, "cmd:   {}", fmt::command(&r.cmd));
    let _ = writeln!(o, "cwd:   {}", r.cwd);
    match &r.diagnosis {
        Some(d) => {
            let _ = writeln!(
                o,
                "diagnosis: {} — {}{}",
                d.cause,
                d.summary,
                if d.early { " [died early]" } else { "" }
            );
            for e in &d.evidence {
                let _ = writeln!(o, "  [{}] {}", e.seq, fmt::truncate(e.text.trim_end(), 160));
            }
        }
        None => {
            let _ = writeln!(o, "diagnosis: –");
        }
    }
    if !r.alerts.is_empty() {
        let _ = writeln!(o, "alerts (open):");
        for a in &r.alerts {
            let _ = writeln!(
                o,
                "  [{}] {}   since {} · {}",
                a.level.as_str(),
                fmt::truncate(&a.message, 120),
                fmt::duration(now - a.opened_at),
                a.check
            );
        }
    }
    if !r.steps.is_empty() {
        let _ = writeln!(o, "steps:");
        for line in fmt::step_tree(&r.steps, now) {
            let _ = writeln!(o, "  {line}");
        }
    }
    if !r.metrics.is_empty() {
        let _ = writeln!(o, "metrics (last):");
        let width = r
            .metrics
            .keys()
            .map(|k| k.chars().count())
            .max()
            .unwrap_or(0)
            .min(24);
        for (name, m) in r.metrics.iter().take(20) {
            let step = m.step.map(|s| format!(" step {s}")).unwrap_or_default();
            let nf = if m.non_finite > 0 {
                format!(" · {} non-finite!", m.non_finite)
            } else {
                String::new()
            };
            let _ = writeln!(
                o,
                "  {:<width$}  {:>12}{step} · {}{nf}",
                fmt::truncate(name, 24),
                fmt::num(m.value.0),
                fmt::ago(m.ts)
            );
        }
        if r.metrics.len() > 20 {
            let _ = writeln!(o, "  … {} more", r.metrics.len() - 20);
        }
    }
    match r.last_output_at {
        Some(t) => {
            let _ = writeln!(o, "last output {} (seq {}):", fmt::ago(t), r.last_seq);
        }
        None => {
            let _ = writeln!(o, "no output yet");
        }
    }
    for l in tail {
        let _ = writeln!(o, "  [{}] {}", l.seq, fmt::truncate(l.text.trim_end(), 200));
    }
    o
}
