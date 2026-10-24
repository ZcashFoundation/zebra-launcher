//! The pid file that lets a new launcher notice a zebrad left behind by an
//! earlier one (after a crash or a force quit).
//!
//! The file holds the pid and the path of the zebrad binary that was started.
//! A pid alone can be reused by an unrelated process, so [`check`] only reports
//! a stale entry when the process is alive and, where the platform lets us
//! find out, runs the recorded binary. The UI asks the user before killing
//! anything.
//!
//! The executable check is best effort: it reads `/proc/<pid>/exe` on Linux,
//! calls `proc_pidpath` on macOS and `QueryFullProcessImageNameW` on Windows.
//! On other platforms, or when the executable cannot be read (for example a
//! process owned by another user), the entry is trusted as before.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use launcher_types::StaleProcessInfo;

use crate::fsutil;

/// A zebrad process recorded by a previous launcher run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaleProcess {
    /// The recorded process id.
    pub pid: u32,
    /// The zebrad binary that was started, for display.
    pub binary: PathBuf,
}

/// The wire form for the UI: the one place the two types are mapped.
impl From<&StaleProcess> for StaleProcessInfo {
    fn from(stale: &StaleProcess) -> Self {
        Self {
            pid: stale.pid,
            binary: stale.binary.display().to_string(),
        }
    }
}

/// Writes `pid` and `binary` to `path`, replacing any previous content.
pub fn write(path: &Path, pid: u32, binary: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fsutil::write_atomic(path, format!("{pid}\n{}\n", binary.display()).as_bytes())
}

/// Removes the pid file. A missing file is not an error.
pub fn remove(path: &Path) -> io::Result<()> {
    fsutil::remove_if_exists(path)
}

/// Reads the pid file and returns the recorded process if it is still running.
///
/// Returns `None` when there is no file, the file is malformed, the process
/// has exited, or the pid now belongs to a process running a different
/// executable than the recorded binary. Such an entry is removed.
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
        // pid 0 and our own pid never name a zebrad left behind.
        Some(stale)
            if !is_own_or_group(stale.pid)
                && is_alive(stale.pid)
                && runs_binary(stale.pid, &stale.binary) =>
        {
            Ok(Some(stale))
        }
        _ => {
            remove(path)?;
            Ok(None)
        }
    }
}

/// Whether the live process `pid` runs `binary`. True when the executable of
/// `pid` cannot be determined, so the check only ever rules a process out.
fn runs_binary(pid: u32, binary: &Path) -> bool {
    let Some(live) = executable_of(pid) else {
        return true;
    };
    let matches = same_file_path(&live, binary);
    if !matches {
        tracing::info!(
            pid,
            recorded = %binary.display(),
            running = %live.display(),
            "the recorded zebrad pid now belongs to another program"
        );
    }
    matches
}

/// Compares two paths after resolving symlinks and, on Windows, ignoring case.
fn same_file_path(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (a, b) = (canonical(a), canonical(b));
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// The executable of the running process `pid`, if the platform tells us.
#[cfg(target_os = "linux")]
fn executable_of(pid: u32) -> Option<PathBuf> {
    let link = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
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
fn executable_of(pid: u32) -> Option<PathBuf> {
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

/// The executable of the running process `pid`, if the platform tells us.
#[cfg(windows)]
fn executable_of(pid: u32) -> Option<PathBuf> {
    use std::os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    };

    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };

    // SAFETY: OpenProcess has no preconditions; a null result is checked below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    // SAFETY: the handle was just returned by OpenProcess and is owned by us.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut buf = vec![0u16; 32_768];
    let mut size = u32::try_from(buf.len()).ok()?;
    // SAFETY: the handle is valid, `buf` is writable for `size` UTF-16 units and
    // `size` points to writable memory.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle.as_raw_handle(),
            PROCESS_NAME_WIN32,
            buf.as_mut_ptr(),
            &mut size,
        )
    };
    if ok == 0 {
        return None;
    }
    buf.truncate(usize::try_from(size).ok()?);
    Some(PathBuf::from(std::ffi::OsString::from_wide(&buf)))
}

