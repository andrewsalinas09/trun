//! `trun hosts`: add, list, and remove remote hosts (docs/07-remote.md).
//!
//! `add --install` copies a static trun binary to the host over the same SSH
//! command the hub will use (piped through ssh's stdin, so it works through
//! `wsl ssh`, ProxyJump and Tailscale SSH alike), then registers the host with the
//! hub, which connects in the background.

use crate::client::Client;
use crate::fmt;
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::path::PathBuf;
use std::process::{ExitCode, Stdio};
use trun_hub::Paths;
use trun_hub::remote::{explain_ssh_error, ssh_command};
use trun_proto::{HostConfig, HostState, HostStatus, now_ms};

#[derive(Subcommand)]
pub enum HostsAction {
    /// List hosts and their connection status
    Ls,
    /// Add (or update) a remote host reachable over SSH
    Add {
        /// Short name used with `trun run --host NAME`
        name: String,
        /// SSH destination (an ~/.ssh/config alias or user@host); default: NAME
        #[arg(long)]
        target: Option<String>,
        /// SSH client command, e.g. "wsl ssh" when your SSH setup lives in WSL
        #[arg(long, default_value = "ssh")]
        ssh: String,
        /// Path of trun on the host (relative paths are from its home)
        #[arg(long, default_value = ".local/bin/trun")]
        trun_path: String,
        /// Copy a trun binary for the host's platform to it first
        #[arg(long)]
        install: bool,
        /// The binary to install (default: $TRUN_HOME/dist/trun-<target-triple>)
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    /// Remove a host (its runs keep their history)
    Rm { name: String },
}

pub async fn run(paths: &Paths, action: HostsAction) -> Result<ExitCode> {
    match action {
        HostsAction::Ls => list(paths).await,
        HostsAction::Add {
            name,
            target,
            ssh,
            trun_path,
            install,
            binary,
        } => {
            let config = HostConfig {
                target: target.unwrap_or_else(|| name.clone()),
                name,
                ssh: ssh.split_whitespace().map(str::to_string).collect(),
                trun_path,
            };
            if install {
                install_binary(paths, &config, binary).await?;
            }
            let c = Client::connect(paths, true).await?;
            c.post_status("/hosts", &config).await?;
            println!(
                "added host '{}' (ssh: {} {}); connecting in the background…",
                config.name,
                config.ssh.join(" "),
                config.target
            );
            // Show the first connection attempt's outcome.
            for _ in 0..90 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let hosts: Vec<HostState> = c.get("/hosts").await?;
                if let Some(h) = hosts.iter().find(|h| h.config.name == config.name)
                    && h.status != HostStatus::Connecting
                {
                    print_host(h);
                    return Ok(if h.status == HostStatus::Online {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::from(1)
                    });
                }
            }
            println!("still connecting; check `trun hosts`");
            Ok(ExitCode::SUCCESS)
        }
        HostsAction::Rm { name } => {
            let c = Client::connect(paths, false).await?;
            c.delete(&format!("/hosts/{}", crate::enc(&name))).await?;
            println!("removed host '{name}'");
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn list(paths: &Paths) -> Result<ExitCode> {
    let c = Client::connect(paths, false).await?;
    let hosts: Vec<HostState> = c.get("/hosts").await?;
    if hosts.is_empty() {
        println!("no hosts; add one with `trun hosts add NAME [--ssh \"wsl ssh\"] [--install]`");
        return Ok(ExitCode::SUCCESS);
    }
    for h in &hosts {
        print_host(h);
    }
    Ok(ExitCode::SUCCESS)
}

fn print_host(h: &HostState) {
    let (sym, word) = match h.status {
        HostStatus::Online => ("●", "online"),
        HostStatus::Connecting => ("…", "connecting"),
        HostStatus::Offline => ("○", "offline"),
    };
    let info = h
        .info
        .as_ref()
        .map(|i| format!(" · {} {}/{} · trun {}", i.hostname, i.os, i.arch, i.version))
        .unwrap_or_default();
    let seen = h
        .last_seen
        .map(|t| format!(" · seen {}", fmt::ago(t)))
        .unwrap_or_default();
    let clock = match h.clock_offset_ms {
        Some(o) if o.abs() >= 1000 => {
            format!(
                " · clock {} {} (adjusted)",
                fmt::duration(o.abs()),
                if o > 0 { "ahead" } else { "behind" }
            )
        }
        _ => String::new(),
    };
    let seen = format!("{seen}{clock}");
    println!(
        "{sym} {:<12} {word}{info}{seen} · {} active · via {} {}",
        h.config.name,
        h.active_runs,
        h.config.ssh.join(" "),
        h.config.target
    );
    if let Some(e) = &h.error
        && h.status != HostStatus::Online
    {
        println!("    {e}");
    }
    let _ = now_ms;
}

/// `uname -sm` → Rust target triple of a static trun build.
fn triple_for(uname: &str) -> Result<&'static str> {
    let u = uname.trim().to_ascii_lowercase();
    Ok(
        match (u.split_whitespace().next(), u.split_whitespace().nth(1)) {
            (Some("linux"), Some("aarch64" | "arm64")) => "aarch64-unknown-linux-musl",
            (Some("linux"), Some("x86_64" | "amd64")) => "x86_64-unknown-linux-musl",
            (Some("darwin"), Some("arm64")) => "aarch64-apple-darwin",
            (Some("darwin"), Some("x86_64")) => "x86_64-apple-darwin",
            _ => bail!(
                "unsupported remote platform '{}' for --install (install trun there yourself and use --trun-path)",
                uname.trim()
            ),
        },
    )
}

async fn install_binary(paths: &Paths, config: &HostConfig, binary: Option<PathBuf>) -> Result<()> {
    // 1. Which platform?
    let mut probe = ssh_command(config, &[&config.target, "uname", "-sm"]);
    probe.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = probe.output().await.context("running ssh")?;
    if !out.status.success() {
        bail!(
            "{}",
            explain_ssh_error(config, &String::from_utf8_lossy(&out.stderr))
        );
    }
    let uname = String::from_utf8_lossy(&out.stdout).to_string();
    let triple = triple_for(&uname)?;
    let bin = match binary {
        Some(b) => b,
        None => paths.home.join("dist").join(format!("trun-{triple}")),
    };
    if !bin.is_file() {
        bail!(
            "no trun binary for {triple} at {}\n  build one (e.g. `cargo zigbuild --release -p trun --target {triple}`) and pass --binary, or copy it there",
            bin.display()
        );
    }
    let bytes = std::fs::read(&bin).with_context(|| format!("reading {}", bin.display()))?;
    println!(
        "installing {} ({} MB, {triple}) to {}:{} …",
        bin.display(),
        bytes.len() / 1_000_000,
        config.target,
        config.trun_path
    );

    // 2. Stream it over ssh stdin; replace atomically (a running daemon keeps its old
    //    inode; the next `hub ensure` restarts it on the new build when idle).
    let dest = &config.trun_path;
    let script = format!(
        "set -e; d=$(dirname '{dest}'); mkdir -p \"$d\"; cat > '{dest}.new'; chmod +x '{dest}.new'; mv -f '{dest}.new' '{dest}'; '{dest}' --version"
    );
    let mut cmd = ssh_command(config, &[&config.target, &script]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("running ssh")?;
    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(&bytes)
            .await
            .context("sending the binary")?;
        stdin.shutdown().await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!(
            "install failed: {}",
            explain_ssh_error(config, &String::from_utf8_lossy(&out.stderr))
        );
    }
    println!("  {}", String::from_utf8_lossy(&out.stdout).trim());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triples() {
        assert_eq!(
            triple_for("Linux aarch64\n").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            triple_for("Linux x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(triple_for("Darwin arm64").unwrap(), "aarch64-apple-darwin");
        assert!(triple_for("MINGW64_NT x86_64").is_err());
    }
}
