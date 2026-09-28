//! Turn a raw byte stream into lines, handling carriage-return redraws.
//!
//! Progress bars (tqdm, cargo, curl, ...) redraw one line with `\r` many times per
//! second. Storing every redraw would bury the log, and waiting for `\n` would hide
//! progress until the bar finishes. So a `\r`-terminated segment becomes a
//! *provisional* line (`cr = true`) that the next line on the same stream replaces,
//! emitted at most every [`CR_EMIT_INTERVAL`]. A partial line with no terminator
//! (a prompt, a bar without `\r`) is also surfaced provisionally after
//! [`PARTIAL_IDLE`] so a quiet process never looks emptier than it is.

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::time::Instant;
use trun_proto::Stream;

pub const CR_EMIT_INTERVAL: Duration = Duration::from_millis(500);
pub const PARTIAL_IDLE: Duration = Duration::from_millis(1000);
pub const MAX_LINE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub stream: Stream,
    pub text: String,
    /// Provisional: replaced by the next line on the same stream.
    pub cr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    Newline,
    CarriageReturn,
    /// Cut because the line exceeded [`MAX_LINE_BYTES`].
    Overflow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub term: Terminator,
}

/// Pure byte → segment splitter (no timing), so it is easy to test.
#[derive(Debug, Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
}

impl LineSplitter {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Segment> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            match self.buf[i] {
                b'\n' => {
                    out.push(seg(&self.buf[start..i], Terminator::Newline));
                    i += 1;
                    start = i;
                }
                b'\r' => {
                    if i + 1 == self.buf.len() {
                        // Can't tell `\r\n` from a bare `\r` yet; wait for more bytes.
                        break;
                    }
                    if self.buf[i + 1] == b'\n' {
                        out.push(seg(&self.buf[start..i], Terminator::Newline));
                        i += 2;
                    } else {
                        out.push(seg(&self.buf[start..i], Terminator::CarriageReturn));
                        i += 1;
                    }
                    start = i;
                }
                _ => {
                    i += 1;
                    if i - start >= MAX_LINE_BYTES {
                        out.push(seg(&self.buf[start..i], Terminator::Overflow));
                        start = i;
                    }
                }
            }
        }
        self.buf.drain(..start);
        out
    }

    /// Bytes received after the last terminator (a partial line).
    pub fn partial(&self) -> Option<String> {
        let b = self.buf.strip_suffix(b"\r").unwrap_or(&self.buf);
        if b.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(b).into_owned())
        }
    }

    pub fn take_partial(&mut self) -> Option<String> {
        let p = self.partial();
        self.buf.clear();
        p
    }
}

fn seg(bytes: &[u8], term: Terminator) -> Segment {
    Segment {
        text: String::from_utf8_lossy(bytes).into_owned(),
        term,
    }
}

