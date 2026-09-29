//! Pseudo-terminal mode (`trun run --pty`).
//!
//! Some programs only show progress bars, colors, or line-buffered output when
//! attached to a terminal. A pty merges stdout and stderr into one stream.
//!
//! * Unix: openpty via portable-pty (the stream is the program's own bytes).
//! * Windows: our own ConPTY driver ([`crate::conpty`]), for control over flags.
//!
//! In both cases trun *is* the terminal, so it answers terminal queries itself.

#[cfg(unix)]
pub use unix::start;

#[cfg(windows)]
pub use crate::conpty::start;

#[cfg(unix)]
mod unix {
    use crate::process::{ExitStatusInfo, Killer, SpawnSpec, Started};
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{self, Read, Write};
    use std::path::PathBuf;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::io::StreamReader;

    /// Wide enough that tools don't wrap progress bars.
    const COLS: u16 = 200;
    const ROWS: u16 = 50;

    pub fn start(spec: &SpawnSpec, resolved: PathBuf, args: &[String]) -> io::Result<Started> {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;

        let mut cmd = CommandBuilder::new(resolved);
        cmd.args(args);
        cmd.cwd(&spec.cwd);
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        if std::env::var_os("TERM").is_none() {
            cmd.env("TERM", "xterm-256color");
        }

        let mut child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        // The slave end must close in this process, or the reader never sees EOF.
        drop(pair.slave);
        let pid = child.process_id().unwrap_or(0);
        let killer = Killer::for_pid(pid);

        let mut reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let mut writer = pair.master.take_writer().map_err(io::Error::other)?;
        let (btx, brx) = mpsc::channel::<io::Result<bytes::Bytes>>(64);
        std::thread::Builder::new()
            .name(format!("pty-read-{pid}"))
            .spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let (out, replies) = super::answer_queries(&buf[..n]);
                            if !replies.is_empty() {
                                let _ = writer.write_all(&replies).and_then(|_| writer.flush());
                            }
                            if !out.is_empty()
                                && btx.blocking_send(Ok(bytes::Bytes::from(out))).is_err()
                            {
                                break;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break, // EIO when the child side closes
                    }
                }
            })?;

        let (etx, erx) = oneshot::channel();
        let master = pair.master;
        std::thread::Builder::new()
            .name(format!("pty-wait-{pid}"))
            .spawn(move || {
                let status = child.wait().map(|s| ExitStatusInfo {
                    code: Some(s.exit_code() as i32),
                    signal: None,
                });
                std::thread::sleep(std::time::Duration::from_millis(100));
                drop(master);
                let _ = etx.send(status);
            })?;

        let stream = tokio_stream::wrappers::ReceiverStream::new(brx);
        Ok(Started {
            pid,
            killer,
            stdout: Box::pin(StreamReader::new(stream)),
            stderr: None,
            exit: erx,
        })
    }
}

/// Terminal queries a program (or the pseudo-console) may block on, and our answers.
const QUERIES: &[(&[u8], &[u8])] = &[
    (b"\x1b[6n", b"\x1b[1;1R"), // DSR: cursor position report
    (b"\x1b[5n", b"\x1b[0n"),   // DSR: device status "OK"
    (b"\x1b[c", b"\x1b[?1;0c"), // DA1: primary device attributes (VT100)
    (b"\x1b[0c", b"\x1b[?1;0c"),
];

/// Remove terminal queries from output and collect the replies to send back.
pub(crate) fn answer_queries(chunk: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut out = Vec::with_capacity(chunk.len());
    let mut replies = Vec::new();
    let mut i = 0;
    'outer: while i < chunk.len() {
        if chunk[i] == 0x1b {
            for (q, a) in QUERIES {
                if chunk[i..].starts_with(q) {
                    replies.extend_from_slice(a);
                    i += q.len();
                    continue 'outer;
                }
            }
        }
        out.push(chunk[i]);
        i += 1;
    }
    (out, replies)
}

#[cfg(test)]
mod tests {
    use super::answer_queries;

    #[test]
    fn answers_and_strips_queries() {
        let (out, replies) = answer_queries(b"\x1b[6nhello\x1b[31mred\x1b[5n");
        assert_eq!(out, b"hello\x1b[31mred");
        assert_eq!(replies, b"\x1b[1;1R\x1b[0n");
        let (out, replies) = answer_queries(b"plain");
        assert_eq!((out.as_slice(), replies.len()), (&b"plain"[..], 0));
    }
}
