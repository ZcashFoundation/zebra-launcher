//! The pid file that lets a new launcher notice a zebrad left behind by an
//! earlier one (after a crash or a force quit).
//!
//! The file holds the pid and the path of the zebrad binary that was started.
//! A pid alone can be reused by an unrelated process, so [`check`] only reports
//! a stale entry when the process is alive and, where the platform lets us
//! find out, runs the recorded binary. The UI asks the user before killing
//! anything.
//!
//! The executable check is best effort ([`process::executable_of`]): where
//! the platform cannot tell, the entry is trusted as before.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use launcher_types::StaleProcessInfo;

use crate::{fsutil, process};

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
    let Some(live) = process::executable_of(pid) else {
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

/// True for pids that never name a recorded zebrad: 0 (which `kill` treats as
/// the caller's own process group) and the launcher itself.
fn is_own_or_group(pid: u32) -> bool {
    pid == 0 || pid == std::process::id()
}

/// Returns true when a process with `pid` exists ([`process::is_alive`]).
///
/// Always false for pid 0 and for the launcher's own pid.
pub fn is_alive(pid: u32) -> bool {
    !is_own_or_group(pid) && process::is_alive(pid)
}

/// Sends the platform's termination request to `pid`
/// ([`process::terminate_pid`]): `SIGTERM` on Unix, `TerminateProcess` on
/// Windows, because a stale process is not in a job the launcher owns.
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
    process::terminate_pid(pid)
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
            let seen = process::executable_of(pid);
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
