//! The launcher's own settings, `launcher.json`, owned by an actor task.
//!
//! One task holds the settings and is the only writer of the file, so updates
//! from the UI, the tray and the supervisor are applied one at a time without
//! locks. Readers get the current value from a `watch` channel.

use std::{fs, io, path::PathBuf};

use launcher_types::{Settings, is_loopback_http_url};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{fsutil, paths::Paths};

/// Loading, validating or saving settings failed.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The Grafana URL is not a loopback `http(s)` URL.
    #[error(
        "the Grafana URL {0:?} must start with http:// or https:// and point to 127.0.0.1, localhost or [::1]"
    )]
    InvalidGrafanaUrl(String),
    /// The settings file could not be read or written.
    #[error("settings file I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The settings could not be serialised.
    #[error("settings could not be encoded: {0}")]
    Json(#[from] serde_json::Error),
    /// The settings task has stopped.
    #[error("the settings store has stopped")]
    Closed,
}

/// Checks settings before they are saved.
pub fn validate(settings: &Settings) -> Result<(), SettingsError> {
    if let Some(url) = &settings.grafana_url
        && !is_loopback_http_url(url)
    {
        return Err(SettingsError::InvalidGrafanaUrl(url.clone()));
    }
    Ok(())
}

type Edit = Box<dyn FnOnce(&mut Settings) + Send>;

struct Update {
    edit: Edit,
    reply: oneshot::Sender<Result<Settings, SettingsError>>,
}

/// Starts the settings actor.
pub struct SettingsStore;

impl SettingsStore {
    /// Loads `launcher.json` and starts the actor task. Must be called inside a
    /// Tokio runtime.
    ///
    /// A missing file gives the defaults. A corrupt file is logged, moved to
    /// `launcher.json.corrupt` (replacing an older backup) and replaced by the
    /// defaults on the next update.
    pub fn open(paths: &Paths) -> Result<SettingsHandle, SettingsError> {
        let path = paths.settings_file();
        let settings = load(&path)?;
        let (watch_tx, watch_rx) = watch::channel(settings);
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(run(path, rx, watch_tx));
        Ok(SettingsHandle {
            tx,
            watch: watch_rx,
        })
    }
}

/// Reads and updates the settings. Cheap to clone; the actor stops when the
/// last handle is dropped.
#[derive(Clone, Debug)]
pub struct SettingsHandle {
    tx: mpsc::Sender<Update>,
    watch: watch::Receiver<Settings>,
}

impl SettingsHandle {
    /// The current settings.
    pub fn current(&self) -> Settings {
        self.watch.borrow().clone()
    }

    /// A receiver that sees every saved change.
    pub fn watch(&self) -> watch::Receiver<Settings> {
        self.watch.clone()
    }

    /// Applies `f` to the current settings, validates the result and saves it.
    ///
    /// Updates are applied in the order they reach the actor, each on top of the
    /// previous one. On error nothing is saved or published.
    pub async fn update(
        &self,
        f: impl FnOnce(&mut Settings) + Send + 'static,
    ) -> Result<Settings, SettingsError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Update {
                edit: Box::new(f),
                reply,
            })
            .await
            .map_err(|_| SettingsError::Closed)?;
        response.await.map_err(|_| SettingsError::Closed)?
    }
}

async fn run(path: PathBuf, mut rx: mpsc::Receiver<Update>, watch: watch::Sender<Settings>) {
    while let Some(Update { edit, reply }) = rx.recv().await {
        let mut next = watch.borrow().clone();
        edit(&mut next);
        let result = save(&path, &next).await.map(|()| {
            watch.send_replace(next.clone());
            next
        });
        // The caller may have given up waiting; the change is saved regardless.
        let _ = reply.send(result);
    }
}

async fn save(path: &std::path::Path, settings: &Settings) -> Result<(), SettingsError> {
    validate(settings)?;
    let mut json = serde_json::to_vec_pretty(settings)?;
    json.push(b'\n');
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fsutil::write_atomic(&path, &json)
    })
    .await
    .map_err(io::Error::other)??;
    Ok(())
}

fn load(path: &std::path::Path) -> Result<Settings, SettingsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(err) => return Err(err.into()),
    };
    match serde_json::from_slice::<Settings>(&bytes) {
        Ok(settings) if validate(&settings).is_ok() => Ok(settings),
        Ok(settings) => {
            tracing::warn!(
                url = ?settings.grafana_url,
                "launcher.json has an invalid Grafana URL; ignoring it"
            );
            Ok(Settings {
                grafana_url: None,
                ..settings
            })
        }
        Err(err) => {
            let backup = corrupt_path(path);
            tracing::warn!(
                %err,
                backup = %backup.display(),
                "launcher.json is corrupt; using default settings"
            );
            // A backup from an earlier corruption is replaced; renaming over
            // an existing file fails on Windows. If the file cannot be moved
            // aside, the defaults are still used and the next update
            // overwrites it.
            if let Err(err) =
                fsutil::remove_if_exists(&backup).and_then(|()| fs::rename(path, &backup))
            {
                tracing::warn!(
                    %err,
                    backup = %backup.display(),
                    "could not move the corrupt launcher.json aside"
                );
            }
            Ok(Settings::default())
        }
    }
}

