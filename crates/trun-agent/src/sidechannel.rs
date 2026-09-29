//! The per-run `TRUN_EVENTS` side channel (docs/04-progress-protocol.md).
//!
//! Programs that want clean stdout, or emit high-frequency metrics, write
//! newline-delimited messages here instead: JSON [`Directive`] objects, or `::`
//! protocol lines (so `trun emit` and shell scripts can use the same endpoint).
//!
//! * Unix: a socket in a per-user 0700 directory under the temp dir.
//! * Windows: the named pipe `\\.\pipe\trun-<run id>`. The default pipe ACL only
//!   lets the creating user (and admins) open it for writing, and remote clients
//!   are rejected.

use std::io;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use trun_proto::Directive;

/// Longest accepted message; longer lines are reported and skipped.
const MAX_LINE: usize = 256 * 1024;

#[derive(Debug)]
pub enum SideMsg {
    Directive(Directive),
    Invalid(String),
}

/// Listening endpoint; stops listening (and removes the socket file) on drop.
pub struct SideChannel {
    pub endpoint: String,
    task: JoinHandle<()>,
    #[cfg(unix)]
    path: std::path::PathBuf,
}

impl Drop for SideChannel {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

fn parse(line: &str) -> Option<SideMsg> {
    let t = line.trim();
    if t.is_empty() {
        return None;
    }
    if t.starts_with("::") {
        return Some(match trun_parse::protocol::parse_line(t) {
            Some(Ok(d)) => SideMsg::Directive(d),
            Some(Err(e)) => SideMsg::Invalid(format!("{e}: {t}")),
            None => SideMsg::Invalid(format!("unknown protocol verb: {t}")),
        });
    }
    Some(match serde_json::from_str::<Directive>(t) {
        Ok(d) => SideMsg::Directive(d),
        Err(e) => SideMsg::Invalid(format!("{e}: {}", clip(t))),
    })
}

fn clip(s: &str) -> String {
    if s.len() <= 200 {
        s.to_string()
    } else {
        format!("{}…", &s[..s.floor_char_boundary(200)])
    }
}

async fn read_conn<R: AsyncRead + Unpin>(conn: R, tx: mpsc::Sender<SideMsg>) {
    let mut reader = BufReader::new(conn);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let msg = if buf.len() > MAX_LINE {
            Some(SideMsg::Invalid(format!(
                "message too long ({} bytes)",
                buf.len()
            )))
        } else {
            parse(&String::from_utf8_lossy(&buf))
        };
        if let Some(m) = msg
            && tx.send(m).await.is_err()
        {
            return;
        }
    }
}

#[cfg(unix)]
pub fn open(run_id: &str, tx: mpsc::Sender<SideMsg>) -> io::Result<SideChannel> {
    use std::os::unix::fs::PermissionsExt;
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("trun-{uid}"));
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = dir.join(format!("{run_id}.sock"));
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)?;
    let task = tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            tokio::spawn(read_conn(conn, tx.clone()));
        }
    });
    Ok(SideChannel {
        endpoint: path.to_string_lossy().into_owned(),
        task,
        path,
    })
}

#[cfg(windows)]
pub fn open(run_id: &str, tx: mpsc::Sender<SideMsg>) -> io::Result<SideChannel> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = format!(r"\\.\pipe\trun-{run_id}");
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .access_outbound(false)
        .create(&name)?;
    let pipe_name = name.clone();
    let task = tokio::spawn(async move {
        loop {
            if server.connect().await.is_err() {
                return;
            }
            // Create the next instance before serving this one so writers never
            // find the pipe busy.
            let next = match ServerOptions::new()
                .reject_remote_clients(true)
                .access_outbound(false)
                .create(&pipe_name)
            {
                Ok(n) => n,
                Err(_) => return,
            };
            let conn = std::mem::replace(&mut server, next);
            tokio::spawn(read_conn(conn, tx.clone()));
        }
    });
    Ok(SideChannel {
        endpoint: name,
        task,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_and_protocol_lines() {
        assert!(matches!(
            parse(r#"{"kind":"heartbeat"}"#),
            Some(SideMsg::Directive(Directive::Heartbeat))
        ));
        assert!(matches!(
            parse("::metric loss=1"),
            Some(SideMsg::Directive(Directive::Metric { .. }))
        ));
        assert!(matches!(parse("{bad json"), Some(SideMsg::Invalid(_))));
        assert!(matches!(parse("::nope x"), Some(SideMsg::Invalid(_))));
        assert!(parse("   ").is_none());
    }

    #[tokio::test]
    async fn end_to_end_over_endpoint() {
        let (tx, mut rx) = mpsc::channel(16);
        let id = format!("TEST{}", std::process::id());
        let ch = open(&id, tx).unwrap();
        let endpoint = ch.endpoint.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            #[cfg(unix)]
            let mut w = std::os::unix::net::UnixStream::connect(&endpoint).unwrap();
            #[cfg(windows)]
            let mut w = std::fs::OpenOptions::new()
                .write(true)
                .open(&endpoint)
                .unwrap();
            w.write_all(b"{\"kind\":\"note\",\"text\":\"hi\"}\n::progress t 1/2\n")
                .unwrap();
        })
        .await
        .unwrap();
        let a = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let b = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(a, SideMsg::Directive(Directive::Note { .. })));
        assert!(matches!(b, SideMsg::Directive(Directive::Progress { .. })));
        drop(ch);
    }
}
