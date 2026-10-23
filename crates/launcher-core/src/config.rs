//! The launcher-managed `zebrad.toml`.
//!
//! The file is minimal: the keys the launcher manages plus whatever the user
//! added, never the full `zebrad generate` output. zebrad rejects unknown
//! fields, so a defaulted key copied from an older generate output would break
//! the node as soon as a newer zebrad drops it. Edits go through
//! [`toml_edit`], which keeps the user's comments and layout.
//!
//! Saving from the editor is a small protocol with rollback:
//! [`begin_apply`] checks the compare-and-swap token from [`read`], validates
//! the text, keeps the current file as `zebrad.toml.bak`, writes a
//! `zebrad.toml.pending` marker and replaces the file atomically. The caller
//! restarts zebrad and then calls [`PendingApply::commit`] or
//! [`PendingApply::rollback`]. If the launcher dies in between, [`recover`]
//! rolls back on the next start.

use std::{
    fs,
    hash::{DefaultHasher, Hasher},
    io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

pub use launcher_types::ConfigDocument;
use launcher_types::Network;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::{fsutil, paths::Paths};

/// The default Mainnet RPC port.
pub const MAINNET_RPC_PORT: u16 = 8232;
/// The default Testnet RPC port.
pub const TESTNET_RPC_PORT: u16 = 18232;
/// The default Prometheus metrics port.
pub const METRICS_PORT: u16 = 9999;

/// The comment at the top of a newly created `zebrad.toml`.
const HEADER: &str = "\
# zebrad configuration managed by Zebra Launcher.
#
# The launcher keeps network.network, rpc.listen_addr, rpc.enable_cookie_auth,
# rpc.cookie_dir, metrics.endpoint_addr and state.cache_dir up to date.
# Add your own settings below; the reference shows every option with its
# default. Keys you do not set use zebrad's defaults.

";

/// The values the launcher owns in `zebrad.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedKeys {
    /// `network.network`.
    pub network: Network,
    /// `rpc.listen_addr`. Always loopback.
    pub rpc_addr: SocketAddr,
    /// `metrics.endpoint_addr`. Always loopback.
    pub metrics_addr: SocketAddr,
    /// `rpc.cookie_dir`, where zebrad writes its auth cookie.
    pub cookie_dir: PathBuf,
    /// `state.cache_dir`, where the chain state lives.
    pub state_dir: PathBuf,
}

impl ManagedKeys {
    /// The defaults for `network`: RPC on `127.0.0.1:8232` (Mainnet) or
    /// `127.0.0.1:18232` (Testnet), metrics on `127.0.0.1:9999`, and the cookie
    /// in [`Paths::rpc_cookie_dir`].
    pub fn defaults(network: Network, paths: &Paths, state_dir: impl Into<PathBuf>) -> Self {
        let rpc_port = match network {
            Network::Mainnet => MAINNET_RPC_PORT,
            Network::Testnet => TESTNET_RPC_PORT,
        };
        Self {
            network,
            rpc_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, rpc_port)),
            metrics_addr: SocketAddr::from((Ipv4Addr::LOCALHOST, METRICS_PORT)),
            cookie_dir: paths.rpc_cookie_dir(),
            state_dir: state_dir.into(),
        }
    }

    /// The `(section, key, value)` triples, in file order.
    fn entries(&self) -> Result<[(&'static str, &'static str, Value); 6], ConfigError> {
        Ok([
            ("network", "network", Value::from(self.network.as_str())),
            ("rpc", "listen_addr", Value::from(self.rpc_addr.to_string())),
            ("rpc", "enable_cookie_auth", Value::from(true)),
            (
                "rpc",
                "cookie_dir",
                Value::from(path_str(&self.cookie_dir)?),
            ),
            (
                "metrics",
                "endpoint_addr",
                Value::from(self.metrics_addr.to_string()),
            ),
            (
                "state",
                "cache_dir",
                Value::from(path_str(&self.state_dir)?),
            ),
        ])
    }
}

