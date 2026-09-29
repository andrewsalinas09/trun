//! Built-in parsers that recognize common tools' output and turn it into
//! [`Directive`]s, so unmodified programs still get steps, progress and metrics.

mod cargo;
mod fraction;
mod pytest;
mod tqdm;

pub use cargo::Cargo;
pub use fraction::Fraction;
pub use pytest::Pytest;
pub use tqdm::Tqdm;

use trun_proto::Directive;

/// A stateful parser fed every (ANSI-stripped) output line of one run.
pub trait LineParser: Send {
    fn name(&self) -> &'static str;
    /// `cr`: the line is a provisional progress-bar redraw.
    fn parse(&mut self, line: &str, cr: bool, out: &mut Vec<Directive>);
}

/// Step id from a free-form label: lowercase, alphanumerics and dashes.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.trim().chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `12.3k` → 12300 (tqdm `unit_scale` suffixes).
pub fn scaled(s: &str) -> Option<f64> {
    let (num, mult) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1e3),
        'M' => (&s[..s.len() - 1], 1e6),
        'G' => (&s[..s.len() - 1], 1e9),
        'T' => (&s[..s.len() - 1], 1e12),
        'P' => (&s[..s.len() - 1], 1e15),
        _ => (s, 1.0),
    };
    num.parse::<f64>().ok().map(|v| v * mult)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Epoch 1"), "epoch-1");
        assert_eq!(slug("  Training (GPU 0): "), "training-gpu-0");
        assert_eq!(slug("::"), "");
    }

    #[test]
    fn scale() {
        assert_eq!(scaled("12.5k"), Some(12500.0));
        assert_eq!(scaled("3"), Some(3.0));
        assert_eq!(scaled("x"), None);
    }
}
