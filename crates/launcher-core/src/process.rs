//! Spawning zebrad and stopping it, and the OS process primitives behind the
//! pid file ([`is_alive`], [`terminate_pid`], [`executable_of`]).
//!
//! Spawning happens on one long-lived thread, the [`Spawner`]: on Linux the
//! child's parent-death signal is tied to the thread that forked it, and on
//! Windows the child is created suspended, put in its own job object, and
//! resumed, which is simplest done synchronously. The supervisor keeps a
//! [`ChildHandle`] to stop the process and moves the `Child` itself into a
//! task that waits for it.
//!
//! All raw OS calls live in the two backends, `unix` and `windows`, which
//! offer the same functions so they can be read side by side.

use std::{ffi::OsString, io, path::PathBuf, process::Stdio};

use tokio::{
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    runtime::Handle,
    sync::oneshot,
};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Returns true when a process with `pid` exists: on Unix also when it
/// belongs to another user, on Windows only while it has not exited.
///
/// For a process the launcher did not start; the pid file's check adds the
/// rules about pid 0 and the launcher's own pid.
pub(crate) fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unix::is_alive(pid)
    }
    #[cfg(windows)]
    {
        windows::is_alive(pid)
    }
}

/// Sends the platform's termination request to a process the launcher did
/// not start: `SIGTERM` on Unix, `TerminateProcess` on Windows (a stale
/// process is in no job the launcher owns).
pub(crate) fn terminate_pid(pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix::terminate(pid)
    }
    #[cfg(windows)]
    {
        windows::terminate_pid(pid)
    }
}

/// The executable of the running process `pid`, if the platform tells us: it
/// reads `/proc/<pid>/exe` on Linux, calls `proc_pidpath` on macOS and
/// `QueryFullProcessImageNameW` on Windows. `None` on other platforms, or
/// when the executable cannot be read (for example a process owned by
/// another user).
pub(crate) fn executable_of(pid: u32) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        unix::executable_of(pid)
    }
    #[cfg(windows)]
    {
        windows::executable_of(pid)
    }
}

/// The argument that asks zebrad to shut down when its stdin closes.
///
/// Proposed upstream; fake-zebrad implements it already.
pub const SHUTDOWN_ON_STDIN_CLOSE_ARG: &str = "--shutdown-on-stdin-close";

/// How to run zebrad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZebradCommand {
    /// The zebrad binary.
    pub binary: PathBuf,
    /// The config file passed with `-c`.
    pub config: PathBuf,
    /// Extra arguments after `start`.
    pub extra_args: Vec<OsString>,
    /// Extra environment variables. Inherited `ZEBRA_*` variables are removed
    /// first, so the launcher's config is not silently overridden.
    pub env: Vec<(OsString, OsString)>,
    /// Pass [`SHUTDOWN_ON_STDIN_CLOSE_ARG`] and keep a pipe to zebrad's stdin,
    /// so dropping the pipe is a graceful stop on every platform, also when
    /// the launcher dies. Needs a zebrad that understands the flag.
    pub shutdown_on_stdin_close: bool,
    /// Windows only: kill zebrad when the launcher exits for any reason,
    /// including a crash (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`).
    ///
    /// Paired with [`Self::shutdown_on_stdin_close`]: while that is off this
    /// stays on, or a launcher crash would leave zebrad running with nothing
    /// to stop it; once the bundled zebrad has the flag the closing pipe does
    /// the job gracefully and this can be turned off. Nothing turns it off
    /// yet. [`Spawner::spawn`] asserts the pairing in debug builds.
    pub kill_on_launcher_exit: bool,
}

impl ZebradCommand {
    /// `binary -c config start`, with the stdin-close mode off and the Windows
    /// kill-on-exit job limit on.
    pub fn new(binary: impl Into<PathBuf>, config: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            config: config.into(),
            extra_args: Vec::new(),
            env: Vec::new(),
            shutdown_on_stdin_close: false,
            kill_on_launcher_exit: true,
        }
    }

    /// Whether zebrad still stops when the launcher dies: its stdin pipe
    /// closes, or (Windows) its job is closed.
    fn stops_when_launcher_dies(&self) -> bool {
        self.shutdown_on_stdin_close || self.kill_on_launcher_exit
    }

    /// Whether a requested stop lets zebrad run its shutdown: the stdin
    /// pipe closes ([`Self::shutdown_on_stdin_close`]), or `SIGTERM` is sent
    /// on Unix. Without the stdin mode a stop on Windows terminates the
    /// process instead ([`StopMethod::Terminated`]); `Status::stop_is_graceful`
    /// tells the UI.
    pub fn stop_is_graceful(&self) -> bool {
        self.shutdown_on_stdin_close || cfg!(unix)
    }
}

/// A freshly spawned zebrad, split into the parts the supervisor needs.
pub struct SpawnedChild {
    /// Stops the process.
    pub handle: ChildHandle,
    /// Waits for the process. Killed on drop as a last resort.
    pub child: Child,
    /// zebrad's stdout.
    pub stdout: ChildStdout,
    /// zebrad's stderr.
    pub stderr: ChildStderr,
}

/// How a stop was requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopMethod {
    /// The stdin pipe was closed; zebrad shuts down gracefully.
    StdinClosed,
    /// `SIGTERM` was sent; zebrad shuts down gracefully.
    Signal,
    /// The process was terminated outright (Windows without the stdin mode).
    Terminated,
}

/// What the supervisor keeps to stop zebrad.
#[derive(Debug)]
pub struct ChildHandle {
    pid: u32,
    stdin: Option<ChildStdin>,
    #[cfg(windows)]
    job: windows::Job,
}

