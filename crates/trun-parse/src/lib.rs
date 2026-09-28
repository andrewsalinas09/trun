//! Output parsing. M1 ships the failure diagnosis pass; the `::` progress protocol
//! and built-in parsers (tqdm, pytest, cargo, ...) land in M2.

pub mod diagnosis;

pub use diagnosis::{Diagnoser, ExitInfo};

/// Remove ANSI escape sequences (colors, cursor movement, OSC titles) for matching.
pub fn strip_ansi(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.contains('\x1b') {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: ESC [ params... final byte in @..~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... terminated by BEL or ESC \
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-char escapes (ESC c, ESC 7, ...): drop both.
            _ => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_ansi() {
        assert_eq!(super::strip_ansi("\x1b[31merror\x1b[0m: x"), "error: x");
        assert_eq!(super::strip_ansi("\x1b]0;title\x07ok"), "ok");
        assert_eq!(super::strip_ansi("plain"), "plain");
    }
}
