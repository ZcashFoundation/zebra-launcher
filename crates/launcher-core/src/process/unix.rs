//! Unix process control: process groups, the parent-death signal and signals.

use std::io;

use nix::{
    errno::Errno,
    sys::signal::{Signal, kill, killpg},
    unistd::Pid,
};

/// Puts zebrad in its own process group, so an escalated kill reaches anything
/// it started, and on Linux makes the kernel send it `SIGTERM` if the
/// spawning thread dies.
pub(super) fn prepare(cmd: &mut tokio::process::Command) {
    cmd.process_group(0);
    #[cfg(target_os = "linux")]
    {
        // SAFETY: the closure runs in the forked child before exec and only
        // performs the prctl syscall, which is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                nix::sys::prctl::set_pdeathsig(Signal::SIGTERM).map_err(io::Error::from)
            });
        }
    }
}

fn pid(pid: u32) -> io::Result<Pid> {
    i32::try_from(pid)
        .map(Pid::from_raw)
        .map_err(|_| io::Error::other("pid does not fit in a pid_t"))
}

/// Sends `SIGTERM` to zebrad. A process that is already gone is not an
/// error: it exited on its own just before the stop request.
pub(crate) fn terminate(raw: u32) -> io::Result<()> {
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
pub(crate) fn kill_group(raw: u32) -> io::Result<()> {
    let pid = pid(raw)?;
    ignore_gone(killpg(pid, Signal::SIGKILL))?;
    ignore_gone(kill(pid, Signal::SIGKILL))
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
