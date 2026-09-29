//! The `::` line protocol (docs/04-progress-protocol.md).
//!
//! ```text
//! ::step-begin id=train name="Train model"        ::step-begin compile "Compile firmware"
//! ::step-end id=train status=ok                    ::step-end compile failed
//! ::progress id=train current=12 total=50 unit=epoch
//! ::progress train 12/50        ::progress train 12 50        ::progress 0.42
//! ::metric loss=0.31 val_loss=0.4 step=1200
//! ::warn text   ::error text   ::info text   ::note text
//! ::heartbeat   ::expect silence=20m   ::expect silence=default
//! ```
//!
//! `::trun::verb` is accepted as a fully qualified form. Unknown verbs are not
//! protocol lines and pass through as ordinary output.

use std::collections::BTreeMap;
use trun_proto::{Directive, LogLevel, Num, StepStatus, parse_num};

/// Parse one output line. `None` means "not a protocol line".
pub fn parse_line(line: &str) -> Option<Result<Directive, String>> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix("::trun::")
        .or_else(|| t.strip_prefix("::"))?;
    let (verb, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest.trim_end(), ""),
    };
    let d = match verb {
        "step-begin" => step_begin(args),
        "step-end" => step_end(args),
        "progress" => progress(args),
        "metric" => metric(args),
        "warn" | "warning" => Ok(Directive::Log {
            level: LogLevel::Warn,
            text: args.to_string(),
        }),
        "error" => Ok(Directive::Log {
            level: LogLevel::Error,
            text: args.to_string(),
        }),
        "info" => Ok(Directive::Log {
            level: LogLevel::Info,
            text: args.to_string(),
        }),
        "note" => Ok(Directive::Note {
            text: args.to_string(),
        }),
        "heartbeat" => Ok(Directive::Heartbeat),
        "expect" => expect(args),
        _ => return None,
    };
    Some(d)
}

/// Positional tokens and `key=value` pairs, with double-quoted values.
#[derive(Debug, Default)]
struct Args {
    pos: Vec<String>,
    kv: BTreeMap<String, String>,
    /// Keys in order of appearance (metrics keep user order in the UI).
    order: Vec<String>,
}

fn tokenize(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            '\\' if in_q => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !in_q => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if in_q {
        return Err("unterminated quote".into());
    }
    if has {
        out.push(cur);
    }
    Ok(out)
}

fn args(s: &str) -> Result<Args, String> {
    let mut a = Args::default();
    for tok in tokenize(s)? {
        match tok.split_once('=') {
            Some((k, v)) if !k.is_empty() && !k.contains(char::is_whitespace) => {
                a.order.push(k.to_string());
                a.kv.insert(k.to_string(), v.to_string());
            }
            _ => a.pos.push(tok),
        }
    }
    Ok(a)
}

fn nonempty(s: Option<&String>) -> Option<String> {
    s.filter(|v| !v.is_empty()).cloned()
}

fn step_begin(s: &str) -> Result<Directive, String> {
    let a = args(s)?;
    let id = nonempty(a.kv.get("id"))
        .or_else(|| a.pos.first().cloned())
        .ok_or("step-begin needs an id")?;
    let name = nonempty(a.kv.get("name")).or_else(|| a.pos.get(1).cloned());
    Ok(Directive::StepBegin {
        id,
        name,
        parent: nonempty(a.kv.get("parent")),
    })
}

fn step_end(s: &str) -> Result<Directive, String> {
    let a = args(s)?;
    let id = nonempty(a.kv.get("id")).or_else(|| a.pos.first().cloned());
    let status_s =
        a.kv.get("status")
            .cloned()
            .or_else(|| a.pos.get(1).cloned());
    let status = match status_s {
        Some(s) => StepStatus::parse(&s).ok_or_else(|| format!("unknown step status '{s}'"))?,
        None => StepStatus::Ok,
    };
    Ok(Directive::StepEnd { id, status })
}

fn num(s: &str, what: &str) -> Result<f64, String> {
    parse_num(s).ok_or_else(|| format!("{what} is not a number: '{s}'"))
}

fn progress(s: &str) -> Result<Directive, String> {
    let a = args(s)?;
    let unit = nonempty(a.kv.get("unit"));
    if let Some(c) = a.kv.get("current") {
        let total = a.kv.get("total").map(|t| num(t, "total")).transpose()?;
        return Ok(Directive::Progress {
            id: nonempty(a.kv.get("id")).or_else(|| a.pos.first().cloned()),
            current: num(c, "current")?,
            total,
            unit,
        });
    }
    let id_kv = nonempty(a.kv.get("id"));
    // Positional forms: [id] N/M | [id] N M | [id] fraction
    let mut pos: Vec<&str> = a.pos.iter().map(String::as_str).collect();
    let id = match id_kv {
        Some(i) => Some(i),
        None if pos.len() >= 2
            || pos
                .first()
                .is_some_and(|p| parse_num(p).is_none() && !p.contains('/')) =>
        {
            Some(pos.remove(0).to_string())
        }
        None => None,
    };
    match pos.as_slice() {
        [frac] if frac.contains('/') => {
            let (c, t) = frac.split_once('/').unwrap();
            Ok(Directive::Progress {
                id,
                current: num(c, "current")?,
                total: Some(num(t, "total")?),
                unit,
            })
        }
        [c, t] => Ok(Directive::Progress {
            id,
            current: num(c, "current")?,
            total: Some(num(t, "total")?),
            unit,
        }),
        [f] => {
            let v = num(f, "progress")?;
            // A bare value in [0, 1] is a fraction of the step.
            let total = (0.0..=1.0).contains(&v).then_some(1.0);
            Ok(Directive::Progress {
                id,
                current: v,
                total,
                unit,
            })
        }
        _ => Err("progress needs current[/total]".into()),
    }
}

