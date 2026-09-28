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
    let exe = std::env::current_exe().context("locating the trun executable")?;

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
