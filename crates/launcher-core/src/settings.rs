//! The launcher's own settings, `launcher.json`, owned by an actor task.
//!
//! One task holds the settings and the node list and is the only writer of
//! the file, so updates from the UI, the tray and the node registry are
//! applied one at a time without locks. Readers get the current values from
//! `watch` channels.
//!
//! The file holds the launcher's settings at the top level and the nodes in
//! a `nodes` list ([`NodeSpec`]); the node registry (`crate::registry`) owns
//! that list and saves it here with [`SettingsHandle::save_nodes`].

use std::{fs, io, path::PathBuf};

use launcher_types::{DEFAULT_STOP_GRACE_SECS, Network, PortRole, Settings, is_loopback_http_url};
pub use launcher_types::{MAX_STOP_GRACE_SECS, MIN_STOP_GRACE_SECS, STOP_GRACE_SECS};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::{
    config, fsutil,
    nodes::{self, NodeListError, NodeSpec},
    paths::Paths,
};

/// Loading, validating or saving settings failed.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The Grafana URL is not `http://127.0.0.1` or `http://localhost`.
    #[error("the Grafana URL {0:?} must start with http://127.0.0.1 or http://localhost")]
    InvalidGrafanaUrl(String),
    /// The stop grace period is outside [`MIN_STOP_GRACE_SECS`]..=[`MAX_STOP_GRACE_SECS`].
    #[error(
        "the stop grace period of {0} s must be between {MIN_STOP_GRACE_SECS} and {MAX_STOP_GRACE_SECS} s"
    )]
    InvalidStopGrace(u64),
    /// The settings file could not be read or written.
    #[error("settings file I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The settings could not be serialised.
    #[error("settings could not be encoded: {0}")]
    Json(#[from] serde_json::Error),
    /// The node list breaks a rule every saved list keeps.
    #[error("the node list cannot be saved: {0}")]
    Nodes(#[from] NodeListError),
    /// The settings task has stopped.
    #[error("the settings store has stopped")]
    Closed,
}

/// Checks settings before they are saved: a loopback Grafana URL and a stop
/// grace period within bounds (the supervisor adds it to an `Instant`, and a
/// very short one would kill zebrad before it can stop).
pub fn validate(settings: &Settings) -> Result<(), SettingsError> {
    if let Some(url) = &settings.grafana_url
        && !is_loopback_http_url(url)
    {
        return Err(SettingsError::InvalidGrafanaUrl(url.clone()));
    }
    if !STOP_GRACE_SECS.contains(&settings.stop_grace_secs) {
        return Err(SettingsError::InvalidStopGrace(settings.stop_grace_secs));
    }
    Ok(())
}

/// Replaces `current` with settings sent by the UI (`set_settings`), keeping
/// the fields the UI may not set directly: `first_run_done` (only
/// `complete_first_run` sets it), `data_dir` (only `choose_data_dir` changes
/// it, after its checks) and `schema_version` (always the version this
/// launcher writes, so a stale copy cannot undo the migration marker).
pub fn apply_from_ui(current: &mut Settings, mut incoming: Settings) {
    incoming.first_run_done = current.first_run_done;
    incoming.data_dir = current.data_dir.take();
    incoming.schema_version = Settings::SCHEMA_VERSION;
    *current = incoming;
}

type Edit = Box<dyn FnOnce(&mut Settings) + Send>;

enum Msg {
    Update {
        edit: Edit,
        reply: oneshot::Sender<Result<Settings, SettingsError>>,
    },
    SaveNodes {
        nodes: Vec<NodeSpec>,
        reply: oneshot::Sender<Result<(), SettingsError>>,
    },
}

/// `launcher.json` as it is written: the settings, then the nodes.
#[derive(Serialize)]
struct FileOut<'a> {
    #[serde(flatten)]
    settings: &'a Settings,
    nodes: &'a [NodeSpec],
}

/// `launcher.json` as it is read. A file older than schema version 3 has no
/// `nodes`; its single node's settings are at the top level ([`LegacyNode`]).
#[derive(Deserialize)]
struct FileIn {
    #[serde(flatten)]
    settings: Settings,
    #[serde(default)]
    nodes: Vec<NodeSpec>,
}

