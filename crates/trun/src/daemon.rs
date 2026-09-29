//! Start the hub as a background process that outlives the terminal (and the AI
//! session) that triggered it.

use anyhow::{Context, Result};
use std::process::{Command, Stdio};
use trun_hub::Paths;

pub fn spawn_hub(paths: &Paths, port: u16) -> Result<()> {
    std::fs::create_dir_all(&paths.home)?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_file())
        .with_context(|| format!("opening {}", paths.log_file().display()))?;
    let exe = hub_executable(paths)?;

    let build = || -> Result<Command> {
        let mut cmd = Command::new(&exe);
        cmd.args(["hub", "start", "--foreground", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log.try_clone()?));
        Ok(cmd)
    };

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CreateProcess passes *every* inheritable handle to the child, including
        // this CLI's own stdout/stderr pipes. A long-lived hub holding those open
        // makes anything capturing the CLI's output (`$(trun …)`, `| cat`, an agent
        // harness) wait forever for EOF. The hub gets its own log handles instead.
        disinherit_std_handles();
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Break away from the caller's job object when allowed: terminals and agent
        // harnesses often kill their whole job when a command finishes.
        let mut cmd = build()?;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        if cmd.spawn().is_err() {
            let mut cmd = build()?;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn().context("starting the hub")?;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let mut cmd = build()?;
        // New session: no controlling terminal, immune to the terminal's SIGHUP.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn().context("starting the hub")?;
    }
    Ok(())
}

/// Stop this process's stdin/stdout/stderr from being inherited by children.
#[cfg(windows)]
pub fn disinherit_std_handles() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            let h = GetStdHandle(which);
            if !h.is_null() && h as isize != -1 {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn disinherit_std_handles() {}

/// The executable the background hub runs from.
///
/// Windows locks a running .exe, so a hub started straight from `trun.exe` would
/// block rebuilding or upgrading trun for as long as it runs. There the hub runs
/// from a copy in `$TRUN_HOME/bin`, keyed by size and mtime so each build gets its
/// own copy. Unix can replace a running binary, so it uses the original.
fn hub_executable(paths: &Paths) -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("locating the trun executable")?;
    if !cfg!(windows) {
        return Ok(exe);
    }
    let meta = std::fs::metadata(&exe)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bin = paths.home.join("bin");
    std::fs::create_dir_all(&bin)?;
    let copy = bin.join(format!("trun-hub-{:x}-{:x}.exe", meta.len(), mtime));
    if !copy.exists() {
        let tmp = copy.with_extension("tmp");
        std::fs::copy(&exe, &tmp).with_context(|| format!("copying trun to {}", tmp.display()))?;
        std::fs::rename(&tmp, &copy)?;
        // Old copies that are no longer running can go; running ones stay locked.
        if let Ok(rd) = std::fs::read_dir(&bin) {
            for e in rd.flatten() {
                let p = e.path();
                if p != copy
                    && p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("trun-hub-"))
                {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
    }
    Ok(copy)
}