fn metric(s: &str) -> Result<Directive, String> {
    let a = args(s)?;
    let mut values = BTreeMap::new();
    let mut step = None;
    for k in &a.order {
        let v = &a.kv[k];
        match k.as_str() {
            "step" => step = Some(num(v, "step")? as i64),
            "unit" => {}
            _ => {
                values.insert(k.clone(), Num(num(v, k)?));
            }
        }
    }
    if values.is_empty() {
        return Err("metric needs at least one name=value".into());
    }
    Ok(Directive::Metric { values, step })
}

fn expect(s: &str) -> Result<Directive, String> {
    let a = args(s)?;
    let v =
        a.kv.get("silence")
            .ok_or("expect needs silence=<duration>|default")?;
    let silence_ms = if v == "default" {
        None
    } else {
        Some(parse_duration_ms(v).ok_or_else(|| format!("bad duration '{v}'"))?)
    };
    Ok(Directive::Expect { silence_ms })
}

/// `500ms`, `90s`, `20m`, `1h`, `1h30m`, or a bare number of seconds.
pub fn parse_duration_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(secs) = s.parse::<f64>() {
        return Some((secs * 1000.0) as i64);
    }
    let mut total = 0f64;
    let mut num = String::new();
    let mut chars = s.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let v: f64 = num.parse().ok()?;
        num.clear();
        let mult = match c {
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                1.0
            }
            'h' => 3_600_000.0,
            'm' => 60_000.0,
            's' => 1000.0,
            'd' => 86_400_000.0,
            _ => return None,
        };
        total += v * mult;
        any = true;
    }
    if !num.is_empty() || !any {
        return None;
    }
    Some(total as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Directive {
        parse_line(s).expect("protocol line").expect("valid")
    }

    #[test]
    fn not_protocol() {
        assert!(parse_line("hello").is_none());
        assert!(parse_line("::unknownverb x").is_none());
        assert!(parse_line("C::D").is_none());
    }

    #[test]
    fn steps() {
        assert_eq!(
            p(r#"::step-begin id=train name="Train model""#),
            Directive::StepBegin {
                id: "train".into(),
                name: Some("Train model".into()),
                parent: None
            }
        );
        assert_eq!(
            p(r#"::step-begin compile "Compile firmware""#),
            Directive::StepBegin {
                id: "compile".into(),
                name: Some("Compile firmware".into()),
                parent: None
            }
        );
        assert_eq!(
            p("::step-begin id=e3 parent=train"),
            Directive::StepBegin {
                id: "e3".into(),
                name: None,
                parent: Some("train".into())
            }
        );
        assert_eq!(
            p("::step-end compile failed"),
            Directive::StepEnd {
                id: Some("compile".into()),
                status: StepStatus::Failed
            }
        );
        assert_eq!(
            p("::step-end"),
            Directive::StepEnd {
                id: None,
                status: StepStatus::Ok
            }
        );
        assert_eq!(
            p("  ::trun::step-end id=x status=skipped"),
            Directive::StepEnd {
                id: Some("x".into()),
                status: StepStatus::Skipped
            }
        );
    }

    #[test]
    fn progress_forms() {
        let prog = |d: Directive| match d {
            Directive::Progress {
                id,
                current,
                total,
                unit,
            } => (id, current, total, unit),
            o => panic!("{o:?}"),
        };
        assert_eq!(
            prog(p("::progress id=train current=12 total=50 unit=epoch")),
            (Some("train".into()), 12.0, Some(50.0), Some("epoch".into()))
        );
        assert_eq!(
            prog(p("::progress train 12/50")),
            (Some("train".into()), 12.0, Some(50.0), None)
        );
        assert_eq!(
            prog(p("::progress epoch 12 50")),
            (Some("epoch".into()), 12.0, Some(50.0), None)
        );
        assert_eq!(prog(p("::progress 0.42")), (None, 0.42, Some(1.0), None));
        assert_eq!(prog(p("::progress 3/9")), (None, 3.0, Some(9.0), None));
        assert!(parse_line("::progress train").unwrap().is_err());
    }

    #[test]
    fn metrics() {
        match p("::metric loss=0.3121 val_loss=0.402 lr=3e-4 step=1200 g=nan") {
            Directive::Metric { values, step } => {
                assert_eq!(step, Some(1200));
                assert_eq!(values["lr"], Num(3e-4));
                assert!(values["g"].0.is_nan());
                assert_eq!(values.len(), 4);
            }
            o => panic!("{o:?}"),
        }
        assert!(parse_line("::metric loss=abc").unwrap().is_err());
    }

    #[test]
    fn misc() {
        assert_eq!(
            p("::warn disk low"),
            Directive::Log {
                level: LogLevel::Warn,
                text: "disk low".into()
            }
        );
        assert_eq!(
            p("::note switching lr"),
            Directive::Note {
                text: "switching lr".into()
            }
        );
        assert_eq!(p("::heartbeat"), Directive::Heartbeat);
        assert_eq!(
            p("::expect silence=20m"),
            Directive::Expect {
                silence_ms: Some(1_200_000)
            }
        );
        assert_eq!(
            p("::expect silence=default"),
            Directive::Expect { silence_ms: None }
        );
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration_ms("500ms"), Some(500));
        assert_eq!(parse_duration_ms("1h30m"), Some(5_400_000));
        assert_eq!(parse_duration_ms("90"), Some(90_000));
        assert_eq!(parse_duration_ms("x"), None);
        assert_eq!(parse_duration_ms("5q"), None);
    }
}
