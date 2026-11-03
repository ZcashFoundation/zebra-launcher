//! Small filesystem helpers shared by the config and settings modules.

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Suffix of the temporary files [`write_atomic`] creates next to its target.
pub(crate) const TEMP_SUFFIX: &str = ".tmp";

/// Replaces `path` with `contents` so readers see either the old or the new
/// file, never a partial one: the bytes go to a temporary file in the same
/// directory, are flushed to disk, and the temporary file is renamed over the
/// target. The temporary file is removed if anything fails.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = temp_path(path)?;
    let result = (|| {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copies `from` over `to` atomically, see [`write_atomic`].
pub(crate) fn copy_atomic(from: &Path, to: &Path) -> io::Result<()> {
    let contents = fs::read(from)?;
    write_atomic(to, &contents)
}

/// Removes `path`; a missing file is not an error.
pub(crate) fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// A temporary path next to `path` that no other writer in this process uses.
fn temp_path(path: &Path) -> io::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} has no file name", path.display())))?;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.{n}{TEMP_SUFFIX}", std::process::id()));
    Ok(path.with_file_name(tmp_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.txt");
        write_atomic(&path, b"one").expect("first write");
        write_atomic(&path, b"two").expect("second write");
        assert_eq!(fs::read(&path).expect("read"), b"two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("file.txt")]);
    }

    #[test]
    fn failed_write_removes_the_temp_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Renaming a file over a non-empty directory fails on every platform.
        let target = dir.path().join("occupied");
        fs::create_dir(&target).expect("mkdir");
        fs::write(target.join("inner"), b"x").expect("write inner");
        assert!(write_atomic(&target, b"data").is_err());
        let names: Vec<_> = fs::read_dir(dir.path())
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("occupied")]);
    }
}
