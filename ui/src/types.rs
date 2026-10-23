//! IPC types that are being added to `launcher-types` in M4.
//!
//! TODO(m4): replace with launcher_types::* — these are temporary local copies with the agreed
//! shapes, so the merge is a delete plus an import change.

use serde::{Deserialize, Serialize};

/// The Zcash network zebrad runs on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    Mainnet,
    Testnet,
}

/// How the launcher updates itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    #[default]
    Off,
    Notify,
    Auto,
}

/// The launcher's own settings (`launcher.json`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub update_mode: UpdateMode,
    pub close_to_tray: bool,
    pub start_node_on_launch: bool,
    pub grafana_url: Option<String>,
    pub data_dir: Option<String>,
    pub network: Network,
    pub stop_grace_secs: u64,
}

// The backend owns the real defaults; these only fill fields missing from a payload.
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            update_mode: UpdateMode::Off,
            close_to_tray: false,
            start_node_on_launch: true,
            grafana_url: None,
            data_dir: None,
            network: Network::Mainnet,
            stop_grace_secs: 60,
        }
    }
}

/// What the RPC monitor last learned from zebrad.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeStats {
    pub height: u64,
    pub estimated_height: u64,
    pub verification_progress: f64,
    pub peers: u32,
    pub mempool_transactions: u64,
    pub chain: String,
    pub zebrad_version: String,
    pub last_error: Option<String>,
    pub eos: Option<EosInfo>,
    pub updated_at: std::time::SystemTime,
}

/// When this zebrad release reaches its end of support.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EosInfo {
    pub block_height: u64,
    pub estimated_time: std::time::SystemTime,
}

/// The managed `zebrad.toml`, with a token that `apply_config` checks against the file on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigDocument {
    pub text: String,
    pub token: String,
    pub path: String,
}

/// The result of `apply_config` when the file was written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApplyOutcome {
    /// zebrad accepted the new config.
    Applied,
    /// zebrad rejected the new config and the previous one was restored.
    RolledBack { error: String },
}

/// Why `apply_config` did not write the file. `kind` is `conflict` when the file changed on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyError {
    pub kind: String,
    pub message: String,
}
