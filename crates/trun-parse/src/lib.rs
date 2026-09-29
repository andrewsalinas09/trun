//! Output parsing: the `::` protocol, built-in parsers for common tools, and the
//! failure diagnosis pass.

pub mod builtin;
pub mod diagnosis;
pub mod protocol;

pub use diagnosis::{Diagnoser, ExitInfo};
pub use protocol::parse_duration_ms;

use builtin::LineParser;
use trun_proto::{Directive, LogLevel};

/// Result of feeding one output line.
#[derive(Debug, Default)]
pub struct Feed {
    /// A protocol line: don't show it as output.
    pub hide: bool,
    pub directives: Vec<Directive>,
}

/// All parsers for one run. Protocol lines win; built-ins run on everything else;
/// the generic `N/M` fallback only while nothing more specific reported progress.
pub struct ParserSet {
    builtins: Vec<Box<dyn LineParser>>,
    fraction: builtin::Fraction,
    structured: bool,
}

impl Default for ParserSet {
    fn default() -> Self {
        Self::new()
    }
}

impl ParserSet {
    pub fn new() -> Self {
        ParserSet {
            builtins: vec![
                Box::new(builtin::Tqdm::default()),
                Box::new(builtin::Pytest::default()),
                Box::new(builtin::Cargo::default()),
            ],
            fraction: builtin::Fraction::default(),
            structured: false,
        }
    }

    /// Feed one line of output (`cr`: provisional progress redraw).
    pub fn feed(&mut self, raw: &str, cr: bool) -> Feed {
        let clean = strip_ansi(raw);
        let mut feed = Feed::default();
        if !cr && let Some(parsed) = protocol::parse_line(&clean) {
            match parsed {
                Ok(d) => {
                    feed.hide = true;
                    self.note_structure(std::slice::from_ref(&d));
                    feed.directives.push(d);
                }
                Err(e) => feed.directives.push(Directive::Log {
                    level: LogLevel::Warn,
                    text: format!(
                        "trun: ignored malformed protocol line ({e}): {}",
                        clean.trim()
                    ),
                }),
            }
            return feed;
        }
        for p in &mut self.builtins {
            p.parse(&clean, cr, &mut feed.directives);
        }
        if feed.directives.is_empty() && !self.structured {
            self.fraction.parse(&clean, cr, &mut feed.directives);
        } else {
            let ds = std::mem::take(&mut feed.directives);
            self.note_structure(&ds);
            feed.directives = ds;
        }
        feed
    }

    /// Structured directives from outside the line stream (the side channel).
    pub fn note_external(&mut self, d: &Directive) {
        self.note_structure(std::slice::from_ref(d));
    }

    fn note_structure(&mut self, ds: &[Directive]) {
        if ds
            .iter()
            .any(|d| matches!(d, Directive::Progress { .. } | Directive::StepBegin { .. }))
        {
            self.structured = true;
        }
    }
}

/// Keep only color/style escapes (SGR, `ESC[…m`); drop cursor movement, mode
/// switches, OSC titles and other control sequences, which mean nothing in a log
/// and render as junk in the UI.
pub fn sanitize_display(s: &str) -> std::borrow::Cow<'_, str> {
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
            Some('[') => {
                let mut seq = String::from("\x1b[");
                for c in chars.by_ref() {
                    seq.push(c);
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
                if seq.ends_with('m') && !seq.contains(['?', '>', '<', '=']) {
                    out.push_str(&seq);
                }
            }
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
            _ => {}
        }
    }
    std::borrow::Cow::Owned(out)
}

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
    use super::*;

    #[test]
    fn protocol_lines_are_hidden_and_disable_fallback() {
        let mut p = ParserSet::new();
        let f = p.feed("Epoch 1/5", false);
        assert!(!f.hide);
        assert!(
            f.directives
                .iter()
                .any(|d| matches!(d, Directive::Progress { .. }))
        );

        let mut p = ParserSet::new();
        let f = p.feed("::progress train 1/3", false);
        assert!(f.hide);
        let f = p.feed("Epoch 1/5", false);
        assert!(
            f.directives.is_empty(),
            "fallback must stay off once structure is reported"
        );
    }

    #[test]
    fn malformed_protocol_line_is_shown_with_warning() {
        let mut p = ParserSet::new();
        let f = p.feed("::metric loss=abc", false);
        assert!(!f.hide);
        assert!(matches!(
            &f.directives[0],
            Directive::Log {
                level: LogLevel::Warn,
                ..
            }
        ));
    }

    #[test]
    fn colored_tqdm_line_parses() {
        let mut p = ParserSet::new();
        let f = p.feed(
            "\x1b[32mtrain\x1b[0m:  10%|#         | 1/10 [00:01<00:09, 1.00it/s]",
            true,
        );
        assert!(
            f.directives
                .iter()
                .any(|d| matches!(d, Directive::Progress { current, .. } if *current == 1.0))
        );
    }

    #[test]
    fn sanitize_keeps_only_colors() {
        assert_eq!(
            sanitize_display("\x1b[?25l\x1b]0;title\x07\x1b[32mgreen\x1b[0m\x1b[2K\x1b[1A done"),
            "\x1b[32mgreen\x1b[0m done"
        );
        assert_eq!(sanitize_display("plain"), "plain");
    }

    #[test]
    fn strips_ansi() {
        assert_eq!(super::strip_ansi("\x1b[31merror\x1b[0m: x"), "error: x");
        assert_eq!(super::strip_ansi("\x1b]0;title\x07ok"), "ok");
        assert_eq!(super::strip_ansi("plain"), "plain");
    }
}
