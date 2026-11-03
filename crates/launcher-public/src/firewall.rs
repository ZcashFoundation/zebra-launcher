//! Which firewall tool this computer has, from `PATH`. Zeal shows its commands and never
//! runs them, with `sudo` or without.

use std::{ffi::OsStr, path::Path};

use launcher_types::public::FirewallTool;

/// Whether an executable named `name` is in one of `path`'s directories.
fn on_path(path: &OsStr, name: &str) -> bool {
    std::env::split_paths(path).any(|dir| is_executable(&dir.join(name)))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The firewall tool on `path`; with `None`, the process's `PATH` and the sbin directories
/// where distributions put them.
pub fn detect(path: Option<&OsStr>) -> Option<FirewallTool> {
    let default;
    let path = match path {
        Some(path) => path,
        None => {
            let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default();
            dirs.extend(["/usr/sbin", "/sbin"].map(std::path::PathBuf::from));
            default = std::env::join_paths(dirs).unwrap_or_default();
            default.as_os_str()
        }
    };
    launcher_herd::public::firewall::pick_tool(on_path(path, "firewall-cmd"), on_path(path, "ufw"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn tools_are_found_on_the_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().as_os_str().to_owned();
        assert_eq!(detect(Some(&path)), None);
        let ufw = dir.path().join("ufw");
        std::fs::write(&ufw, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&ufw, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(detect(Some(&path)), Some(FirewallTool::Ufw));
        let firewalld = dir.path().join("firewall-cmd");
        std::fs::write(&firewalld, "#!/bin/sh\n").unwrap();
        // Not executable: not a tool.
        assert_eq!(detect(Some(&path)), Some(FirewallTool::Ufw));
        std::fs::set_permissions(&firewalld, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(detect(Some(&path)), Some(FirewallTool::Firewalld));
    }
}