/// Read `reader` to EOF, sending [`Line`]s to `tx`. Returns when the stream closes
/// or the receiver is dropped.
pub async fn pump<R: AsyncRead + Unpin>(mut reader: R, stream: Stream, tx: mpsc::Sender<Line>) {
    let mut splitter = LineSplitter::default();
    let mut buf = vec![0u8; 8192];
    // Latest `\r` segment not yet emitted, and when we last emitted a provisional line.
    let mut pending_cr: Option<String> = None;
    let mut last_cr_emit: Option<Instant> = None;
    // Whether the current provisional line on screen came from a partial/cr segment.
    let mut last_partial_emitted: Option<String> = None;
    let mut last_data = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let send = |text: String, cr: bool| {
        let tx = tx.clone();
        async move { tx.send(Line { stream, text, cr }).await.is_ok() }
    };

    loop {
        tokio::select! {
            r = reader.read(&mut buf) => {
                let n = match r { Ok(0) | Err(_) => break, Ok(n) => n };
                last_data = Instant::now();
                for s in splitter.feed(&buf[..n]) {
                    match s.term {
                        Terminator::Newline | Terminator::Overflow => {
                            pending_cr = None;
                            last_partial_emitted = None;
                            if !send(s.text, false).await { return; }
                        }
                        Terminator::CarriageReturn => {
                            if s.text.is_empty() { continue; }
                            let due = last_cr_emit.is_none_or(|t| t.elapsed() >= CR_EMIT_INTERVAL);
                            if due {
                                last_cr_emit = Some(Instant::now());
                                pending_cr = None;
                                if !send(s.text, true).await { return; }
                            } else {
                                pending_cr = Some(s.text);
                            }
                        }
                    }
                }
            }
            _ = tick.tick() => {
                if let Some(text) = pending_cr.take() {
                    if last_cr_emit.is_none_or(|t| t.elapsed() >= CR_EMIT_INTERVAL) {
                        last_cr_emit = Some(Instant::now());
                        if !send(text, true).await { return; }
                    } else {
                        pending_cr = Some(text);
                    }
                } else if last_data.elapsed() >= PARTIAL_IDLE
                    && let Some(p) = splitter.partial()
                    && last_partial_emitted.as_deref() != Some(p.as_str())
                {
                    last_partial_emitted = Some(p.clone());
                    if !send(p, true).await { return; }
                }
            }
        }
    }

    // EOF: whatever was last on screen becomes a permanent line.
    if let Some(p) = splitter.take_partial() {
        let _ = send(p, false).await;
    } else if let Some(text) = pending_cr {
        let _ = send(text, false).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(v: &[Segment]) -> Vec<(&str, Terminator)> {
        v.iter().map(|s| (s.text.as_str(), s.term)).collect()
    }

    #[test]
    fn splits_newlines_and_crlf() {
        let mut s = LineSplitter::default();
        let out = s.feed(b"a\nb\r\nc");
        assert_eq!(
            texts(&out),
            vec![("a", Terminator::Newline), ("b", Terminator::Newline)]
        );
        assert_eq!(s.partial().as_deref(), Some("c"));
    }

    #[test]
    fn trailing_cr_waits_for_next_byte() {
        let mut s = LineSplitter::default();
        assert!(s.feed(b"x\r").is_empty());
        let out = s.feed(b"\n");
        assert_eq!(texts(&out), vec![("x", Terminator::Newline)]);
        let mut s = LineSplitter::default();
        assert!(s.feed(b"10%\r").is_empty());
        let out = s.feed(b"20%\r");
        assert_eq!(texts(&out), vec![("10%", Terminator::CarriageReturn)]);
    }

    #[test]
    fn tqdm_style_sequence() {
        let mut s = LineSplitter::default();
        let out = s.feed(b"\r 0%|    |\r50%|##  |\r100%|####|\n");
        assert_eq!(
            texts(&out),
            vec![
                ("", Terminator::CarriageReturn),
                (" 0%|    |", Terminator::CarriageReturn),
                ("50%|##  |", Terminator::CarriageReturn),
                ("100%|####|", Terminator::Newline),
            ]
        );
    }

    #[test]
    fn overflow_cuts_long_lines() {
        let mut s = LineSplitter::default();
        let big = vec![b'x'; MAX_LINE_BYTES + 10];
        let out = s.feed(&big);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].term, Terminator::Overflow);
        assert_eq!(s.partial().map(|p| p.len()), Some(10));
    }

    #[tokio::test]
    async fn pump_emits_final_state_at_eof() {
        let (tx, mut rx) = mpsc::channel(64);
        let data: &[u8] = b"start\n\rstep 1\rstep 2\rdone";
        pump(data, Stream::Stdout, tx).await;
        let mut got = vec![];
        while let Some(l) = rx.recv().await {
            got.push((l.text, l.cr));
        }
        // First cr segment is emitted immediately, later ones throttled, final state permanent.
        assert_eq!(got.first(), Some(&("start".to_string(), false)));
        assert_eq!(got.last(), Some(&("done".to_string(), false)));
        assert!(got.iter().any(|(t, cr)| t == "step 1" && *cr));
    }
}
