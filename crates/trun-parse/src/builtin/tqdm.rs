//! tqdm progress bars, including the postfix (`loss=0.12`) as metrics.
//!
//! ```text
//! Epoch 1:  45%|████▌     | 45/100 [00:03<00:04, 12.30it/s, loss=0.123]
//!  45%|####5     | 45/100 [00:03<00:04, 12.30it/s]
//! 123it [00:01, 99.20it/s]
//! ```

use super::{LineParser, scaled, slug};
use regex::Regex;
use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;
use trun_proto::{Directive, Num, parse_num};

static WITH_TOTAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<desc>.*?)\s*(?P<pct>\d{1,3})%\|[^|]*\|\s*(?P<cur>[\d.]+[kMGTP]?)/(?P<tot>[\d.]+[kMGTP]?)\s*\[(?P<inner>[^\]]*)\]\s*$")
        .expect("tqdm regex")
});

static NO_TOTAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<desc>.*?)\s*(?P<cur>[\d.]+[kMGTP]?)(?P<unit>[A-Za-z]+)\s*\[(?P<inner>\d+:\d\d[^\]]*)\]\s*$")
        .expect("tqdm regex")
});

/// `12.30it/s` → `it`; `1.23s/it` → `it`; `?it/s` → `it`.
fn unit_from_rate(rate: &str) -> Option<String> {
    let r = rate
        .trim()
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '?');
    let r = r.trim_start_matches(|c: char| matches!(c, 'k' | 'M' | 'G' | 'T' | 'P') && r.len() > 3);
    if let Some(u) = r.strip_suffix("/s") {
        return (!u.is_empty()).then(|| u.to_string());
    }
    if let Some(u) = r.strip_prefix("s/") {
        return (!u.is_empty()).then(|| u.to_string());
    }
    None
}

#[derive(Default)]
pub struct Tqdm {
    seen: HashSet<String>,
}

impl Tqdm {
    fn step(&mut self, desc: &str, out: &mut Vec<Directive>) -> String {
        let desc = desc.trim().trim_end_matches(':').trim();
        let id = match slug(desc) {
            s if s.is_empty() => "progress".to_string(),
            s => s,
        };
        if self.seen.insert(id.clone()) {
            let name = if desc.is_empty() {
                "progress".to_string()
            } else {
                desc.to_string()
            };
            out.push(Directive::StepBegin {
                id: id.clone(),
                name: Some(name),
                parent: None,
            });
        }
        id
    }
}

/// Postfix `k=v` pairs after elapsed and rate, e.g. `loss=0.123, acc=0.9`.
fn postfix_metrics(inner: &str) -> BTreeMap<String, Num> {
    let mut m = BTreeMap::new();
    for part in inner.split(',').skip(2) {
        if let Some((k, v)) = part.trim().split_once('=')
            && let Some(x) = parse_num(v)
        {
            let k = k.trim();
            if !k.is_empty() && !k.contains(char::is_whitespace) {
                m.insert(k.to_string(), Num(x));
            }
        }
    }
    m
}

impl LineParser for Tqdm {
    fn name(&self) -> &'static str {
        "tqdm"
    }

    fn parse(&mut self, line: &str, _cr: bool, out: &mut Vec<Directive>) {
        if let Some(c) = WITH_TOTAL.captures(line) {
            let (Some(cur), Some(tot)) = (scaled(&c["cur"]), scaled(&c["tot"])) else {
                return;
            };
            let inner = &c["inner"];
            let unit = inner.split(',').nth(1).and_then(unit_from_rate);
            let id = self.step(&c["desc"], out);
            out.push(Directive::Progress {
                id: Some(id),
                current: cur,
                total: Some(tot),
                unit,
            });
            let m = postfix_metrics(inner);
            if !m.is_empty() {
                out.push(Directive::Metric {
                    values: m,
                    step: None,
                });
            }
        } else if let Some(c) = NO_TOTAL.captures(line) {
            let Some(cur) = scaled(&c["cur"]) else { return };
            let inner = &c["inner"];
            let id = self.step(&c["desc"], out);
            out.push(Directive::Progress {
                id: Some(id),
                current: cur,
                total: None,
                unit: Some(c["unit"].to_string()),
            });
            let m = postfix_metrics(inner);
            if !m.is_empty() {
                out.push(Directive::Metric {
                    values: m,
                    step: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> Vec<Directive> {
        let mut p = Tqdm::default();
        let mut out = vec![];
        for l in lines {
            p.parse(l, true, &mut out);
        }
        out
    }

    #[test]
    fn bar_with_desc_and_postfix() {
        let out = run(&[
            "Epoch 1:  45%|████▌     | 45/100 [00:03<00:04, 12.30it/s, loss=0.123, lr=3e-4]",
        ]);
        assert_eq!(
            out[0],
            Directive::StepBegin {
                id: "epoch-1".into(),
                name: Some("Epoch 1".into()),
                parent: None
            }
        );
        assert_eq!(
            out[1],
            Directive::Progress {
                id: Some("epoch-1".into()),
                current: 45.0,
                total: Some(100.0),
                unit: Some("it".into())
            }
        );
        match &out[2] {
            Directive::Metric { values, .. } => {
                assert_eq!(values["loss"], Num(0.123));
                assert_eq!(values["lr"], Num(3e-4));
            }
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn bare_bar_and_step_created_once() {
        let out = run(&[
            "  0%|          | 0/50 [00:00<?, ?it/s]",
            " 50%|#####     | 25/50 [00:01<00:01, 24.1it/s]",
            "100%|##########| 50/50 [00:02<00:00, 24.3it/s]",
        ]);
        assert_eq!(
            out.iter()
                .filter(|d| matches!(d, Directive::StepBegin { .. }))
                .count(),
            1
        );
        assert!(
            matches!(&out[3], Directive::Progress { current, total: Some(t), .. } if *current == 50.0 && *t == 50.0)
        );
    }

    #[test]
    fn unit_scale_and_slow_rate() {
        let out = run(&["download: 12%|#2        | 1.20M/10.0M [00:03<00:22, 1.23s/B]"]);
        assert!(
            matches!(&out[1], Directive::Progress { current, total: Some(t), unit: Some(u), .. }
            if *current == 1.2e6 && *t == 1e7 && u == "B")
        );
    }

    #[test]
    fn no_total() {
        let out = run(&["123it [00:01, 99.20it/s]"]);
        assert!(
            matches!(&out[1], Directive::Progress { current, total: None, .. } if *current == 123.0)
        );
    }

    #[test]
    fn ignores_normal_lines() {
        assert!(run(&["loading 45% done", "epoch 3 | loss 0.2"]).is_empty());
    }
}