/// Reading or writing `zebrad.toml` failed.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A filesystem operation failed.
    #[error("config file I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The existing file is not valid TOML.
    #[error("zebrad.toml is not valid TOML: {0}")]
    Toml(#[from] toml_edit::TomlError),
    /// A section the launcher manages holds something other than a table.
    #[error("`{0}` in zebrad.toml must be a table")]
    NotATable(String),
    /// A managed path cannot be written to TOML.
    #[error("the path {0:?} is not valid UTF-8")]
    NonUtf8Path(PathBuf),
}

/// Saving an edited `zebrad.toml` was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum ConfigEditError {
    /// The text does not parse as TOML.
    #[error("invalid TOML: {0}")]
    InvalidToml(String),
    /// A key has a value the launcher does not allow.
    #[error("`{key}` is not allowed: {reason}")]
    Forbidden {
        /// The dotted key, such as `rpc.listen_addr`.
        key: String,
        /// Why, worded for the user.
        reason: String,
    },
    /// The file changed since it was read; reload and edit again.
    #[error("zebrad.toml changed since it was opened")]
    Conflict,
    /// Another change is still being applied.
    #[error("another configuration change is still being applied")]
    ApplyInProgress,
    /// A filesystem operation failed.
    #[error("config file I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Creates `zebrad.toml` with only the managed keys if it is missing, or sets
/// the managed keys in place in the existing file, keeping everything else.
///
/// The file is only rewritten when a managed value actually changes.
pub fn ensure(paths: &Paths, keys: &ManagedKeys) -> Result<(), ConfigError> {
    let path = paths.zebrad_config();
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    let (text, changed) = match &existing {
        Some(text) => {
            let mut doc: DocumentMut = text.parse()?;
            let changed = set_managed(&mut doc, keys)?;
            (doc.to_string(), changed)
        }
        None => (render_minimal(keys)?, true),
    };
    if changed {
        fs::create_dir_all(paths.config_dir())?;
        fsutil::write_atomic(&path, text.as_bytes())?;
    }
    Ok(())
}

/// The minimal file for `keys`: a short header and the managed keys only.
pub fn render_minimal(keys: &ManagedKeys) -> Result<String, ConfigError> {
    let mut doc = DocumentMut::new();
    set_managed(&mut doc, keys)?;
    let mut first = true;
    for (_, item) in doc.as_table_mut().iter_mut() {
        if let Some(table) = item.as_table_mut() {
            table
                .decor_mut()
                .set_prefix(if first { HEADER } else { "\n" });
            first = false;
        }
    }
    Ok(doc.to_string())
}

/// Sets every managed key, returning whether anything changed.
fn set_managed(doc: &mut DocumentMut, keys: &ManagedKeys) -> Result<bool, ConfigError> {
    let mut changed = false;
    for (section, key, value) in keys.entries()? {
        changed |= set_key(doc, section, key, value)?;
    }
    Ok(changed)
}

/// Sets `section.key` to `new`, keeping the existing key's comments.
fn set_key(
    doc: &mut DocumentMut,
    section: &str,
    key: &str,
    new: Value,
) -> Result<bool, ConfigError> {
    let root = doc.as_table_mut();
    if !root.contains_key(section) {
        root.insert(section, Item::Table(Table::new()));
    }
    let table = root
        .get_mut(section)
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| ConfigError::NotATable(section.to_string()))?;
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            if same_value(old, &new) {
                return Ok(false);
            }
            let decor = old.decor().clone();
            *old = new;
            *old.decor_mut() = decor;
        }
        Some(other) => *other = Item::Value(new),
        None => {
            table.insert(key, Item::Value(new));
        }
    }
    Ok(true)
}

fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        _ => false,
    }
}

fn path_str(path: &Path) -> Result<String, ConfigError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| ConfigError::NonUtf8Path(path.to_path_buf()))
}

/// Reads `zebrad.toml` with its compare-and-swap token.
pub fn read(paths: &Paths) -> Result<ConfigDocument, ConfigError> {
    let path = paths.zebrad_config();
    let bytes = fs::read(&path)?;
    let token = token(&bytes);
    let text =
        String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    Ok(ConfigDocument {
        text,
        token,
        path: path.display().to_string(),
    })
}

