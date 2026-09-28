//! Process supervision: spawn a command in its own process group (Unix) or Job
//! Object (Windows), turn its output into lines, and kill the whole tree on cancel.

mod lines;
mod process;

pub use lines::{Line, LineSplitter, Segment, Terminator, pump};
pub use process::{ExitStatusInfo, Killer, SpawnSpec, Supervised, spawn};
