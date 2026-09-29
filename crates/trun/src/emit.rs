//! `trun emit`: report structure from inside a run (shell scripts, Makefiles).
//!
//! The command becomes a `::` protocol line, validated here, then written to the
//! run's `TRUN_EVENTS` side channel. Outside trun (no `TRUN_EVENTS`), the line is
//! printed to stdout instead, which is harmless and still parsed if the output is
//! later captured by trun.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::process::ExitCode;

fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains(char::is_whitespace) || arg.contains('"') {
        // Quote the value part of key=value, or the whole positional token.
        let (prefix, value) = match arg.split_once('=') {
            Some((k, v)) if !k.is_empty() && !k.contains(char::is_whitespace) => {
                (format!("{k}="), v)
            }
            _ => (String::new(), arg),
        };
        format!(
            "{prefix}\"{}\"",
            value.replace('\\', "\\\\").replace('"', "\\\"")
        )
    } else {
        arg.to_string()
    }
}

/// Free-text verbs take the rest of the line verbatim.
const TEXT_VERBS: &[&str] = &["note", "warn", "warning", "error", "info"];

pub fn build_line(verb: &str, args: &[String]) -> Result<String> {
    let rest = if TEXT_VERBS.contains(&verb) {
        args.join(" ")
    } else {
        args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
    };
    let line = if rest.is_empty() {
        format!("::{verb}")
    } else {
        format!("::{verb} {rest}")
    };
    match trun_parse::protocol::parse_line(&line) {
        Some(Ok(_)) => Ok(line),
        Some(Err(e)) => bail!("{e}"),
        None => bail!(
            "unknown verb '{verb}' (expected step-begin, step-end, progress, metric, note, warn, error, info, heartbeat, expect)"
        ),
    }
}

pub fn emit(verb: &str, args: &[String]) -> Result<ExitCode> {
    let line = build_line(verb, args)?;
    let Some(endpoint) = std::env::var_os("TRUN_EVENTS") else {
        println!("{line}");
        return Ok(ExitCode::SUCCESS);
    };
    let endpoint = endpoint.to_string_lossy().into_owned();
    write_endpoint(&endpoint, format!("{line}\n").as_bytes())
        .with_context(|| format!("writing to TRUN_EVENTS ({endpoint})"))?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(unix)]
fn write_endpoint(endpoint: &str, bytes: &[u8]) -> std::io::Result<()> {
    let mut s = std::os::unix::net::UnixStream::connect(endpoint)?;
    s.write_all(bytes)
}

#[cfg(windows)]
fn write_endpoint(endpoint: &str, bytes: &[u8]) -> std::io::Result<()> {
    // All pipe instances can be momentarily busy; retry briefly (ERROR_PIPE_BUSY = 231).
    let mut last = None;
    for _ in 0..50 {
        match std::fs::OpenOptions::new().write(true).open(endpoint) {
            Ok(mut f) => return f.write_all(bytes),
            Err(e) if e.raw_os_error() == Some(231) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("pipe busy")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn builds_valid_lines() {
        assert_eq!(
            build_line("step-begin", &s(&["compile", "Compile firmware"])).unwrap(),
            "::step-begin compile \"Compile firmware\""
        );
        assert_eq!(
            build_line("progress", &s(&["epoch", "12", "50"])).unwrap(),
            "::progress epoch 12 50"
        );
        assert_eq!(
            build_line("metric", &s(&["loss=0.31", "step=5"])).unwrap(),
            "::metric loss=0.31 step=5"
        );
        assert_eq!(
            build_line("note", &s(&["switching", "to", "plan", "B"])).unwrap(),
            "::note switching to plan B"
        );
        assert_eq!(
            build_line("step-begin", &s(&["x", "name=A \"q\""])).unwrap(),
            "::step-begin x name=\"A \\\"q\\\"\""
        );
        assert!(build_line("metric", &s(&["loss=abc"])).is_err());
        assert!(build_line("bogus", &[]).is_err());
    }
}
