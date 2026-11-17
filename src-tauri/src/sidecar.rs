//! Finds the bundled zebrad for a channel, and the bundled zingod.
//!
//! The bundler installs the sidecars next to the launcher executable as
//! `zeal-zebrad`, `zeal-zebrad-beta` and `zeal-zebrad-nightly` (`.exe` on
//! Windows), and on Linux `zeal-zingod`; `cargo tauri dev` copies them there
//! too. Inside an AppImage the executable lives on a FUSE mount that
//! disappears when the launcher exits or is replaced by an update, so the
//! selected sidecar is copied into the data directory first and runs from the
//! copy: `<data>/sidecar/<channel>/<version>/` for zebrad and
//! `<data>/sidecar/zingod/<version>/` for zingod.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use launcher_core::paths::Paths;
use launcher_types::ZebradChannel;

/// The stable sidecar's file name without the platform suffix, as installed.
pub const SIDECAR_NAME: &str = "zeal-zebrad";

/// zingod's sidecar file name, as installed on Linux.
pub const ZINGOD_NAME: &str = "zeal-zingod";

/// A bundled program: zebrad for a channel, or zingod.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sidecar {
    Zebrad(ZebradChannel),
    Zingod,
}

impl Sidecar {
    /// The program's name for logs.
    fn program(self) -> &'static str {
        match self {
            Sidecar::Zebrad(_) => "zebrad",
            Sidecar::Zingod => "zingod",
        }
    }

    /// The folder of its AppImage copies, below the sidecar folder: the channel's, or
    /// `zingod`.
    fn folder(self) -> &'static str {
        match self {
            Sidecar::Zebrad(channel) => channel.as_str(),
            Sidecar::Zingod => "zingod",
        }
    }
}

/// The binary to run for `sidecar`. Never fails: when something is wrong the
/// supervisor reports the spawn failure to the user.
pub fn resolve(paths: &Paths, sidecar: Sidecar) -> PathBuf {
    let bundled = match bundled_path(sidecar) {
        Ok(path) => path,
        Err(err) => {
            tracing::error!(%err, sidecar = sidecar.folder(), "could not locate the launcher executable");
            PathBuf::from(sidecar_file_name(sidecar))
        }
    };
    if !bundled.is_file() {
        // zingod is bundled on Linux only: elsewhere its absence is expected.
        if sidecar == Sidecar::Zingod && !cfg!(target_os = "linux") {
            tracing::debug!(path = %bundled.display(), "no bundled zingod on this system");
        } else {
            tracing::error!(path = %bundled.display(), sidecar = sidecar.folder(), "the bundled {} is missing", sidecar.program());
        }
        return bundled;
    }
    if cfg!(target_os = "linux") && std::env::var_os("APPIMAGE").is_some() {
        let dir = appimage_dir(paths, sidecar);
        match copy_out(&bundled, &dir) {
            Ok(copy) => {
                remove_other_versions(&paths.sidecar_dir().join(sidecar.folder()), &dir);
                return copy;
            }
            Err(err) => tracing::error!(
                %err,
                sidecar = sidecar.folder(),
                dir = %dir.display(),
                "could not copy {} out of the AppImage; running it from the mount",
                sidecar.program()
            ),
        }
    }
    bundled
}

/// The bundled zingod, where one is: Linux only. `None` disables the light client server's
/// zingod choice.
pub fn zingod(paths: &Paths) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    Some(resolve(paths, Sidecar::Zingod)).filter(|path| path.is_file())
}

fn sidecar_file_name(sidecar: Sidecar) -> String {
    let name = match sidecar {
        Sidecar::Zebrad(channel) => channel.sidecar_name(),
        Sidecar::Zingod => ZINGOD_NAME,
    };
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The sidecar's file next to the running executable.
fn bundled_path(sidecar: Sidecar) -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("the launcher executable has no parent directory"))?;
    Ok(dir.join(sidecar_file_name(sidecar)))
}

fn appimage_dir(paths: &Paths, sidecar: Sidecar) -> PathBuf {
    paths
        .sidecar_dir()
        .join(sidecar.folder())
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
    tracing::info!(from = %source.display(), to = %target.display(), "copied a sidecar out of the AppImage");
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
                sidecar_file_name(Sidecar::Zebrad(channel)),
                format!("{}{}", channel.sidecar_name(), std::env::consts::EXE_SUFFIX)
            );
        }
        assert_eq!(
            sidecar_file_name(Sidecar::Zebrad(ZebradChannel::Stable)),
            format!("{SIDECAR_NAME}{}", std::env::consts::EXE_SUFFIX)
        );
        assert_eq!(
            sidecar_file_name(Sidecar::Zingod),
            format!("zeal-zingod{}", std::env::consts::EXE_SUFFIX)
        );
    }

    #[test]
    fn zingods_appimage_copy_has_a_folder_of_its_own() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(root.path());
        let zingod = appimage_dir(&paths, Sidecar::Zingod);
        assert_eq!(
            zingod,
            paths
                .sidecar_dir()
                .join("zingod")
                .join(env!("CARGO_PKG_VERSION"))
        );
        // zebrad's folders are unchanged.
        assert_eq!(
            appimage_dir(&paths, Sidecar::Zebrad(ZebradChannel::Stable)),
            paths
                .sidecar_dir()
                .join("stable")
                .join(env!("CARGO_PKG_VERSION"))
        );
        // Each program's cleanup keeps the other's copies.
        let stable = appimage_dir(&paths, Sidecar::Zebrad(ZebradChannel::Stable));
        let old_zingod = zingod.parent().expect("version parent").join("0.0.9");
        for dir in [&zingod, &stable, &old_zingod] {
            fs::create_dir_all(dir).expect("copy");
        }
        remove_other_versions(&paths.sidecar_dir().join("zingod"), &zingod);
        assert!(zingod.is_dir() && stable.is_dir());
        assert!(!old_zingod.exists());
    }

    #[test]
    fn appimage_cleanup_preserves_other_channels() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(root.path());
        let stable = appimage_dir(&paths, Sidecar::Zebrad(ZebradChannel::Stable));
        let beta = appimage_dir(&paths, Sidecar::Zebrad(ZebradChannel::Beta));
        let old_stable = stable.parent().expect("version parent").join("0.0.9");
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
