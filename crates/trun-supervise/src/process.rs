//! Spawning and killing supervised processes.
//!
//! * Unix: the child leads a new process group (pipes) or session (pty), so
//!   signals reach the whole tree.
//! * Windows: the child is placed in a Job Object, so the whole tree can be
//!   terminated; with pipes it gets a hidden console (`CREATE_NO_WINDOW`) so
//!   console programs behave normally without flashing a window.
//!
//! Both modes produce the same [`Started`]: readers for output, a [`Killer`], and
//! a channel that yields the exit status.

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::AsyncRead;
use tokio::process::Command;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Default)]
pub struct SpawnSpec {
    pub cmd: Vec<String>,
    pub cwd: PathBuf,
    /// Extra environment on top of the inherited one.
    pub env: Vec<(String, String)>,
    /// Run under a pseudo-terminal: one merged output stream, programs see a TTY.
    pub pty: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExitStatusInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitStatusInfo {
    pub fn success(&self) -> bool {
        self.code == Some(0) && self.signal.is_none()
    }

    /// Conventional shell exit code for this status (128+signal for signals).
    pub fn shell_code(&self) -> i32 {
        match (self.code, self.signal) {
            (Some(c), _) => c,
            (None, Some(s)) => 128 + s,
            _ => 1,
        }
    }
}

pub type BoxRead = Pin<Box<dyn AsyncRead + Send>>;

/// A started process.
pub struct Started {
    pub pid: u32,
    pub killer: Killer,
    /// stdout, or the merged terminal output in pty mode.
    pub stdout: BoxRead,
    /// `None` in pty mode.
    pub stderr: Option<BoxRead>,
    /// Resolves once when the process exits.
    pub exit: oneshot::Receiver<io::Result<ExitStatusInfo>>,
}

pub fn start(spec: &SpawnSpec) -> io::Result<Started> {
    let (program, args) = spec
        .cmd
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let resolved = resolve_program(program, &spec.cwd)?;
    if spec.pty {
        crate::pty::start(spec, resolved, args)
    } else {
        start_piped(spec, resolved, args)
    }
}

fn start_piped(spec: &SpawnSpec, resolved: PathBuf, args: &[String]) -> io::Result<Started> {
    let mut cmd = Command::new(&resolved);
    cmd.args(args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    let mut child = cmd.spawn()?;
    let pid = child.id().unwrap_or(0);
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let killer = Killer::for_pid(pid);

    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let r = child.wait().await.map(|status| {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                ExitStatusInfo {
                    code: status.code(),
                    signal: status.signal(),
                }
            }
            #[cfg(not(unix))]
            {
                ExitStatusInfo {
                    code: status.code(),
                    signal: None,
                }
            }
        });
        let _ = tx.send(r);
    });

    Ok(Started {
        pid,
        killer,
        stdout: Box::pin(stdout),
        stderr: Some(Box::pin(stderr)),
        exit: rx,
    })
}

/// Resolve the program via PATH (and PATHEXT on Windows, so `npm` finds `npm.cmd`).
fn resolve_program(program: &str, cwd: &std::path::Path) -> io::Result<PathBuf> {
    let p = std::path::Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        return Ok(full);
    }
    which::which_in(program, std::env::var_os("PATH"), cwd).map_err(|_| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("program not found: {program}"),
        )
    })
}

/// Cheap, clonable handle for terminating a supervised process tree.
#[derive(Clone)]
pub struct Killer {
    pid: u32,
    #[cfg(windows)]
    job: Option<std::sync::Arc<win::Job>>,
}

impl Killer {
    /// Take control of the tree rooted at `pid`: on Windows, put it in a Job Object.
    pub(crate) fn for_pid(pid: u32) -> Self {
        #[cfg(windows)]
        {
            let job = match win::Job::new().and_then(|j| j.assign_pid(pid).map(|_| j)) {
                Ok(j) => Some(std::sync::Arc::new(j)),
                Err(e) => {
                    tracing::warn!(pid, error = %e, "could not assign process to a job object; cancel will only kill the root process");
                    None
                }
            };
            Killer { pid, job }
        }
        #[cfg(not(windows))]
        {
            Killer { pid }
        }
    }

    /// Ask the tree to stop (SIGTERM). On Windows there is no reliable graceful
    /// signal for a console-less tree, so this terminates the job.
    pub fn terminate(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGTERM);
        }
        #[cfg(windows)]
        self.kill();
    }

    /// Kill the whole tree immediately.
    pub fn kill(&self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            if let Some(job) = &self.job {
                job.terminate(1);
            } else {
                win::terminate_pid(self.pid);
            }
        }
    }
}

#[cfg(windows)]
mod win {
    use std::io;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE, TerminateProcess,
    };

    /// Owns a Job Object handle. Deliberately *not* KILL_ON_JOB_CLOSE: the hub
    /// closing the handle (e.g. on restart) must not kill user work.
    pub struct Job(HANDLE);

    // The handle is only used through thread-safe Win32 calls.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> io::Result<Self> {
            let h = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if h.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Job(h))
            }
        }

        pub fn assign_pid(&self, pid: u32) -> io::Result<()> {
            unsafe {
                let h = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if h.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let ok = AssignProcessToJobObject(self.0, h);
                let err = io::Error::last_os_error();
                CloseHandle(h);
                if ok == 0 { Err(err) } else { Ok(()) }
            }
        }

        pub fn terminate(&self, code: u32) {
            unsafe { TerminateJobObject(self.0, code) };
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn terminate_pid(pid: u32) {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }
}
