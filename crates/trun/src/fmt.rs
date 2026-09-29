//! Human-readable formatting shared by the commands.

use trun_proto::{Lifecycle, Millis, RunSummary, now_ms};

pub fn duration(ms: Millis) -> String {
    let ms = ms.max(0);
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let s = ms / 1000;
    if s < 60 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else {
        format!("{m}m{s:02}s")
    }
}

pub fn ago(ts: Millis) -> String {
    format!("{} ago", duration(now_ms() - ts))
}

pub fn state(r: &RunSummary) -> String {
    let mut s = r.lifecycle.as_str().to_string();
    match r.lifecycle {
        Lifecycle::Failed => {
            if let Some(sig) = r.signal {
                s.push_str(&format!(" (signal {sig})"));
            } else if let Some(c) = r.exit_code {
                s.push_str(&format!(" (exit {c})"));
            }
        }
        Lifecycle::Running => {
            if r.health != trun_proto::Health::Ok {
                s.push_str(&format!(" · {}", r.health.as_str()));
            }
            if let Some(p) = headline_progress(&r.steps) {
                s.push_str(&format!(" {p}"));
            }
        }
        _ => {}
    }
    s
}

pub fn symbol(l: Lifecycle) -> &'static str {
    match l {
        Lifecycle::Queued | Lifecycle::Starting => "…",
        Lifecycle::Running => "▶",
        Lifecycle::Succeeded => "✓",
        Lifecycle::Failed => "✗",
        Lifecycle::Cancelled => "■",
        Lifecycle::Lost | Lifecycle::Preempted => "?",
    }
}

/// Compact number: 4 significant digits, scientific for very small/large values.
pub fn num(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let a = v.abs();
    if v == v.trunc() && a < 1e12 {
        format!("{v:.0}")
    } else if !(1e-3..1e6).contains(&a) {
        format!("{v:.3e}")
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Round to 3 significant digits (rates don't need more).
fn sig3(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    let mag = 10f64.powi(2 - v.abs().log10().floor() as i32);
    (v * mag).round() / mag
}

/// `23/50 epoch (46%) · 1.2/s · eta 21s`
pub fn progress(s: &trun_proto::StepState) -> String {
    let Some(cur) = s.current else {
        return String::new();
    };
    let unit = s
        .unit
        .as_deref()
        .map(|u| format!(" {u}"))
        .unwrap_or_default();
    let mut out = match s.total {
        Some(t) if t > 0.0 => format!(
            "{}/{}{unit} ({:.0}%)",
            num(cur),
            num(t),
            (cur / t * 100.0).min(100.0)
        ),
        _ => format!("{}{unit}", num(cur)),
    };
    if s.state == trun_proto::StepRunState::Running {
        if let Some(r) = s.rate.filter(|r| *r > 0.0) {
            out.push_str(&format!(" · {}/s", num(sig3(r))));
        }
        if let Some(e) = s.eta_ms {
            out.push_str(&format!(" · eta {}", duration(e)));
        }
    }
    out
}

/// The step tree, indented by depth.
pub fn step_tree(steps: &[trun_proto::StepState], now: Millis) -> Vec<String> {
    use trun_proto::StepRunState as S;
    fn depth(steps: &[trun_proto::StepState], s: &trun_proto::StepState) -> usize {
        let mut d = 0;
        let mut p = s.parent.as_deref();
        while let Some(pid) = p {
            d += 1;
            p = steps
                .iter()
                .find(|x| x.id == pid)
                .and_then(|x| x.parent.as_deref());
            if d > 8 {
                break;
            }
        }
        d
    }
    // Parents first, children right after their parent, in begin order.
    fn visit<'a>(
        steps: &'a [trun_proto::StepState],
        parent: Option<&str>,
        out: &mut Vec<&'a trun_proto::StepState>,
    ) {
        for s in steps.iter().filter(|s| s.parent.as_deref() == parent) {
            out.push(s);
            visit(steps, Some(&s.id), out);
        }
    }
    let mut ordered = Vec::new();
    visit(steps, None, &mut ordered);
    // Orphans (parent id unknown) at the end.
    for s in steps {
        if !ordered.iter().any(|o| o.id == s.id) {
            ordered.push(s);
        }
    }
    ordered
        .into_iter()
        .map(|s| {
            let icon = match s.state {
                S::Running => "▸",
                S::Ok => "✓",
                S::Failed => "✗",
                S::Skipped => "–",
            };
            let took = duration(s.ended_at.unwrap_or(now) - s.started_at);
            let indent = "  ".repeat(depth(steps, s));
            format!(
                "{indent}{icon} {:<24} {:>8}  {}",
                truncate(&s.name, 24),
                took,
                progress(s)
            )
        })
        .collect()
}

/// Progress of the most relevant running step, for list views: `46%` or `118 it`.
pub fn headline_progress(steps: &[trun_proto::StepState]) -> Option<String> {
    let s = steps
        .iter()
        .rev()
        .find(|s| s.state == trun_proto::StepRunState::Running && s.current.is_some())?;
    let cur = s.current?;
    Some(match s.total {
        Some(t) if t > 0.0 => format!("{:.0}%", (cur / t * 100.0).min(100.0)),
        _ => format!("{} {}", num(cur), s.unit.as_deref().unwrap_or(""))
            .trim_end()
            .to_string(),
    })
}

pub fn command(cmd: &[String]) -> String {
    cmd.iter()
        .map(|a| {
            if a.is_empty() || a.contains([' ', '"', '\'']) {
                format!("{a:?}")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration(850), "850ms");
        assert_eq!(duration(3_240), "3.2s");
        assert_eq!(duration(252_000), "4m12s");
        assert_eq!(duration(3_780_000), "1h03m");
    }
}