/// The single node's settings as a file older than schema version 3 kept
/// them, at the top level; the defaults are those of that launcher.
#[derive(Deserialize)]
#[serde(default)]
struct LegacyNode {
    network: Network,
    data_dir: Option<String>,
    start_node_on_launch: bool,
    stop_grace_secs: u64,
}

impl Default for LegacyNode {
    fn default() -> Self {
        Self {
            network: Network::Mainnet,
            data_dir: None,
            start_node_on_launch: true,
            stop_grace_secs: DEFAULT_STOP_GRACE_SECS,
        }
    }
}

impl LegacyNode {
    /// The first node of a launcher that ran one: `zebrad-<network>`, with
    /// its files where they have always been (the same `zebrad.toml`, the
    /// same state folder) and the same ports, the P2P one as `zebrad.toml`
    /// gives it to zebrad.
    fn into_first_node(self, paths: &Paths) -> NodeSpec {
        let mut node = NodeSpec::first(self.network);
        node.data_dir = self.data_dir;
        node.autostart = self.start_node_on_launch;
        node.stop_grace_secs = if STOP_GRACE_SECS.contains(&self.stop_grace_secs) {
            self.stop_grace_secs
        } else {
            DEFAULT_STOP_GRACE_SECS
        };
        let p2p = config::read_p2p_port(&node.paths(paths), self.network);
        for port in &mut node.ports {
            if port.role == PortRole::P2p {
                port.port = p2p;
            }
        }
        node
    }
}

/// What a fresh install starts with: the default settings and the first node
/// on Mainnet, which the first run may move to Testnet.
fn fresh() -> (Settings, Vec<NodeSpec>) {
    (Settings::default(), vec![NodeSpec::first(Network::Mainnet)])
}

/// Starts the settings actor.
pub struct SettingsStore;

impl SettingsStore {
    /// Loads `launcher.json` and starts the actor task. Must be called inside a
    /// Tokio runtime.
    ///
    /// A missing file gives the defaults and the first node, on Mainnet. A
    /// corrupt file (one that does not parse, or whose node list breaks the
    /// rules of [`nodes::check_list`]) is logged, moved to
    /// `launcher.json.corrupt` (replacing an older backup) and replaced by the
    /// defaults on the next update.
    ///
    /// A file older than schema version 2 (or without a `schema_version`) was
    /// written by a launcher that had no first-run flow: it is migrated by
    /// setting `first_run_done`, so upgrading users are not sent through the
    /// first run again. A file older than version 3 ran a single node: its
    /// network, data directory, start at launch and stop grace become the
    /// first node ([`NodeSpec::first`]), whose files stay where they are.
    /// A migrated file is written back; if the write fails the migrated
    /// settings are still used and saved on the next update.
    pub fn open(paths: &Paths) -> Result<SettingsHandle, SettingsError> {
        let path = paths.settings_file();
        let (settings, nodes, migrated) = load(paths)?;
        if migrated {
            match encode(&settings, &nodes) {
                Ok(json) => match fsutil::write_atomic(&path, &json) {
                    Ok(()) => tracing::info!(
                        schema_version = settings.schema_version,
                        "migrated launcher.json"
                    ),
                    Err(err) => {
                        tracing::warn!(%err, "could not write the migrated launcher.json")
                    }
                },
                Err(err) => tracing::warn!(%err, "could not encode the migrated launcher.json"),
            }
        }
        let (watch_tx, watch_rx) = watch::channel(settings);
        let (nodes_tx, nodes_rx) = watch::channel(nodes);
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(run(path, rx, watch_tx, nodes_tx));
        Ok(SettingsHandle {
            tx,
            watch: watch_rx,
            nodes: nodes_rx,
        })
    }
}

/// Reads and updates the settings. Cheap to clone; the actor stops when the
/// last handle is dropped.
#[derive(Clone, Debug)]
pub struct SettingsHandle {
    tx: mpsc::Sender<Msg>,
    watch: watch::Receiver<Settings>,
    nodes: watch::Receiver<Vec<NodeSpec>>,
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
            .send(Msg::Update {
                edit: Box::new(f),
                reply,
            })
            .await
            .map_err(|_| SettingsError::Closed)?;
        response.await.map_err(|_| SettingsError::Closed)?
    }

    /// The node list, as loaded or last saved.
    pub fn nodes(&self) -> Vec<NodeSpec> {
        self.nodes.borrow().clone()
    }

    /// Saves `nodes` as the node list, next to the current settings. Refused
    /// unless the list keeps the rules of [`nodes::check_list`]; on error
    /// nothing is saved.
    pub async fn save_nodes(&self, nodes: Vec<NodeSpec>) -> Result<(), SettingsError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(Msg::SaveNodes { nodes, reply })
            .await
            .map_err(|_| SettingsError::Closed)?;
        response.await.map_err(|_| SettingsError::Closed)?
    }
}

