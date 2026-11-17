//! Unix process control: process groups, the parent-death signal, signals,
//! and the liveness and executable checks behind the pid file.

use std::{io, path::PathBuf};

use nix::{
    errno::Errno,
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};

/// Puts the child in its own process group, so an escalated kill reaches
/// anything it started, on Linux makes the kernel send it `SIGTERM` if the
/// spawning thread dies, and raises its nice value by `nice` when that isn't 0
/// (zingod's 10).
pub(super) fn prepare(cmd: &mut tokio::process::Command, nice: i32) {
    cmd.process_group(0);
    // Elsewhere zebrad spawns without a hook, as it always has.
    if !cfg!(target_os = "linux") && nice == 0 {
        return;
    }
    // SAFETY: the closure runs in the forked child before exec and only makes
    // the prctl, getpriority and setpriority system calls and sets errno: no
    // allocation and no lock.
    unsafe {
        cmd.pre_exec(move || {
            #[cfg(target_os = "linux")]
            nix::sys::prctl::set_pdeathsig(Signal::SIGTERM).map_err(io::Error::from)?;
            set_nice(nice)
        });
    }
}

/// Raises the calling process's nice value by `nice`, capped at 19; 0 leaves
/// it alone. It adds to the nice value the child inherited, so the child stays
/// below the parent's priority, and since it never lowers the value it needs no
/// privilege at any parent nice.
fn set_nice(nice: i32) -> io::Result<()> {
    if nice == 0 {
        return Ok(());
    }
    // getpriority can return -1 as a nice value, so only errno tells an error.
    Errno::clear();
    // SAFETY: getpriority reads no memory of ours; `who` 0 is this process.
    let current = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    if current == -1 && Errno::last_raw() != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: setpriority reads no memory of ours; `who` 0 is this process.
    let result =
        unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, current.saturating_add(nice).min(19)) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn pid(pid: u32) -> io::Result<Pid> {
    i32::try_from(pid)
        .map(Pid::from_raw)
        .map_err(|_| io::Error::other("pid does not fit in a pid_t"))
}

/// Sends `SIGTERM` to zebrad. A process that is already gone is not an
/// error: it exited on its own just before the stop request.
pub(super) fn terminate(raw: u32) -> io::Result<()> {
    ignore_gone(kill(pid(raw)?, Signal::SIGTERM))
}

/// Maps `ESRCH` (no such process) to success.
fn ignore_gone(result: nix::Result<()>) -> io::Result<()> {
    match result {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(err) => Err(io::Error::from(err)),
    }
}

/// Sends `SIGKILL` to zebrad's process group and to zebrad itself. A process
/// that is already gone is not an error.
pub(super) fn kill_group(raw: u32) -> io::Result<()> {
    let pid = pid(raw)?;
    ignore_gone(killpg(pid, Signal::SIGKILL))?;
    ignore_gone(kill(pid, Signal::SIGKILL))
}

/// Returns true when a process with `pid` exists, also when it belongs to
/// another user.
pub(super) fn is_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 performs the permission and existence checks without sending anything.
    match kill(Pid::from_raw(pid), None) {
        Ok(()) => true,
        // The process exists but belongs to another user.
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// The executable of the running process `pid`, if the platform tells us.
#[cfg(target_os = "linux")]
pub(super) fn executable_of(pid: u32) -> Option<PathBuf> {
    let link = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    // A binary replaced or removed since the process started reads as
    // "<path> (deleted)".
    let link = match link.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(stripped) => PathBuf::from(stripped),
        None => link,
    };
    Some(link)
}

/// The executable of the running process `pid`, if the platform tells us.
#[cfg(target_os = "macos")]
pub(super) fn executable_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;

    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buf = vec![0u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
    let size = u32::try_from(buf.len()).ok()?;
    // SAFETY: `buf` is writable for `size` bytes for the duration of the call.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), size) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    buf.truncate(len);
    Some(PathBuf::from(std::ffi::OsString::from_vec(buf)))
}

/// Other Unixes cannot tell; the pid alone is trusted.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn executable_of(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signalling_an_exited_process_is_not_an_error() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("a trivial child process spawns");
        let pid = child.id();
        child.wait().expect("the child exits");
        terminate(pid).expect("terminate ignores a vanished process");
        kill_group(pid).expect("kill_group ignores a vanished process");
    }
}