impl ChildHandle {
    /// The process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Asks zebrad to stop, as gracefully as the platform and the zebrad allow.
    pub fn request_stop(&mut self) -> io::Result<StopMethod> {
        if let Some(stdin) = self.stdin.take() {
            drop(stdin);
            return Ok(StopMethod::StdinClosed);
        }
        #[cfg(unix)]
        {
            unix::terminate(self.pid)?;
            Ok(StopMethod::Signal)
        }
        #[cfg(windows)]
        {
            self.kill()?;
            Ok(StopMethod::Terminated)
        }
    }

    /// Kills zebrad and, where the platform allows, everything it started.
    pub fn kill(&mut self) -> io::Result<()> {
        self.stdin.take();
        #[cfg(unix)]
        {
            unix::kill_group(self.pid)
        }
        #[cfg(windows)]
        {
            self.job.terminate()
        }
    }
}

struct SpawnRequest {
    command: ZebradCommand,
    reply: oneshot::Sender<io::Result<SpawnedChild>>,
}

/// Spawns zebrad processes from one dedicated thread.
#[derive(Clone, Debug)]
pub struct Spawner {
    tx: std::sync::mpsc::Sender<SpawnRequest>,
}

impl std::fmt::Debug for SpawnRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnRequest")
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

impl Spawner {
    /// Starts the spawner thread. `runtime` is the runtime the children's
    /// pipes and exit notifications are registered with.
    pub fn new(runtime: Handle) -> io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<SpawnRequest>();
        std::thread::Builder::new()
            .name("zebrad-spawner".into())
            .spawn(move || {
                let _guard = runtime.enter();
                // Ends when the last Spawner is dropped.
                while let Ok(request) = rx.recv() {
                    let result = spawn_here(&request.command);
                    let _ = request.reply.send(result);
                }
            })?;
        Ok(Self { tx })
    }

    /// Spawns `command` and returns its parts.
    pub async fn spawn(&self, command: ZebradCommand) -> io::Result<SpawnedChild> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(SpawnRequest { command, reply })
            .map_err(|_| io::Error::other("the zebrad spawner thread has exited"))?;
        response
            .await
            .map_err(|_| io::Error::other("the zebrad spawner thread dropped the request"))?
    }
}

/// Whether `key` is a variable zebrad's config loader reads. The loader
/// lowercases keys and Windows variable names ignore case, so `zebra_` and
/// `Zebra_` count too.
fn is_zebra_env_var(key: &std::ffi::OsStr) -> bool {
    key.as_encoded_bytes()
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"ZEBRA_"))
}

/// Spawns on the calling thread. See the module docs for why that matters.
fn spawn_here(command: &ZebradCommand) -> io::Result<SpawnedChild> {
    debug_assert!(
        command.stops_when_launcher_dies(),
        "kill_on_launcher_exit must stay on until shutdown_on_stdin_close is on"
    );
    let mut cmd = Command::new(&command.binary);
    cmd.arg("-c").arg(&command.config).arg("start");
    if command.shutdown_on_stdin_close {
        cmd.arg(SHUTDOWN_ON_STDIN_CLOSE_ARG);
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.args(&command.extra_args);
    for (key, _) in std::env::vars_os() {
        if is_zebra_env_var(&key) {
            cmd.env_remove(key);
        }
    }
    cmd.env("ZEBRA_TRACING__USE_COLOR", "false");
    cmd.env("ZEBRA_TRACING__FORCE_USE_COLOR", "false");
    cmd.envs(
        command
            .env
            .iter()
            .map(|(k, v)| (k.as_os_str(), v.as_os_str())),
    );
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    unix::prepare(&mut cmd);
    #[cfg(windows)]
    windows::prepare(&mut cmd);

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("the spawned zebrad has no pid"))?;

    #[cfg(windows)]
    let job = match windows::attach_job(&child, command.kill_on_launcher_exit) {
        Ok(job) => job,
        Err(err) => {
            // The child is still suspended; kill_on_drop finishes it off.
            let _ = child.start_kill();
            return Err(err);
        }
    };

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("zebrad stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("zebrad stderr was not piped"))?;
    let stdin = child.stdin.take();

    Ok(SpawnedChild {
        handle: ChildHandle {
            pid,
            stdin,
            #[cfg(windows)]
            job,
        },
        child,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::{ZebradCommand, is_zebra_env_var};

    #[test]
    fn zebrad_stays_stoppable_when_the_launcher_dies() {
        let mut command = ZebradCommand::new("zebrad", "zebrad.toml");
        assert!(command.stops_when_launcher_dies(), "the default pairing");
        command.kill_on_launcher_exit = false;
        assert!(
            !command.stops_when_launcher_dies(),
            "turning the job limit off needs the stdin-close mode"
        );
        command.shutdown_on_stdin_close = true;
        assert!(command.stops_when_launcher_dies());
    }

    #[test]
    fn a_stop_is_graceful_with_stdin_close_or_a_signal() {
        let mut command = ZebradCommand::new("zebrad", "zebrad.toml");
        assert_eq!(command.stop_is_graceful(), cfg!(unix));
        command.shutdown_on_stdin_close = true;
        assert!(command.stop_is_graceful());
    }

    #[test]
    fn zebra_env_vars_match_in_any_case() {
        for key in [
            "ZEBRA_NETWORK__NETWORK",
            "zebra_rpc__listen_addr",
            "Zebra_X",
            "ZEBRA_",
        ] {
            assert!(is_zebra_env_var(OsStr::new(key)), "{key}");
        }
        for key in ["ZEBRA", "ZEBRAD_X", "PATH", "MY_ZEBRA_X", ""] {
            assert!(!is_zebra_env_var(OsStr::new(key)), "{key}");
        }
    }
}
