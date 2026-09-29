//! Low-confidence fallback: `N/M` or `N of M` at the start of a line
//! (`Epoch 3/10`, `[12/50] compiling`, `Step 120 of 1000`). Only consulted while
//! nothing more specific has reported progress for the run.

use super::{LineParser, slug};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use trun_proto::Directive;

static FRACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*\[?\s*(?:(?P<label>[A-Za-z][A-Za-z_\- ]{0,24}?)\s*[:#]?\s*)?(?P<cur>\d+)\s*(?:/|\bof\b)\s*(?P<tot>\d+)\s*\]?(?:\s|:|$)")
        .expect("fraction regex")
});

#[derive(Default)]
pub struct Fraction {
    seen: HashSet<String>,
}

impl LineParser for Fraction {
    fn name(&self) -> &'static str {
        "generic-fraction"
    }

    fn parse(&mut self, line: &str, _cr: bool, out: &mut Vec<Directive>) {
        let Some(c) = FRACTION.captures(line) else {
            return;
        };
        let (Ok(cur), Ok(tot)) = (c["cur"].parse::<f64>(), c["tot"].parse::<f64>()) else {
            return;
        };
        if tot < 2.0 || cur > tot || tot > 1e9 {
            return;
        }
        let label = c.name("label").map(|m| m.as_str().trim()).unwrap_or("");
        let id = match slug(label) {
            s if s.is_empty() => "progress".to_string(),
            s => s,
        };
        if self.seen.insert(id.clone()) {
            let name = if label.is_empty() {
                "progress".into()
            } else {
                label.to_string()
            };
            out.push(Directive::StepBegin {
                id: id.clone(),
                name: Some(name),
                parent: None,
            });
        }
        out.push(Directive::Progress {
            id: Some(id),
            current: cur,
            total: Some(tot),
            unit: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(line: &str) -> Option<(String, f64, f64)> {
        let mut p = Fraction::default();
        let mut out = vec![];
        p.parse(line, false, &mut out);
        out.into_iter().find_map(|d| match d {
            Directive::Progress {
                id, current, total, ..
            } => Some((id.unwrap(), current, total.unwrap())),
            _ => None,
        })
    }

    #[test]
    fn matches_common_forms() {
        assert_eq!(prog("Epoch 3/10"), Some(("epoch".into(), 3.0, 10.0)));
        assert_eq!(
            prog("[12/50] compiling foo.c"),
            Some(("progress".into(), 12.0, 50.0))
        );
        assert_eq!(
            prog("Step 120 of 1000: loss 0.2"),
            Some(("step".into(), 120.0, 1000.0))
        );
    }

    #[test]
    fn rejects_non_progress() {
        assert_eq!(prog("3/1 things"), None);
        assert_eq!(prog("result: x = 3/10"), None);
        assert_eq!(prog("1/1"), None);
        assert_eq!(prog("2024/10/05 12:00 started"), None);
    }
}