fn corrupt_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".corrupt");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use launcher_types::{Network, UpdateMode};
    use tokio::time::timeout;

    use super::*;

    const T: Duration = Duration::from_secs(10);

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        (dir, paths)
    }

    #[tokio::test]
    async fn defaults_when_missing() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.current(), Settings::default());
        assert!(!paths.settings_file().exists());
    }

    #[tokio::test]
    async fn round_trip() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        let mut watch = store.watch();
        let saved = timeout(
            T,
            store.update(|s| {
                s.network = Network::Testnet;
                s.update_mode = UpdateMode::Notify;
                s.grafana_url = Some("http://127.0.0.1:3000/d/zebra".into());
                s.data_dir = Some("/big/disk".into());
            }),
        )
        .await
        .expect("in time")
        .expect("update");
        assert_eq!(saved.network, Network::Testnet);
        timeout(T, watch.changed())
            .await
            .expect("in time")
            .expect("changed");
        assert_eq!(*watch.borrow(), saved);
        assert_eq!(store.current(), saved);
        drop(store);

        let reopened = SettingsStore::open(&paths).expect("reopen");
        assert_eq!(reopened.current(), saved);
    }

    #[tokio::test]
    async fn corrupt_file_is_backed_up() {
        let (_dir, paths) = paths();
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        fs::write(paths.settings_file(), b"{ not json").expect("write");
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.current(), Settings::default());
        assert!(!paths.settings_file().exists());
        assert_eq!(
            fs::read(corrupt_path(&paths.settings_file())).expect("backup"),
            b"{ not json"
        );
        timeout(T, store.update(|s| s.close_to_tray = false))
            .await
            .expect("in time")
            .expect("update");
        assert!(paths.settings_file().exists());
    }

    #[tokio::test]
    async fn repeated_corruption_replaces_the_backup() {
        let (_dir, paths) = paths();
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        let backup = corrupt_path(&paths.settings_file());
        for content in [&b"{ first"[..], &b"{ second"[..]] {
            fs::write(paths.settings_file(), content).expect("write");
            let store = SettingsStore::open(&paths).expect("open");
            assert_eq!(store.current(), Settings::default());
            assert!(!paths.settings_file().exists());
            assert_eq!(fs::read(&backup).expect("backup"), content);
        }
    }

    #[tokio::test]
    async fn corrupt_file_that_cannot_be_moved_still_gives_defaults() {
        let (_dir, paths) = paths();
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        // A non-empty directory where the backup goes cannot be removed or
        // renamed over.
        let backup = corrupt_path(&paths.settings_file());
        fs::create_dir(&backup).expect("mkdir backup");
        fs::write(backup.join("inner"), b"x").expect("write inner");
        fs::write(paths.settings_file(), b"{ not json").expect("write");
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.current(), Settings::default());
        timeout(T, store.update(|s| s.close_to_tray = false))
            .await
            .expect("in time")
            .expect("update replaces the corrupt file");
        assert!(
            !fs::read_to_string(paths.settings_file())
                .expect("read")
                .contains("not json")
        );
    }

    #[tokio::test]
    async fn invalid_grafana_url_is_rejected() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        let err = timeout(
            T,
            store.update(|s| s.grafana_url = Some("http://grafana.example.com".into())),
        )
        .await
        .expect("in time")
        .expect_err("non-loopback URL");
        assert!(
            matches!(err, SettingsError::InvalidGrafanaUrl(_)),
            "{err:?}"
        );
        assert_eq!(store.current(), Settings::default());
        assert!(!paths.settings_file().exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_serialise() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        let mut tasks = Vec::new();
        for _ in 0..50 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store.update(|s| s.stop_grace_secs += 1).await
            }));
        }
        for task in tasks {
            timeout(T, task)
                .await
                .expect("in time")
                .expect("task")
                .expect("update");
        }
        assert_eq!(store.current().stop_grace_secs, 110);
        let on_disk: Settings =
            serde_json::from_slice(&fs::read(paths.settings_file()).expect("read")).expect("json");
        assert_eq!(on_disk.stop_grace_secs, 110);
        let names: Vec<_> = fs::read_dir(paths.config_dir())
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from("launcher.json")]);
    }
}
