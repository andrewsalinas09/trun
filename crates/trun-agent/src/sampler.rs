//! Samples resource usage for checks: the CPU and memory of each run's process tree,
//! host CPU and memory, and free disk space where each run works.
//!
//! One background thread for the whole agent (sysinfo is synchronous). GPU
//! sampling (NVML) arrives with remote agents (M4); until then those series stay
//! empty and GPU checks don't fire.

use crate::Agent;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use sysinfo::{Disks, Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use trun_proto::now_ms;

pub const SAMPLE_EVERY: Duration = Duration::from_secs(5);

pub fn spawn(agent: Agent) {
    let _ = std::thread::Builder::new()
        .name("trun-sampler".into())
        .spawn(move || {
            let mut sys = System::new();
            let mut disks = Disks::new_with_refreshed_list();
            let mut disk_refresh = 0u32;
            loop {
                std::thread::sleep(SAMPLE_EVERY);
                let targets = agent.sample_targets();
                if targets.is_empty() {
                    continue;
                }
                sys.refresh_cpu_usage();
                sys.refresh_memory();
                sys.refresh_processes_specifics(
                    ProcessesToUpdate::All,
                    true,
                    ProcessRefreshKind::nothing().with_cpu().with_memory(),
                );
                disk_refresh += 1;
                if disk_refresh.is_multiple_of(12) {
                    disks.refresh(true); // new mounts show up within a minute
                } else {
                    disks.refresh(false);
                }

                let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
                for (pid, p) in sys.processes() {
                    if let Some(parent) = p.parent() {
                        children.entry(parent).or_default().push(*pid);
                    }
                }
                let host_cpu = sys.global_cpu_usage() as f64;
                let host_mem = if sys.total_memory() > 0 {
                    sys.used_memory() as f64 / sys.total_memory() as f64 * 100.0
                } else {
                    f64::NAN
                };
                let now = now_ms();
                for (h, pid, cwd) in targets {
                    let (mut cpu, mut mem, mut seen) = (0.0f64, 0u64, false);
                    let mut stack = vec![Pid::from_u32(pid)];
                    let mut guard = 0;
                    while let Some(p) = stack.pop() {
                        guard += 1;
                        if guard > 100_000 {
                            break;
                        }
                        if let Some(proc_) = sys.process(p) {
                            seen = true;
                            cpu += proc_.cpu_usage() as f64;
                            mem += proc_.memory();
                        }
                        if let Some(kids) = children.get(&p) {
                            stack.extend(kids.iter().copied());
                        }
                    }
                    let free = disk_free(&disks, Path::new(&cwd));
                    let mut st = h.state.lock().unwrap();
                    if seen {
                        st.data.proc_cpu.push(now, cpu);
                        st.data.proc_mem.push(now, mem as f64);
                    }
                    st.data.host_cpu.push(now, host_cpu);
                    if host_mem.is_finite() {
                        st.data.host_mem.push(now, host_mem);
                    }
                    if let Some(f) = free {
                        st.data.disk_free.push(now, f as f64);
                    }
                }
            }
        });
}

/// Free space on the disk whose mount point is the longest prefix of `path`.
fn disk_free(disks: &Disks, path: &Path) -> Option<u64> {
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(|d| d.available_space())
}