/// A hex digest of `bytes`, used to detect changes, not for security.
fn token(bytes: &[u8]) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(bytes);
    format!("{:016x}-{}", hasher.finish(), bytes.len())
}

/// The listener addresses that must stay on loopback.
const LOOPBACK_KEYS: [(&str, &str); 5] = [
    ("rpc", "listen_addr"),
    ("rpc", "indexer_listen_addr"),
    ("rpc", "lightwalletd_listen_addr"),
    ("metrics", "endpoint_addr"),
    ("health", "listen_addr"),
];

/// Checks an edited `zebrad.toml` before it is saved.
///
/// The text must parse, and the security- and supervision-relevant keys must
/// keep values the launcher supports: listeners on loopback, RPC cookie auth
/// on, logs on stdout (no `tracing.progress_bar` or `tracing.log_file`), and
/// `zcashd_compat` off. Everything else is left to zebrad.
pub fn validate(text: &str) -> Result<(), ConfigEditError> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|err: toml_edit::TomlError| ConfigEditError::InvalidToml(err.to_string()))?;
    let forbidden = |section: &str, key: &str, reason: &str| ConfigEditError::Forbidden {
        key: format!("{section}.{key}"),
        reason: reason.to_string(),
    };

    for (section, key) in LOOPBACK_KEYS {
        let Some(item) = lookup(&doc, section, key) else {
            continue;
        };
        let loopback = item
            .as_str()
            .and_then(|s| s.parse::<SocketAddr>().ok())
            .is_some_and(|addr| addr.ip().is_loopback());
        if !loopback {
            return Err(forbidden(
                section,
                key,
                "must be a loopback address such as \"127.0.0.1:8232\", so the node is not reachable from other machines",
            ));
        }
    }
    if let Some(item) = lookup(&doc, "rpc", "enable_cookie_auth")
        && item.as_bool() != Some(true)
    {
        return Err(forbidden(
            "rpc",
            "enable_cookie_auth",
            "must be true: the launcher authenticates to zebrad with the RPC cookie",
        ));
    }
    for key in ["progress_bar", "log_file"] {
        if lookup(&doc, "tracing", key).is_some() {
            return Err(forbidden(
                "tracing",
                key,
                "must not be set: it moves zebrad's logs away from the launcher",
            ));
        }
    }
    if let Some(item) = lookup(&doc, "zcashd_compat", "enabled")
        && item.as_bool() != Some(false)
    {
        return Err(forbidden(
            "zcashd_compat",
            "enabled",
            "is not supported by the launcher; remove it or set it to false",
        ));
    }
    Ok(())
}

/// `section.key`, whether written as a `[section]` table, an inline table or a
/// dotted key.
fn lookup<'a>(doc: &'a DocumentMut, section: &str, key: &str) -> Option<&'a Item> {
    doc.as_table()
        .get(section)?
        .as_table_like()?
        .get(key)
        .filter(|item| !item.is_none())
}

/// A configuration change written to disk but not yet confirmed by a zebrad run.
///
/// Call [`PendingApply::commit`] once zebrad started with the new file, or
/// [`PendingApply::rollback`] if it refused it. Dropping it does neither: the
/// pending marker stays, so [`recover`] rolls back on the next launch.
#[must_use = "a pending apply must be committed or rolled back"]
#[derive(Debug)]
pub struct PendingApply {
    paths: Paths,
}

impl PendingApply {
    /// Keeps the new file. The previous version stays as `zebrad.toml.bak`.
    pub fn commit(self) -> Result<(), ConfigError> {
        fsutil::remove_if_exists(&self.paths.pending_apply_marker())?;
        Ok(())
    }

    /// Restores the previous file from `zebrad.toml.bak`.
    pub fn rollback(self) -> Result<(), ConfigError> {
        restore_backup(&self.paths)
    }
}