async fn run(
    path: PathBuf,
    mut rx: mpsc::Receiver<Msg>,
    watch: watch::Sender<Settings>,
    nodes_watch: watch::Sender<Vec<NodeSpec>>,
) {
    while let Some(msg) = rx.recv().await {
        // The caller may have given up waiting; a change is saved regardless.
        match msg {
            Msg::Update { edit, reply } => {
                let mut next = watch.borrow().clone();
                edit(&mut next);
                let nodes = nodes_watch.borrow().clone();
                let result = save(&path, &next, &nodes).await.map(|()| {
                    watch.send_replace(next.clone());
                    next
                });
                let _ = reply.send(result);
            }
            Msg::SaveNodes { nodes, reply } => {
                let settings = watch.borrow().clone();
                let result = match nodes::check_list(&nodes) {
                    Ok(()) => save(&path, &settings, &nodes).await.map(|()| {
                        nodes_watch.send_replace(nodes);
                    }),
                    Err(err) => Err(err.into()),
                };
                let _ = reply.send(result);
            }
        }
    }
}

/// The file's bytes for `settings` and `nodes`.
fn encode(settings: &Settings, nodes: &[NodeSpec]) -> Result<Vec<u8>, serde_json::Error> {
    let mut json = serde_json::to_vec_pretty(&FileOut { settings, nodes })?;
    json.push(b'\n');
    Ok(json)
}

async fn save(
    path: &std::path::Path,
    settings: &Settings,
    nodes: &[NodeSpec],
) -> Result<(), SettingsError> {
    validate(settings)?;
    let json = encode(settings, nodes)?;
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

/// Reads a file's settings and nodes and brings them up to
/// [`Settings::SCHEMA_VERSION`]. Returns whether anything changed, or why the
/// file counts as corrupt.
fn parse(
    bytes: &[u8],
    paths: &Paths,
) -> Result<(Settings, Vec<NodeSpec>, bool), Box<dyn std::error::Error>> {
    let stored_version = stored_schema_version(bytes);
    let FileIn {
        mut settings,
        mut nodes,
    } = serde_json::from_slice(bytes)?;
    sanitize(&mut settings);
    let mut migrated = false;
    if stored_version < 2 {
        // Version 1 predates the first-run flow: whoever wrote the file has
        // already been using the launcher.
        settings.first_run_done = true;
        migrated = true;
    }
    if stored_version < 3 {
        // Up to version 2 the launcher ran one node, whose settings were the
        // launcher's.
        let legacy: LegacyNode = serde_json::from_slice(bytes)?;
        nodes = vec![legacy.into_first_node(paths)];
        migrated = true;
    } else {
        for node in &mut nodes {
            sanitize_node(node);
        }
        if nodes.is_empty() {
            tracing::warn!("launcher.json lists no nodes; adding the first one");
            nodes = fresh().1;
            migrated = true;
        }
    }
    nodes::check_list(&nodes)?;
    if migrated {
        settings.schema_version = Settings::SCHEMA_VERSION;
    }
    Ok((settings, nodes, migrated))
}

/// The settings and nodes in `launcher.json` and whether they were migrated.
fn load(paths: &Paths) -> Result<(Settings, Vec<NodeSpec>, bool), SettingsError> {
    let path = paths.settings_file();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let (settings, nodes) = fresh();
            return Ok((settings, nodes, false));
        }
        Err(err) => return Err(err.into()),
    };
    match parse(&bytes, paths) {
        Ok(loaded) => Ok(loaded),
        Err(err) => {
            let backup = corrupt_path(&path);
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
                fsutil::remove_if_exists(&backup).and_then(|()| fs::rename(&path, &backup))
            {
                tracing::warn!(
                    %err,
                    backup = %backup.display(),
                    "could not move the corrupt launcher.json aside"
                );
            }
            let (settings, nodes) = fresh();
            Ok((settings, nodes, false))
        }
    }
}

