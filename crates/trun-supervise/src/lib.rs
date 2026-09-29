//! Process supervision: spawn a command in its own process group (Unix) or Job
//! Object (Windows), optionally under a pseudo-terminal, turn its output into
//! lines, and kill the whole tree on cancel.

#[cfg(windows)]
mod conpty;
mod lines;
mod process;
mod pty;

pub use lines::{Line, LineSplitter, Segment, Terminator, pump};
pub use process::{BoxRead, ExitStatusInfo, Killer, SpawnSpec, Started, start};