/// Starts saving `new_text` as `zebrad.toml`; see the module docs.
///
/// Fails with [`ConfigEditError::Conflict`] when the file no longer matches
/// `expected_token`, and with [`ConfigEditError::Forbidden`] or
/// [`ConfigEditError::InvalidToml`] when [`validate`] refuses the text. The
/// file is unchanged when an error is returned.
pub fn begin_apply(
    paths: &Paths,
    new_text: &str,
    expected_token: &str,
) -> Result<PendingApply, ConfigEditError> {
    let config = paths.zebrad_config();
    let current = fs::read(&config)?;
    if token(&current) != expected_token {
        return Err(ConfigEditError::Conflict);
    }
    validate(new_text)?;

    let marker = paths.pending_apply_marker();
    if marker.exists() {
        return Err(ConfigEditError::ApplyInProgress);
    }
    fsutil::write_atomic(&paths.zebrad_config_backup(), &current)?;
    fs::write(
        &marker,
        b"zebrad.toml is being replaced; zebrad.toml.bak is the previous version\n",
    )?;
    if let Err(err) = fsutil::write_atomic(&config, new_text.as_bytes()) {
        // The rename did not happen, so the old file is still in place.
        let _ = fsutil::remove_if_exists(&marker);
        return Err(err.into());
    }
    Ok(PendingApply {
        paths: paths.clone(),
    })
}

/// What [`recover`] rolled back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovered {
    /// The configuration that was being applied when the launcher stopped.
    pub abandoned_text: String,
}

/// Rolls back a configuration change interrupted by a launcher crash. Call it
/// at launcher start, before zebrad is started.
///
/// Returns `None` when no change was pending.
pub fn recover(paths: &Paths) -> Result<Option<Recovered>, ConfigError> {
    let marker = paths.pending_apply_marker();
    if !marker.exists() {
        return Ok(None);
    }
    let abandoned_text = match fs::read_to_string(paths.zebrad_config()) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    if !paths.zebrad_config_backup().exists() {
        // The marker is written after the backup, so this only happens if the
        // backup was deleted by hand. There is nothing to restore.
        tracing::warn!("pending config marker without a backup; keeping zebrad.toml");
        fsutil::remove_if_exists(&marker)?;
        return Ok(None);
    }
    restore_backup(paths)?;
    Ok(Some(Recovered { abandoned_text }))
}