/// Replaces the values of a loaded node that [`nodes::check_list`] would
/// refuse with their defaults, so a hand-edited file still loads.
fn sanitize_node(node: &mut NodeSpec) {
    if !STOP_GRACE_SECS.contains(&node.stop_grace_secs) {
        tracing::warn!(
            node = %node.id,
            secs = node.stop_grace_secs,
            "launcher.json has a stop grace period out of range; using the default"
        );
        node.stop_grace_secs = DEFAULT_STOP_GRACE_SECS;
    }
    if node.name.trim().is_empty() {
        tracing::warn!(node = %node.id, "launcher.json has a node without a name; using the default");
        node.name = launcher_types::NodeInfo::default_name(node.kind, node.network);
    }
}

/// Replaces the values of a loaded file that [`validate`] would refuse with
/// their defaults, so a hand-edited or older `launcher.json` still loads.
fn sanitize(settings: &mut Settings) {
    if let Some(url) = &settings.grafana_url
        && !is_loopback_http_url(url)
    {
        tracing::warn!(%url, "launcher.json has an invalid Grafana URL; ignoring it");
        settings.grafana_url = None;
    }
    if !STOP_GRACE_SECS.contains(&settings.stop_grace_secs) {
        let default = Settings::default().stop_grace_secs;
        tracing::warn!(
            secs = settings.stop_grace_secs,
            default,
            "launcher.json has a stop grace period out of range; using the default"
        );
        settings.stop_grace_secs = default;
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

    use launcher_types::{NodeKind, UpdateMode};
    use tokio::time::timeout;

    use super::*;
    use crate::{config::ManagedKeys, nodes::NodeLayout};

    const T: Duration = Duration::from_secs(10);

    fn paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(dir.path());
        (dir, paths)
    }

    #[test]
    fn ui_settings_keep_the_data_dir_first_run_marker_and_schema_version() {
        let mut current = Settings {
            data_dir: Some("/chosen".into()),
            first_run_done: true,
            ..Settings::default()
        };
        let stale = Settings {
            schema_version: 0,
            data_dir: None,
            first_run_done: false,
            network: Network::Testnet,
            ..Settings::default()
        };
        apply_from_ui(&mut current, stale);
        assert_eq!(current.data_dir.as_deref(), Some("/chosen"));
        assert!(current.first_run_done);
        assert_eq!(current.schema_version, Settings::SCHEMA_VERSION);
        assert_eq!(current.network, Network::Testnet);
    }

    #[test]
    fn the_stop_grace_must_be_within_bounds() {
        for secs in [MIN_STOP_GRACE_SECS, 60, MAX_STOP_GRACE_SECS] {
            let settings = Settings {
                stop_grace_secs: secs,
                ..Settings::default()
            };
            validate(&settings).unwrap_or_else(|err| panic!("{secs}: {err}"));
        }
        for secs in [
            0,
            MIN_STOP_GRACE_SECS - 1,
            MAX_STOP_GRACE_SECS + 1,
            u64::MAX,
        ] {
            let settings = Settings {
                stop_grace_secs: secs,
                ..Settings::default()
            };
            assert!(
                matches!(validate(&settings), Err(SettingsError::InvalidStopGrace(s)) if s == secs),
                "{secs}"
            );
        }
    }

    #[tokio::test]
    async fn an_out_of_range_stop_grace_is_rejected_and_loads_as_the_default() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        let err = timeout(T, store.update(|s| s.stop_grace_secs = 0))
            .await
            .expect("in time")
            .expect_err("zero grace");
        assert!(matches!(err, SettingsError::InvalidStopGrace(0)), "{err:?}");
        assert_eq!(store.current(), Settings::default());
        drop(store);

        // A hand-edited file keeps loading, with the default grace.
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        let edited = br#"{ "schema_version": 2, "stop_grace_secs": 18446744073709551615, "network": "testnet" }"#;
        fs::write(paths.settings_file(), edited).expect("write");
        let store = SettingsStore::open(&paths).expect("open");
        let current = store.current();
        assert_eq!(current.stop_grace_secs, Settings::default().stop_grace_secs);
        assert_eq!(current.network, Network::Testnet, "the rest is kept");
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
            assert_eq!(stored_schema_version(&written), 3);
            assert_eq!(store.nodes().len(), 1);
            assert_eq!(store.nodes()[0].id.as_str(), "zebrad-testnet");
        }
    }

    #[tokio::test]
    async fn schema_2_files_and_new_installs_keep_the_first_run() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        assert!(!store.current().first_run_done);
        assert!(!paths.settings_file().exists(), "nothing to migrate");
        // A fresh install starts with the first node, on Mainnet.
        assert_eq!(store.nodes(), [NodeSpec::first(Network::Mainnet)]);
        drop(store);

        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        let current = br#"{ "schema_version": 2, "first_run_done": false }"#;
        fs::write(paths.settings_file(), current).expect("write");
        let store = SettingsStore::open(&paths).expect("open");
        assert!(
            !store.current().first_run_done,
            "the first run is still due"
        );
        let written = fs::read(paths.settings_file()).expect("read");
        assert_eq!(
            stored_schema_version(&written),
            3,
            "but the file has the node list"
        );
    }

    /// A version 2 `launcher.json`, and the `zebrad.toml` the launcher that
    /// wrote it kept up to date, under `paths`.
    fn version_2_install(paths: &Paths, launcher_json: &str, zebrad_toml: Option<&str>) {
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        fs::write(paths.settings_file(), launcher_json).expect("write launcher.json");
        if let Some(text) = zebrad_toml {
            fs::write(paths.launcher_node().zebrad_config(), text).expect("write zebrad.toml");
        }
    }

    #[tokio::test]
    async fn a_version_2_file_becomes_one_node_that_keeps_its_files_and_ports() {
        let (dir, paths) = paths();
        let chosen = dir.path().join("big disk");
        // What a version 2 launcher wrote for a Testnet node in a chosen folder.
        let keys = ManagedKeys::defaults(
            Network::Testnet,
            &paths.launcher_node(),
            chosen.join("state"),
        );
        let zebrad_toml = config::render_minimal(&keys).expect("render");
        let launcher_json = serde_json::json!({
            "schema_version": 2,
            "update_mode": "notify",
            "close_to_tray": false,
            "start_node_on_launch": false,
            "grafana_url": null,
            "data_dir": chosen.to_str().expect("utf-8"),
            "network": "testnet",
            "stop_grace_secs": 90,
            "first_run_done": true,
            "theme": "dark"
        })
        .to_string();
        version_2_install(&paths, &launcher_json, Some(&zebrad_toml));

        let store = SettingsStore::open(&paths).expect("open");
        let settings = store.current();
        assert_eq!(settings.schema_version, 3);
        assert_eq!(settings.update_mode, UpdateMode::Notify);
        assert!(!settings.close_to_tray);
        assert!(settings.first_run_done);
        let [node] = store.nodes().try_into().expect("one node");
        assert_eq!(node.id.as_str(), "zebrad-testnet");
        assert_eq!(node.name, "Zebra · Testnet");
        assert_eq!(node.network, Network::Testnet);
        assert_eq!(node.layout, NodeLayout::Launcher);
        assert!(!node.autostart);
        assert_eq!(node.stop_grace_secs, 90);
        assert_eq!(node.data_dir.as_deref(), chosen.to_str());
        assert_eq!(
            node.ports,
            NodeKind::Zebrad.default_ports(Network::Testnet),
            "the ports a single Testnet node has always used"
        );
        // The same files: zebrad.toml, cookie, pid file and log stay where
        // they are, and the state is in the chosen folder.
        assert_eq!(node.paths(&paths), paths.launcher_node());
        assert_eq!(node.state_dir(&paths), chosen.join("state"));
        // Nothing in zebrad.toml changes: the managed keys are the ones it has.
        assert_eq!(node.managed_keys(&paths), keys);
        config::ensure(&node.paths(&paths), &node.managed_keys(&paths)).expect("ensure");
        assert_eq!(
            fs::read_to_string(paths.launcher_node().zebrad_config()).expect("read"),
            zebrad_toml
        );
        // The migration is written back once, as version 3.
        let written = fs::read(paths.settings_file()).expect("read");
        assert_eq!(stored_schema_version(&written), 3);
        drop(store);
        let reopened = SettingsStore::open(&paths).expect("reopen");
        assert_eq!(reopened.nodes(), [node]);
        assert_eq!(
            fs::read(paths.settings_file()).expect("read"),
            written,
            "a version 3 file is read as it is"
        );
    }

    #[tokio::test]
    async fn the_migrated_node_keeps_the_p2p_port_its_zebrad_toml_sets() {
        let (_dir, paths) = paths();
        version_2_install(
            &paths,
            r#"{ "schema_version": 2, "first_run_done": true }"#,
            Some("[network]\nnetwork = \"Mainnet\"\nlisten_addr = \"0.0.0.0:28233\"\n"),
        );
        let store = SettingsStore::open(&paths).expect("open");
        let [node] = store.nodes().try_into().expect("one node");
        assert_eq!(node.id.as_str(), "zebrad-mainnet");
        assert_eq!(node.port(PortRole::P2p), Some(28233));
        assert_eq!(node.port(PortRole::Rpc), Some(8232));
        assert_eq!(node.port(PortRole::Metrics), Some(9999));
        assert!(
            node.autostart,
            "a version 2 file without the key started the node"
        );
    }

    #[tokio::test]
    async fn the_node_list_and_the_settings_are_saved_together() {
        let (_dir, paths) = paths();
        let store = SettingsStore::open(&paths).expect("open");
        let mut nodes = store.nodes();
        nodes.push(NodeSpec {
            id: launcher_types::NodeId::new("zebrad-testnet").expect("id"),
            layout: NodeLayout::Folder,
            ..NodeSpec::first(Network::Testnet)
        });
        timeout(T, store.save_nodes(nodes.clone()))
            .await
            .expect("in time")
            .expect("save the nodes");
        timeout(T, store.update(|s| s.close_to_tray = false))
            .await
            .expect("in time")
            .expect("save a setting");
        assert_eq!(store.nodes(), nodes, "a settings update keeps the nodes");
        // A list that breaks the rules is refused and nothing changes.
        let mut twice = nodes.clone();
        twice.push(nodes[1].clone());
        let err = timeout(T, store.save_nodes(twice))
            .await
            .expect("in time")
            .expect_err("a duplicate id");
        assert!(matches!(err, SettingsError::Nodes(_)), "{err:?}");
        let err = timeout(T, store.save_nodes(Vec::new()))
            .await
            .expect("in time")
            .expect_err("no nodes");
        assert!(
            matches!(err, SettingsError::Nodes(NodeListError::Empty)),
            "{err:?}"
        );
        drop(store);

        let reopened = SettingsStore::open(&paths).expect("reopen");
        assert_eq!(reopened.nodes(), nodes);
        assert!(!reopened.current().close_to_tray);
    }

    #[tokio::test]
    async fn a_node_list_that_breaks_the_rules_counts_as_corrupt() {
        let (_dir, paths) = paths();
        let node = serde_json::to_value(NodeSpec::first(Network::Mainnet)).expect("json");
        let twice = serde_json::json!({
            "schema_version": 3,
            "first_run_done": true,
            "nodes": [node, node]
        })
        .to_string();
        version_2_install(&paths, &twice, None);
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.current(), Settings::default());
        assert_eq!(store.nodes(), [NodeSpec::first(Network::Mainnet)]);
        assert_eq!(
            fs::read_to_string(corrupt_path(&paths.settings_file())).expect("backup"),
            twice
        );
        drop(store);

        // An empty list gets the first node back, and a hand-edited stop grace
        // out of range loads as the default.
        let empty = serde_json::json!({ "schema_version": 3, "nodes": [] }).to_string();
        version_2_install(&paths, &empty, None);
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.nodes(), [NodeSpec::first(Network::Mainnet)]);
        drop(store);
        let mut hasty = serde_json::to_value(NodeSpec::first(Network::Testnet)).expect("json");
        hasty["stop_grace_secs"] = serde_json::json!(1);
        let edited = serde_json::json!({ "schema_version": 3, "nodes": [hasty] }).to_string();
        version_2_install(&paths, &edited, None);
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.nodes()[0].stop_grace_secs, DEFAULT_STOP_GRACE_SECS);
        assert_eq!(
            store.nodes()[0].network,
            Network::Testnet,
            "the rest is kept"
        );
    }

    #[tokio::test]
    async fn corrupt_file_is_backed_up() {
        let (_dir, paths) = paths();
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        fs::write(paths.settings_file(), b"{ not json").expect("write");
        let store = SettingsStore::open(&paths).expect("open");
        assert_eq!(store.current(), Settings::default());
        assert_eq!(store.nodes(), [NodeSpec::first(Network::Mainnet)]);
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
