//! The pid file that lets a new launcher notice a zebrad left behind by an
//! earlier one (after a crash or a force quit).
//!
//! The file holds the pid and the path of the zebrad binary that was started.
//! A pid alone can be reused by an unrelated process, so callers only act on a
//! stale entry after confirming the process is alive, and the UI asks the user
//! before killing anything.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// A zebrad process recorded by a previous launcher run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleProcess {
    /// The recorded process id.
    pub pid: u32,
    /// The zebrad binary that was started, for display.
    pub binary: PathBuf,
}

/// Writes `pid` and `binary` to `path`, replacing any previous content.
pub fn write(path: &Path, pid: u32, binary: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("pid.tmp");
    fs::write(&tmp, format!("{pid}\n{}\n", binary.display()))?;
    fs::rename(&tmp, path)
}

/// Removes the pid file. A missing file is not an error.
pub fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Reads the pid file and returns the recorded process if it is still running.
///
/// Returns `None` when there is no file, the file is malformed, or the process
/// has exited. A malformed or dead entry is removed.
pub fn check(path: &Path) -> io::Result<Option<StaleProcess>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut lines = content.lines();
    let parsed = lines
        .next()
        .and_then(|pid| pid.trim().parse::<u32>().ok())
        .map(|pid| StaleProcess {
            pid,
            binary: PathBuf::from(lines.next().unwrap_or_default().trim()),
        });
    match parsed {
        Some(stale) if is_alive(stale.pid) => Ok(Some(stale)),
        _ => {
            remove(path)?;
            Ok(None)
        }
    }
}

/// Returns true when a process with `pid` exists.
#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
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

/// Returns true when a process with `pid` exists and has not exited.
#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};

    use windows_sys::Win32::{
        Foundation::STILL_ACTIVE,
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    // SAFETY: OpenProcess has no preconditions; a null result is checked below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: the handle was just returned by OpenProcess and is owned by us.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut exit_code: u32 = 0;
    // SAFETY: the handle is valid for the call and exit_code points to writable memory.
    let ok = unsafe {
        GetExitCodeProcess(
            std::os::windows::io::AsRawHandle::as_raw_handle(&handle),
            &mut exit_code,
        )
    };
    // STILL_ACTIVE is 259; a process that happens to exit with 259 is misreported, which
    // is the documented limitation of this API and acceptable for a stale-pid hint.
    ok != 0 && exit_code == STILL_ACTIVE as u32
}

/// Sends the platform's termination request to `pid`.
///
/// On Unix this is `SIGTERM`; on Windows it is `TerminateProcess`, because a
/// stale process is not in a job the launcher owns.
pub fn terminate(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use nix::{
            sys::signal::{Signal, kill},
            unistd::Pid,
        };
        let pid = i32::try_from(pid).map_err(|_| io::Error::other("pid out of range"))?;
        kill(Pid::from_raw(pid), Signal::SIGTERM).map_err(io::Error::from)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        // SAFETY: OpenProcess has no preconditions; a null result is checked below.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the handle was just returned by OpenProcess and is owned by us.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        // SAFETY: the handle is valid and has PROCESS_TERMINATE access.
        if unsafe { TerminateProcess(handle.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_stale_detection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("zebrad.pid");
        assert_eq!(check(&path).expect("missing file is fine"), None);

        // Our own pid is certainly alive.
        let me = std::process::id();
        write(&path, me, Path::new("/opt/zebrad")).expect("write");
        assert_eq!(
            check(&path).expect("read"),
            Some(StaleProcess {
                pid: me,
                binary: PathBuf::from("/opt/zebrad"),
            })
        );

        // A malformed file is removed.
        fs::write(&path, "not a pid\n").expect("write");
        assert_eq!(check(&path).expect("read"), None);
        assert!(!path.exists());

        // A dead pid is removed. A child that has already exited and been
        // waited for is dead, and its pid is not reused that quickly.
        let dead = {
            let mut cmd = if cfg!(windows) {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", "exit 0"]);
                c
            } else {
                std::process::Command::new("true")
            };
            let mut child = cmd.spawn().expect("a trivial child process spawns");
            let pid = child.id();
            child.wait().expect("the child exits");
            pid
        };
        write(&path, dead, Path::new("/opt/zebrad")).expect("write");
        assert_eq!(check(&path).expect("read"), None);
        assert!(!path.exists());
        remove(&path).expect("removing a missing file is fine");
    }
}