fn restore_backup(paths: &Paths) -> Result<(), ConfigError> {
    fsutil::copy_atomic(&paths.zebrad_config_backup(), &paths.zebrad_config())?;
    fsutil::remove_if_exists(&paths.pending_apply_marker())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GENERATED: &str = include_str!("../tests/fixtures/zebrad-7.0.0-generate.toml");

    fn setup() -> (tempfile::TempDir, Paths, ManagedKeys) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under("/launcher");
        let keys = ManagedKeys::defaults(Network::Mainnet, &paths, "/chain/state");
        // Render with fixed paths, then point the layout at the tempdir.
        let paths = Paths::under(dir.path());
        (dir, paths, keys)
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("list")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn defaults_per_network() {
        let paths = Paths::under("/l");
        let main = ManagedKeys::defaults(Network::Mainnet, &paths, "/s");
        let test = ManagedKeys::defaults(Network::Testnet, &paths, "/s");
        assert_eq!(main.rpc_addr.to_string(), "127.0.0.1:8232");
        assert_eq!(test.rpc_addr.to_string(), "127.0.0.1:18232");
        assert_eq!(main.metrics_addr.to_string(), "127.0.0.1:9999");
        assert_eq!(main.cookie_dir, paths.rpc_cookie_dir());
    }

    #[test]
    fn ensure_creates_the_minimal_file() {
        let (_dir, paths, keys) = setup();
        ensure(&paths, &keys).expect("ensure");
        let text = fs::read_to_string(paths.zebrad_config()).expect("read");
        let cookie_dir = Paths::under("/launcher").rpc_cookie_dir();
        let expected = format!(
            "{HEADER}[network]
network = \"Mainnet\"

[rpc]
listen_addr = \"127.0.0.1:8232\"
enable_cookie_auth = true
cookie_dir = {cookie}

[metrics]
endpoint_addr = \"127.0.0.1:9999\"

[state]
cache_dir = \"/chain/state\"
",
            cookie = Value::from(cookie_dir.to_str().expect("utf-8")),
        );
        assert_eq!(text, expected);
        validate(&text).expect("the managed file passes validation");
        assert_eq!(files(paths.config_dir()), ["zebrad.toml"]);
    }

    #[test]
    fn ensure_keeps_user_edits_and_comments() {
        let (_dir, paths, keys) = setup();
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        let user = "\
# My node.
[network]
network = \"Testnet\" # was testing
listen_addr = \"[::]:8233\"

[rpc]
# RPC for my wallet
listen_addr = \"127.0.0.1:1234\"

[mempool]
# keep it small
tx_cost_limit = 1000
";
        fs::write(paths.zebrad_config(), user).expect("write");
        ensure(&paths, &keys).expect("ensure");
        let text = fs::read_to_string(paths.zebrad_config()).expect("read");
        for kept in [
            "# My node.",
            "network = \"Mainnet\" # was testing",
            "listen_addr = \"[::]:8233\"",
            "# RPC for my wallet\nlisten_addr = \"127.0.0.1:8232\"",
            "# keep it small\ntx_cost_limit = 1000",
            "enable_cookie_auth = true",
            "[metrics]\nendpoint_addr = \"127.0.0.1:9999\"",
            "[state]\ncache_dir = \"/chain/state\"",
        ] {
            assert!(text.contains(kept), "{kept:?} missing from:\n{text}");
        }
        assert!(!text.contains("Testnet"), "{text}");

        // A second ensure with the same keys does not touch the file.
        let before = read(&paths).expect("read").token;
        ensure(&paths, &keys).expect("ensure again");
        assert_eq!(read(&paths).expect("read").token, before);
    }

    #[test]
    fn ensure_refuses_a_non_table_section() {
        let (_dir, paths, keys) = setup();
        fs::create_dir_all(paths.config_dir()).expect("mkdir");
        fs::write(paths.zebrad_config(), "rpc = 5\n").expect("write");
        assert!(matches!(
            ensure(&paths, &keys),
            Err(ConfigError::NotATable(s)) if s == "rpc"
        ));
    }

    #[test]
    fn validate_accepts_the_generated_reference() {
        validate(GENERATED).expect("zebrad generate output is valid");
        validate("").expect("an empty file is valid");
        validate(
            "[rpc]\nlisten_addr = \"[::1]:8232\"\nindexer_listen_addr = \"127.0.0.1:8230\"\nlightwalletd_listen_addr = \"127.0.0.1:9067\"\n[health]\nlisten_addr = \"127.0.0.1:8080\"\n",
        )
        .expect("loopback addresses are valid");
        validate("zcashd_compat = { enabled = false }\n").expect("compat off is valid");
    }

    #[test]
    fn validate_rejects_forbidden_values() {
        for (text, key) in [
            ("[rpc]\nlisten_addr = \"0.0.0.0:8232\"\n", "rpc.listen_addr"),
            ("rpc.listen_addr = \"[::]:8232\"\n", "rpc.listen_addr"),
            (
                "[rpc]\nlisten_addr = \"localhost:8232\"\n",
                "rpc.listen_addr",
            ),
            ("[rpc]\nlisten_addr = 8232\n", "rpc.listen_addr"),
            (
                "[rpc]\nindexer_listen_addr = \"0.0.0.0:8230\"\n",
                "rpc.indexer_listen_addr",
            ),
            (
                "[rpc]\nlightwalletd_listen_addr = \"[::]:9067\"\n",
                "rpc.lightwalletd_listen_addr",
            ),
            (
                "[metrics]\nendpoint_addr = \"192.168.1.2:9999\"\n",
                "metrics.endpoint_addr",
            ),
            (
                "health = { listen_addr = \"0.0.0.0:8080\" }\n",
                "health.listen_addr",
            ),
            (
                "[rpc]\nenable_cookie_auth = false\n",
                "rpc.enable_cookie_auth",
            ),
            (
                "[tracing]\nprogress_bar = \"summary\"\n",
                "tracing.progress_bar",
            ),
            ("[tracing]\nlog_file = \"/tmp/z.log\"\n", "tracing.log_file"),
            ("[zcashd_compat]\nenabled = true\n", "zcashd_compat.enabled"),
        ] {
            match validate(text) {
                Err(ConfigEditError::Forbidden { key: got, reason }) => {
                    assert_eq!(got, key, "{text}");
                    assert!(!reason.is_empty());
                }
                other => panic!("{text:?} gave {other:?}"),
            }
        }
        assert!(matches!(
            validate("[rpc\n"),
            Err(ConfigEditError::InvalidToml(_))
        ));
    }

    #[test]
    fn apply_commit_keeps_the_new_file_and_a_backup() {
        let (_dir, paths, keys) = setup();
        ensure(&paths, &keys).expect("ensure");
        let original = read(&paths).expect("read");
        let new_text = format!("{}\n[mempool]\ntx_cost_limit = 1000\n", original.text);

        let pending = begin_apply(&paths, &new_text, &original.token).expect("begin");
        assert!(paths.pending_apply_marker().exists());
        assert_eq!(read(&paths).expect("read").text, new_text);
        pending.commit().expect("commit");

        assert!(!paths.pending_apply_marker().exists());
        assert_eq!(read(&paths).expect("read").text, new_text);
        assert_eq!(
            fs::read_to_string(paths.zebrad_config_backup()).expect("backup"),
            original.text
        );
        assert_eq!(recover(&paths).expect("recover"), None);
        assert_eq!(
            files(paths.config_dir()),
            ["zebrad.toml", "zebrad.toml.bak"]
        );
    }

    #[test]
    fn apply_rollback_restores_the_previous_file() {
        let (_dir, paths, keys) = setup();
        ensure(&paths, &keys).expect("ensure");
        let original = read(&paths).expect("read");
        let pending =
            begin_apply(&paths, "[mempool]\ntx_cost_limit = 1\n", &original.token).expect("begin");
        pending.rollback().expect("rollback");
        assert_eq!(read(&paths).expect("read"), original);
        assert!(!paths.pending_apply_marker().exists());
        assert_eq!(
            files(paths.config_dir()),
            ["zebrad.toml", "zebrad.toml.bak"]
        );
    }

    #[test]
    fn recover_rolls_back_an_interrupted_apply() {
        let (_dir, paths, keys) = setup();
        ensure(&paths, &keys).expect("ensure");
        let original = read(&paths).expect("read");
        let new_text = "[mempool]\ntx_cost_limit = 1\n";
        // Simulate a launcher crash: the pending apply is never resolved.
        drop(begin_apply(&paths, new_text, &original.token).expect("begin"));

        assert_eq!(
            recover(&paths).expect("recover"),
            Some(Recovered {
                abandoned_text: new_text.to_string()
            })
        );
        assert_eq!(read(&paths).expect("read"), original);
        assert_eq!(recover(&paths).expect("second recover"), None);
    }

    #[test]
    fn apply_detects_conflicts_and_invalid_text() {
        let (_dir, paths, keys) = setup();
        ensure(&paths, &keys).expect("ensure");
        let stale = read(&paths).expect("read");
        fs::write(
            paths.zebrad_config(),
            format!("{}# edited elsewhere\n", stale.text),
        )
        .expect("external edit");
        let edited = read(&paths).expect("read");

        assert!(matches!(
            begin_apply(&paths, "", &stale.token),
            Err(ConfigEditError::Conflict)
        ));
        assert!(matches!(
            begin_apply(&paths, "[rpc]\nenable_cookie_auth = false\n", &edited.token),
            Err(ConfigEditError::Forbidden { .. })
        ));
        // Nothing was written by the refused attempts.
        assert_eq!(read(&paths).expect("read"), edited);
        assert_eq!(files(paths.config_dir()), ["zebrad.toml"]);

        // A second apply while one is pending is refused.
        let pending = begin_apply(&paths, "", &edited.token).expect("begin");
        let current = read(&paths).expect("read");
        assert!(matches!(
            begin_apply(&paths, "", &current.token),
            Err(ConfigEditError::ApplyInProgress)
        ));
        pending.commit().expect("commit");
    }
}
