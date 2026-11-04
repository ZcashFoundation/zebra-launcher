//! Finds the bundled zebrad for a channel.
//!
//! The bundler installs the sidecars next to the launcher executable as
//! `zeal-zebrad`, `zeal-zebrad-beta` and `zeal-zebrad-nightly` (`.exe` on
//! Windows); `cargo tauri dev` copies them there too. Inside an AppImage the
//! executable lives on a FUSE mount that disappears when the launcher exits
//! or is replaced by an update, so the selected sidecar is copied into the
//! data directory first and zebrad runs from the copy.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use launcher_core::paths::Paths;
use launcher_types::ZebradChannel;

/// The stable sidecar's file name without the platform suffix, as installed.
pub const SIDECAR_NAME: &str = "zeal-zebrad";

/// The zebrad binary to run for `channel`. Never fails: when something is
/// wrong the supervisor reports the spawn failure to the user.
pub fn resolve(paths: &Paths, channel: ZebradChannel) -> PathBuf {
    let bundled = match bundled_path(&channel) {
        Ok(path) => path,
        Err(err) => {
            tracing::error!(%err, channel = channel.as_str(), "could not locate the launcher executable");
            PathBuf::from(sidecar_file_name(&channel))
        }
    };
    if !bundled.is_file() {
        tracing::error!(path = %bundled.display(), channel = channel.as_str(), "the bundled zebrad is missing");
        return bundled;
    }
    if cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_some() {
        let dir = appimage_dir(paths, &channel);
        match copy_out(&bundled, &dir) {
            Ok(copy) => {
                remove_other_versions(&paths.sidecar_dir().join(channel.as_str()), &dir);
                return copy;
            }
            Err(err) => tracing::error!(
                %err,
                channel = channel.as_str(),
                dir = %dir.display(),
                "could not copy zebrad out of the AppImage; running it from the mount"
            ),
        }
    }
    bundled
}

fn sidecar_file_name(channel: &ZebradChannel) -> String {
    format!("{}{}", channel.sidecar_name(), std::env::consts::EXE_SUFFIX)
}

/// The selected channel's `zeal-zebrad*[.exe]` next to the running executable.
fn bundled_path(channel: &ZebradChannel) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("the launcher executable has no parent directory"))?;
    Ok(dir.join(sidecar_file_name(channel)))
}

fn appimage_dir(paths: &Paths, channel: &ZebradChannel) -> PathBuf {
    paths
        .sidecar_dir()
        .join(channel.as_str())
        .join(env!("CARGO_PKG_VERSION"))
}

/// Copies `source` into `dir` unless an identical copy is already there, and
/// returns the copy's path. The copy is written to a temporary name and
/// renamed, so a crash never leaves a truncated binary behind.
pub fn copy_out(source: &Path, dir: &Path) -> io::Result<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::other("the sidecar path has no file name"))?;
    let target = dir.join(name);
    if target.is_file() && same_contents(source, &target)? {
        return Ok(target);
    }
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp", name.to_string_lossy()));
    fs::copy(source, &tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(&tmp, &target)?;
    tracing::info!(from = %source.display(), to = %target.display(), "copied the zebrad sidecar");
    Ok(target)
}

/// Compares two files byte for byte.
pub fn same_contents(a: &Path, b: &Path) -> io::Result<bool> {
    if fs::metadata(a)?.len() != fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    let mut buf_a = vec![0; 64 * 1024];
    let mut buf_b = vec![0; 64 * 1024];
    loop {
        let n = read_full(&mut a, &mut buf_a)?;
        let m = read_full(&mut b, &mut buf_b)?;
        if n != m || buf_a[..n] != buf_b[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Reads until `buf` is full or the file ends.
fn read_full(file: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Removes copies left by other launcher versions. Best effort.
fn remove_other_versions(sidecar_dir: &Path, keep: &Path) {
    let Ok(entries) = fs::read_dir(sidecar_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep
            && path.is_dir()
            && let Err(err) = fs::remove_dir_all(&path)
        {
            tracing::warn!(%err, path = %path.display(), "could not remove an old sidecar copy");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_once_and_replaces_a_changed_copy() {
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join(SIDECAR_NAME);
        fs::write(&source, b"zebrad v1").expect("write source");
        let dir = root.path().join("copy");

        let copy = copy_out(&source, &dir).expect("first copy");
        assert_eq!(copy, dir.join(SIDECAR_NAME));
        assert_eq!(fs::read(&copy).expect("read copy"), b"zebrad v1");
        assert!(same_contents(&source, &copy).expect("compare"));

        // Same length, different bytes: replaced.
        fs::write(&source, b"zebrad v2").expect("rewrite source");
        assert!(!same_contents(&source, &copy).expect("compare"));
        copy_out(&source, &dir).expect("second copy");
        assert_eq!(fs::read(&copy).expect("read copy"), b"zebrad v2");
        assert!(!dir.join(format!(".{SIDECAR_NAME}.tmp")).exists());
    }

    #[test]
    fn old_versions_are_removed() {
        let root = tempfile::tempdir().expect("tempdir");
        let keep = root.path().join("0.2.0");
        let old = root.path().join("0.1.0");
        fs::create_dir_all(&keep).expect("mkdir");
        fs::create_dir_all(&old).expect("mkdir");
        remove_other_versions(root.path(), &keep);
        assert!(keep.is_dir());
        assert!(!old.exists());
    }

    #[test]
    fn file_names_come_from_the_channel() {
        for channel in ZebradChannel::ALL {
            assert_eq!(
                sidecar_file_name(&channel),
                format!("{}{}", channel.sidecar_name(), std::env::consts::EXE_SUFFIX)
            );
        }
        assert_eq!(
            sidecar_file_name(&ZebradChannel::Stable),
            format!("{SIDECAR_NAME}{}", std::env::consts::EXE_SUFFIX)
        );
    }

    #[test]
    fn appimage_cleanup_preserves_other_channels() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(root.path());
        let stable = appimage_dir(&paths, &ZebradChannel::Stable);
        let beta = appimage_dir(&paths, &ZebradChannel::Beta);
        let old_stable = stable.parent().expect("version parent").join("0.1.0");
        fs::create_dir_all(&stable).expect("stable copy");
        fs::create_dir_all(&beta).expect("beta copy");
        fs::create_dir_all(&old_stable).expect("old stable copy");

        remove_other_versions(&paths.sidecar_dir().join("stable"), &stable);

        assert!(stable.is_dir());
        assert!(beta.is_dir());
        assert!(!old_stable.exists());
        assert_eq!(
            beta.parent().expect("channel parent"),
            paths.sidecar_dir().join("beta")
        );
    }
}
