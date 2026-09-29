//! Windows pseudo-console driver for `trun run --pty`.
//!
//! Written directly against ConPTY instead of using portable-pty because the flags
//! matter and portable-pty hardcodes them:
//! * `PSEUDOCONSOLE_PASSTHROUGH_MODE` (Windows 11 24H2+): ConPTY forwards the
//!   program's VT output as-is. Without it ConPTY re-renders its screen and emits
//!   *diffs* (cursor moves plus changed cells), which breaks line-oriented parsing:
//!   a tqdm bar arrives as `h 1:  80%|…`.
//! * No `PSEUDOCONSOLE_INHERIT_CURSOR`: with it, ConPTY asks the (nonexistent)
//!   terminal for the cursor position at startup and blocks until answered.
//!
//! Passthrough is tried first; older systems fall back to rendered mode.

use crate::process::{ExitStatusInfo, Killer, SpawnSpec, Started};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use tokio_util::io::StreamReader;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, S_OK};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

const PSEUDOCONSOLE_PASSTHROUGH_MODE: u32 = 0x8;
const COLS: i16 = 200;
const ROWS: i16 = 50;

/// A HANDLE that may cross threads (Win32 handles are process-wide).
struct Owned(HANDLE);
unsafe impl Send for Owned {}
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct Pcon(HPCON);
unsafe impl Send for Pcon {}
impl Drop for Pcon {
    fn drop(&mut self) {
        unsafe { ClosePseudoConsole(self.0) };
    }
}

fn pipe() -> io::Result<(Owned, Owned)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    if unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Owned(r), Owned(w)))
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Quote one argument by the MSVC `CommandLineToArgvW` rules.
fn quote_arg(arg: &str, out: &mut String) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        out.push_str(arg);
        return;
    }
    out.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
}

fn is_batch(program: &Path) -> bool {
    program
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("bat") || e.eq_ignore_ascii_case("cmd"))
}

/// The module CreateProcessW should load. Passed explicitly (with backslashes)
/// rather than left for Windows to parse out of the command line, which fails for
/// paths written with forward slashes.
fn application(program: &Path) -> PathBuf {
    if is_batch(program) {
        std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
    } else {
        PathBuf::from(program.to_string_lossy().replace('/', "\\"))
    }
}

fn command_line(program: &Path, args: &[String]) -> String {
    let mut line = String::new();
    if is_batch(program) {
        // Batch files need the command interpreter.
        line.push_str("cmd.exe /d /c ");
    }
    quote_arg(&program.to_string_lossy().replace('/', "\\"), &mut line);
    for a in args {
        line.push(' ');
        quote_arg(a, &mut line);
    }
    line
}

