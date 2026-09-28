//! Writes run output to the terminal, reproducing provisional (`\r`) lines as
//! in-place redraws on a TTY and dropping them when output is captured.

use std::io::{IsTerminal, Write};
use trun_proto::{Event, EventKind, Stream};

pub struct Printer {
    tty_out: bool,
    tty_err: bool,
    show_seq: bool,
    provisional_out: bool,
    provisional_err: bool,
}

impl Printer {
    pub fn new(show_seq: bool) -> Self {
        Printer {
            tty_out: std::io::stdout().is_terminal(),
            tty_err: std::io::stderr().is_terminal(),
            show_seq,
            provisional_out: false,
            provisional_err: false,
        }
    }

    pub fn print(&mut self, ev: &Event) {
        if let EventKind::Output { stream, text, cr } = &ev.kind {
            self.print_line(*stream, text, *cr, ev.seq);
        }
    }

    pub fn print_line(&mut self, stream: Stream, text: &str, cr: bool, seq: u64) {
        let (tty, provisional) = match stream {
            Stream::Stdout => (self.tty_out, &mut self.provisional_out),
            Stream::Stderr => (self.tty_err, &mut self.provisional_err),
        };
        if cr && !tty {
            // Captured output (files, pipes, agents): only permanent lines.
            return;
        }
        let prefix = if self.show_seq {
            format!("[{seq}] ")
        } else {
            String::new()
        };
        let mut out: Box<dyn Write> = match stream {
            Stream::Stdout => Box::new(std::io::stdout().lock()),
            Stream::Stderr => Box::new(std::io::stderr().lock()),
        };
        let redraw = if *provisional { "\r\x1b[2K" } else { "" };
        let _ = if cr {
            write!(out, "{redraw}{prefix}{text}")
        } else {
            writeln!(out, "{redraw}{prefix}{text}")
        };
        let _ = out.flush();
        *provisional = cr;
    }

    /// End any in-place line so following output starts on a fresh line.
    pub fn finish_provisional(&mut self) {
        if std::mem::take(&mut self.provisional_out) {
            println!();
        }
        if std::mem::take(&mut self.provisional_err) {
            eprintln!();
        }
    }
}
