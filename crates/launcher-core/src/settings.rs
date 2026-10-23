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
    /// The Grafana URL is not `http://127.0.0.1` or `http://localhost`.
    #[error("the Grafana URL {0:?} must start with http://127.0.0.1 or http://localhost")]
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

/// Replaces `current` with settings sent by the UI (`set_settings`), keeping
/// the fields the UI may not set directly: `first_run_done` (only
/// `complete_first_run` sets it) and `data_dir` (only `choose_data_dir`
/// changes it, after its checks).
pub fn apply_from_ui(current: &mut Settings, mut incoming: Settings) {
    incoming.first_run_done = current.first_run_done;
    incoming.data_dir = current.data_dir.take();
    *current = incoming;
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
    ///
    /// A file older than schema version 2 (or without a `schema_version`) was
    /// written by a launcher that had no first-run flow: it is migrated by
    /// setting `first_run_done`, so upgrading users are not sent through the
    /// first run again, and written back. If the write fails the migrated
    /// settings are still used and saved on the next update.
    pub fn open(paths: &Paths) -> Result<SettingsHandle, SettingsError> {
        let path = paths.settings_file();
        let (settings, migrated) = load(&path)?;
        if migrated {
            match serde_json::to_vec_pretty(&settings) {
                Ok(mut json) => {
                    json.push(b'\n');
                    match fsutil::write_atomic(&path, &json) {
                        Ok(()) => tracing::info!(
                            schema_version = settings.schema_version,
                            "migrated launcher.json"
                        ),
                        Err(err) => {
                            tracing::warn!(%err, "could not write the migrated launcher.json")
                        }
                    }
                }
                Err(err) => tracing::warn!(%err, "could not encode the migrated launcher.json"),
            }
        }
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

/// The schema version a `launcher.json` was written with; files from before
/// `schema_version` existed count as version 0. `Settings` cannot tell, as a
/// missing field takes the current version.
fn stored_schema_version(bytes: &[u8]) -> u64 {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| value.get("schema_version")?.as_u64())
        .unwrap_or(0)
}

/// Brings settings loaded from an existing file up to
/// [`Settings::SCHEMA_VERSION`]. Returns whether anything changed.
fn migrate(settings: &mut Settings, stored_version: u64) -> bool {
    if stored_version >= 2 {
        return false;
    }
    // Version 1 predates the first-run flow: whoever wrote the file has
    // already been using the launcher.
    settings.first_run_done = true;
    settings.schema_version = Settings::SCHEMA_VERSION;
    true
}

/// The settings in `path` and whether they were migrated.
fn load(path: &std::path::Path) -> Result<(Settings, bool), SettingsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok((Settings::default(), false));
        }
        Err(err) => return Err(err.into()),
    };
    let stored_version = stored_schema_version(&bytes);
    let with_migration = |mut settings: Settings| {
        let migrated = migrate(&mut settings, stored_version);
        (settings, migrated)
    };
    match serde_json::from_slice::<Settings>(&bytes) {
        Ok(settings) if validate(&settings).is_ok() => Ok(with_migration(settings)),
        Ok(settings) => {
            tracing::warn!(
                url = ?settings.grafana_url,
                "launcher.json has an invalid Grafana URL; ignoring it"
            );
            Ok(with_migration(Settings {
                grafana_url: None,
                ..settings
            }))
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
            Ok((Settings::default(), false))
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

    #[test]
    fn ui_settings_keep_the_data_dir_and_first_run_marker() {
        let mut current = Settings {
            data_dir: Some("/chosen".into()),
            first_run_done: true,
            ..Settings::default()
        };
        let stale = Settings {
            data_dir: None,
            first_run_done: false,
            network: Network::Testnet,
            ..Settings::default()
        };
        apply_from_ui(&mut current, stale);
        assert_eq!(current.data_dir.as_deref(), Some("/chosen"));
        assert!(current.first_run_done);
        assert_eq!(current.network, Network::Testnet);
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
    async fn files_before_schema_2_are_migrated() {
        for old in [
            &br#"{ "schema_version": 1, "start_node_on_launch": true, "network": "testnet" }"#[..],
            &br#"{ "start_node_on_launch": true, "network": "testnet" }"#[..],
        ] {
            let (_dir, paths) = paths();
            fs::create_dir_all(paths.config_dir()).expect("mkdir");
            fs::write(paths.settings_file(), old).expect("write");
            let store = SettingsStore::open(&paths).expect("open");
            let current = store.current();
            assert!(current.first_run_done, "upgrading users skip the first run");
            assert!(current.start_node_on_launch);
            assert_eq!(current.network, Network::Testnet);
            assert_eq!(current.schema_version, Settings::SCHEMA_VERSION);
            let written = fs::read(paths.settings_file()).expect("read");
            let on_disk: Settings = serde_json::from_slice(&written).expect("json");
            assert_eq!(on_disk, current, "the migration is written back");
            assert_eq!(stored_schema_version(&written), 2);
        }
    }

    #[tokio::test]
    async fn schema_2_files_and_new_installs_keep_the_first_run() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        assert!(!store.current().first_run_done);
        assert!(!paths.settings_file().exists(), "nothing to migrate");
        drop(store);

        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        let current = br#"{ "schema_version": 2, "first_run_done": false }"#;
        fs::write(paths.settings_file(), current).expect("write");
        let store = SettingsStore::open(&paths).expect("open");
        assert!(!store.current().first_run_done);
        assert_eq!(fs::read(paths.settings_file()).expect("read"), current);
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