/// Inherited environment plus overrides, as a sorted UTF-16 block.
fn environment_block(extra: &[(String, String)]) -> Vec<u16> {
    let mut vars: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        let (k, v) = (
            k.to_string_lossy().into_owned(),
            v.to_string_lossy().into_owned(),
        );
        vars.insert(k.to_uppercase(), (k, v));
    }
    for (k, v) in extra {
        vars.insert(k.to_uppercase(), (k.clone(), v.clone()));
    }
    let mut block: Vec<u16> = Vec::new();
    for (_, (k, v)) in vars {
        block.extend(OsStr::new(&format!("{k}={v}")).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

pub fn start(spec: &SpawnSpec, resolved: PathBuf, args: &[String]) -> io::Result<Started> {
    let (in_read, in_write) = pipe()?;
    let (out_read, out_write) = pipe()?;

    let size = COORD { X: COLS, Y: ROWS };
    let mut hpc: HPCON = unsafe { std::mem::zeroed() };
    let mut hr = unsafe {
        CreatePseudoConsole(
            size,
            in_read.0,
            out_write.0,
            PSEUDOCONSOLE_PASSTHROUGH_MODE,
            &mut hpc,
        )
    };
    if hr != S_OK {
        hr = unsafe { CreatePseudoConsole(size, in_read.0, out_write.0, 0, &mut hpc) };
    }
    if hr != S_OK {
        return Err(io::Error::other(format!(
            "CreatePseudoConsole failed: HRESULT {hr:#x}"
        )));
    }
    let pcon = Pcon(hpc);
    // The pseudo-console owns its copies now.
    drop(in_read);
    drop(out_write);

    // Attribute list carrying the pseudo-console.
    let mut attr_size = 0usize;
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attr_size) };
    let mut attr_buf = vec![0u8; attr_size];
    let attrs = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
    if unsafe { InitializeProcThreadAttributeList(attrs, 1, 0, &mut attr_size) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let ok = unsafe {
        UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            pcon.0 as *const std::ffi::c_void,
            std::mem::size_of::<HPCON>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if ok == 0 {
        let e = io::Error::last_os_error();
        unsafe { DeleteProcThreadAttributeList(attrs) };
        return Err(e);
    }

    let mut si: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    // Without explicit invalid std handles the child can inherit the hub's
    // redirected stdio (its log file) instead of attaching to the pseudo-console.
    si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    si.lpAttributeList = attrs;

    let app = wide(application(&resolved).as_os_str());
    let mut cmdline: Vec<u16> = wide(OsStr::new(&command_line(&resolved, args)));
    let mut env = environment_block(&spec.env);
    let cwd = wide(spec.cwd.as_os_str());
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            app.as_ptr(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            env.as_mut_ptr() as *const std::ffi::c_void,
            cwd.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        )
    };
    let create_err = io::Error::last_os_error();
    unsafe { DeleteProcThreadAttributeList(attrs) };
    if created == 0 {
        return Err(io::Error::new(
            create_err.kind(),
            format!(
                "CreateProcessW({}) failed: {create_err}",
                application(&resolved).display()
            ),
        ));
    }
    drop(Owned(pi.hThread));
    let process = Owned(pi.hProcess);
    let pid = pi.dwProcessId;
    let killer = Killer::for_pid(pid);

    // Output pump: blocking reads on a thread, answering terminal queries inline.
    let (btx, brx) = mpsc::channel::<io::Result<bytes::Bytes>>(64);
    // Debug aid: TRUN_PTY_TRACE=<file> (in the hub's environment) appends the raw
    // pseudo-console bytes, to see exactly what ConPTY emits.
    let mut trace = std::env::var_os("TRUN_PTY_TRACE").and_then(|p| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .ok()
    });
    std::thread::Builder::new()
        .name(format!("conpty-read-{pid}"))
        .spawn(move || {
            let (out_read, in_write) = (out_read, in_write);
            let mut normalizer = Normalizer::default();
            let mut buf = [0u8; 8192];
            loop {
                let mut n = 0u32;
                let ok = unsafe {
                    ReadFile(
                        out_read.0,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        &mut n,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || n == 0 {
                    break;
                }
                if let Some(f) = trace.as_mut() {
                    use std::io::Write;
                    let _ = f.write_all(&buf[..n as usize]);
                }
                let (out, replies) = crate::pty::answer_queries(&buf[..n as usize]);
                if !replies.is_empty() {
                    let mut written = 0u32;
                    unsafe {
                        WriteFile(
                            in_write.0,
                            replies.as_ptr(),
                            replies.len() as u32,
                            &mut written,
                            std::ptr::null_mut(),
                        )
                    };
                }
                let out = normalizer.feed(&out);
                if !out.is_empty() && btx.blocking_send(Ok(bytes::Bytes::from(out))).is_err() {
                    break;
                }
            }
        })?;

    let (etx, erx) = oneshot::channel();
    std::thread::Builder::new()
        .name(format!("conpty-wait-{pid}"))
        .spawn(move || {
            let (process, pcon) = (process, pcon);
            unsafe { WaitForSingleObject(process.0, INFINITE) };
            let mut code = 0u32;
            let status = if unsafe { GetExitCodeProcess(process.0, &mut code) } != 0 {
                Ok(ExitStatusInfo {
                    code: Some(code as i32),
                    signal: None,
                })
            } else {
                Err(io::Error::last_os_error())
            };
            // Output written just before exit is still in flight inside ConPTY; give it
            // a moment, then close the console so the reader sees EOF.
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(pcon);
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

/// Turns ConPTY's rendered output back into line-oriented text.
///
/// Without passthrough, ConPTY replays its screen buffer: a program's `\r` redraw
/// arrives as `ESC[H` / `ESC[row;colH` (cursor positioning), new lines as moves to
/// a lower row, plus screen clears. Line parsing needs the original structure, so:
/// * move to a *later* row → newline
/// * move to column 1 (same or earlier row), or `ESC[1G` → carriage return
/// * clear-screen / erase sequences → dropped
///
/// Escape sequences split across reads are carried over to the next chunk.
#[derive(Default)]
pub(crate) struct Normalizer {
    row: u32,
    carry: Vec<u8>,
}

impl Normalizer {
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(chunk);
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            if b == b'\n' {
                self.row += 1;
                out.push(b);
                i += 1;
                continue;
            }
            if b != 0x1b {
                out.push(b);
                i += 1;
                continue;
            }
            // ESC
            if i + 1 >= data.len() {
                self.carry = data[i..].to_vec();
                break;
            }
            if data[i + 1] != b'[' {
                out.push(b);
                i += 1;
                continue;
            }
            // CSI: ESC [ params final(@..~)
            let Some(end) = data[i + 2..].iter().position(|c| (0x40..=0x7e).contains(c)) else {
                if data.len() - i > 64 {
                    // Not a real sequence; don't carry forever.
                    out.push(b);
                    i += 1;
                    continue;
                }
                self.carry = data[i..].to_vec();
                break;
            };
            let fin = data[i + 2 + end];
            let params = std::str::from_utf8(&data[i + 2..i + 2 + end]).unwrap_or("");
            let seq_len = 3 + end;
            let nums: Vec<u32> = params.split(';').map(|p| p.parse().unwrap_or(1)).collect();
            match fin {
                b'H' | b'f' if !params.starts_with('?') => {
                    let row = nums.first().copied().unwrap_or(1).max(1);
                    let col = nums.get(1).copied().unwrap_or(1).max(1);
                    if self.row == 0 {
                        self.row = row; // first positioning just establishes the row
                    } else if row > self.row {
                        out.extend(std::iter::repeat_n(b'\n', (row - self.row) as usize));
                        self.row = row;
                    } else if col == 1 {
                        out.push(b'\r');
                        self.row = row;
                    }
                }
                b'G' if nums.first().copied().unwrap_or(1) <= 1 => out.push(b'\r'),
                // Clear screen / erase line: meaningless in a log.
                b'J' | b'K' => {}
                _ => out.extend_from_slice(&data[i..i + seq_len]),
            }
            i += seq_len;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(chunks: &[&[u8]]) -> String {
        let mut n = Normalizer::default();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(n.feed(c));
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn rendered_redraws_become_carriage_returns() {
        let s = norm(&[b"\x1b[2J\x1b[m\x1b[HEpoch 1:   0%| 0/6\x1b[?25l\x1b[HEpoch 1:  17%| 1/6\x1b[HEpoch 1: 100%| 6/6\r\nafter\r\n"]);
        assert_eq!(
            s,
            "\x1b[mEpoch 1:   0%| 0/6\x1b[?25l\rEpoch 1:  17%| 1/6\rEpoch 1: 100%| 6/6\r\nafter\r\n"
        );
    }

    #[test]
    fn move_to_later_row_is_a_newline_and_split_sequences_carry() {
        let s = norm(&[b"\x1b[1;1Hline one\x1b[3", b";1Hline three"]);
        assert_eq!(s, "line one\n\nline three");
    }

    #[test]
    fn msvc_quoting() {
        let q = |a: &str| {
            let mut s = String::new();
            quote_arg(a, &mut s);
            s
        };
        assert_eq!(q("plain"), "plain");
        assert_eq!(q("has space"), "\"has space\"");
        assert_eq!(q(""), "\"\"");
        assert_eq!(q(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(q(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(q(r"a\\b"), r"a\\b");
    }

    #[test]
    fn batch_files_go_through_cmd() {
        let line = command_line(Path::new(r"C:\x\npm.cmd"), &["run".into(), "build".into()]);
        assert!(line.starts_with("cmd.exe /d /c "), "{line}");
    }
}
