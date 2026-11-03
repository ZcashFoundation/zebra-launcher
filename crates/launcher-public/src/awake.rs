//! Keeping this computer awake while a service serves: on Linux a `systemd-inhibit` child
//! that holds a sleep and idle lock while it runs. Its command is `cat` on a pipe Zeal holds,
//! so the lock goes when Zeal exits, crashed or not. It cannot stop a closed lid. Without
//! `systemd-inhibit` nothing is held, and the status says so.

use tokio::process::{Child, Command};

/// The inhibitor while it is held.
#[derive(Debug)]
pub struct KeepAwake {
    child: Child,
}

impl KeepAwake {
    /// Takes the lock, or `None` when this system has no `systemd-inhibit`.
    pub fn hold() -> Option<KeepAwake> {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let child = Command::new("systemd-inhibit")
            .args([
                "--what=sleep:idle",
                "--who=Zeal",
                "--why=Serving light wallets",
                "--mode=block",
                "cat",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        Some(KeepAwake { child })
    }

    /// Whether the lock is still held.
    pub fn held(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}
