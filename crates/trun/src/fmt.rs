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
        Lifecycle::Running if r.health != trun_proto::Health::Ok => {
            s.push_str(&format!(" · {}", r.health.as_str()));
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
