//! The public services' files, under `<app data>/public/`: the ACME accounts in `acme/`, per
//! name `certs/<name>/` (`cert.pem`, `key.pem`, `attempts.json`), and `state.json` with the
//! latest evidence and the month's upload. Every write is atomic, and keys and accounts are
//! owner-only on Unix.

use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use launcher_types::public::Evidence;
use serde::{Deserialize, Serialize};

/// Writes `contents` to `path` atomically, readable by the owner only on Unix: a temporary
/// file in the same directory, flushed, then renamed over the target.
pub fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    write(path, contents, true)
}

/// [`write_private`] for files others may read.
pub fn write_public(path: &Path, contents: &[u8]) -> io::Result<()> {
    write(path, contents, false)
}

fn write(path: &Path, contents: &[u8], private: bool) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no file name"))?
        .to_string_lossy()
        .into_owned();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if private { 0o600 } else { 0o644 });
        }
        #[cfg(not(unix))]
        let _ = private;
        let mut file = options.open(&tmp)?;
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

/// Whether others can read `path`: a key like that is refused.
pub fn readable_by_others(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// The public services' directory layout.
#[derive(Clone, Debug)]
pub struct PublicPaths {
    root: PathBuf,
}

impl PublicPaths {
    /// The layout under `data_dir/public`.
    pub fn new(data_dir: &Path) -> Self {
        PublicPaths {
            root: data_dir.join("public"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The ACME account for `environment` (`staging` or `production`).
    pub fn account(&self, environment: &str) -> PathBuf {
        self.root.join("acme").join(format!("{environment}.json"))
    }

    pub fn cert_dir(&self, name: &str) -> PathBuf {
        self.root.join("certs").join(name)
    }

    pub fn cert(&self, name: &str) -> PathBuf {
        self.cert_dir(name).join("cert.pem")
    }

    pub fn key(&self, name: &str) -> PathBuf {
        self.cert_dir(name).join("key.pem")
    }

    pub fn attempts(&self, name: &str) -> PathBuf {
        self.cert_dir(name).join("attempts.json")
    }

    pub fn state(&self) -> PathBuf {
        self.root.join("state.json")
    }

    /// Token files when the keychain is not used.
    pub fn secrets(&self) -> PathBuf {
        self.root.join("secrets")
    }
}

/// `state.json`: what the service actor keeps between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// The newest evidence of each kind.
    pub evidence: Vec<Evidence>,
    /// The month `month_bytes_up` counts, as `YYYY-MM`.
    pub month: String,
    /// Bytes sent to wallets this month, before this run.
    pub month_bytes_up: u64,
    /// The last staging validation through the forward.
    pub staging_validated_at: Option<std::time::SystemTime>,
}

/// Reads `path` as JSON; a missing or unreadable file gives the default.
pub fn load<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Writes `value` to `path` as JSON, atomically.
pub fn save<T: Serialize>(path: &Path, value: &T, private: bool) -> io::Result<()> {
    let mut json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    json.push(b'\n');
    write(path, &json, private)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_files_are_owner_only_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/key.pem");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!readable_by_others(&path));
        let names: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("key.pem")]);
        #[cfg(unix)]
        {
            write_public(&path, b"three").unwrap();
            assert!(readable_by_others(&path));
        }
    }

    #[test]
    fn state_loads_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        assert_eq!(load::<State>(&path), State::default());
        let state = State {
            month: "2026-11".into(),
            month_bytes_up: 42,
            ..State::default()
        };
        save(&path, &state, false).unwrap();
        assert_eq!(load::<State>(&path), state);
    }
}