/// Other platforms cannot tell; the pid alone is trusted.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn executable_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// True for pids that never name a recorded zebrad: 0 (which `kill` treats as
/// the caller's own process group) and the launcher itself.
fn is_own_or_group(pid: u32) -> bool {
    pid == 0 || pid == std::process::id()
}

/// Returns true when a process with `pid` exists.
///
/// Always false for pid 0 and for the launcher's own pid.
#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    if is_own_or_group(pid) {
        return false;
    }
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
///
/// Always false for pid 0 and for the launcher's own pid.
#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    if is_own_or_group(pid) {
        return false;
    }
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
///
/// Refuses pid 0 (on Unix, the launcher's own process group) and the
/// launcher's own pid.
pub fn terminate(pid: u32) -> io::Result<()> {
    if is_own_or_group(pid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to terminate pid {pid}: it is not a separate process"),
        ));
    }
    #[cfg(unix)]
    {
        crate::process::unix::terminate(pid)
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

    /// A child that stays alive until killed.
    /// `executable_of` for a child we just spawned. Right after `spawn` returns,
    /// the kernel can still report the parent's image for the child (the exec
    /// has not replaced it yet), so wait until it no longer reads as our own
    /// executable.
    fn settled_executable_of(pid: u32) -> Option<PathBuf> {
        let ours = std::env::current_exe()
            .ok()
            .and_then(|p| fs::canonicalize(p).ok());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let seen = executable_of(pid);
            let seen_canonical = seen.as_ref().and_then(|p| fs::canonicalize(p).ok());
            if seen.is_some() && (ours.is_none() || seen_canonical != ours) {
                return seen;
            }
            if std::time::Instant::now() >= deadline {
                return seen;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn sleeper() -> std::process::Child {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("a sleeping child process spawns")
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn a_reused_pid_running_another_binary_is_not_stale() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("zebrad.pid");
        let mut live = sleeper();
        let pid = live.id();
        let other = dir.path().join("zebrad");
        fs::write(&other, b"not the running program").expect("write binary");
        write(&path, pid, &other).expect("write");
        let found = check(&path);
        let still_alive = is_alive(pid);
        live.kill().expect("kill");
        live.wait().expect("reap");
        assert!(still_alive, "the process itself is alive");
        assert_eq!(found.expect("read"), None);
        assert!(!path.exists(), "the mismatched entry is removed");
    }

    #[test]
    fn the_wire_form_shows_the_binary_path() {
        let stale = StaleProcess {
            pid: 4242,
            binary: PathBuf::from("/opt/zebrad"),
        };
        assert_eq!(
            StaleProcessInfo::from(&stale),
            StaleProcessInfo {
                pid: 4242,
                binary: "/opt/zebrad".to_string(),
            }
        );
    }

    #[test]
    fn own_pid_and_pid_zero_are_never_stale_or_terminated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("zebrad.pid");
        for pid in [0, std::process::id()] {
            assert!(!is_alive(pid), "pid {pid}");
            assert!(terminate(pid).is_err(), "pid {pid}");
            write(&path, pid, Path::new("/opt/zebrad")).expect("write");
            assert_eq!(check(&path).expect("read"), None, "pid {pid}");
            assert!(!path.exists());
        }
    }

    #[test]
    fn round_trip_and_stale_detection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("zebrad.pid");
        assert_eq!(check(&path).expect("missing file is fine"), None);

        // A live process running the recorded binary is reported.
        let mut live = sleeper();
        let pid = live.id();
        let binary = settled_executable_of(pid).unwrap_or_else(|| PathBuf::from("/opt/zebrad"));
        write(&path, pid, &binary).expect("write");
        let found = check(&path);
        live.kill().expect("kill");
        live.wait().expect("reap");
        assert_eq!(found.expect("read"), Some(StaleProcess { pid, binary }));

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
