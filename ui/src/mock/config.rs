//! The mock's `zebrad.toml`: the document the mock starts with, the minimal file `reset_config`
//! writes, and the marker texts that make a save fail in each of the backend's ways.

use std::sync::atomic::Ordering;

use launcher_types::{ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, Network};
use leptos::prelude::*;

use super::{DEFAULT_DATA_DIR, mock, sleep};

/// The config document the mock starts with: what the launcher's minimal managed file looks like.
pub(super) const SAMPLE_CONFIG: &str = r#"# Managed by Zebra Launcher. Keys you add are kept; the RPC, metrics and
# network keys below are enforced by the launcher.

[network]
network = "Mainnet"
listen_addr = "[::]:8233"

[rpc]
listen_addr = "127.0.0.1:8232"
enable_cookie_auth = true

[metrics]
endpoint_addr = "127.0.0.1:9999"

[state]
cache_dir = "/home/user/.local/share/org.zfnd.zebra-launcher/zebra"

[tracing]
use_color = false
"#;

/// What `reset_config` writes: the launcher's minimal file for the network and the state
/// directory, like `config::render_minimal`.
fn minimal_config(network: Network, state_dir: &str) -> String {
    let rpc_port = match network {
        Network::Mainnet => 8232,
        Network::Testnet => 18232,
    };
    format!(
        r#"# zebrad configuration managed by Zebra Launcher.
#
# The launcher keeps network.network, rpc.listen_addr, rpc.enable_cookie_auth,
# rpc.cookie_dir, metrics.endpoint_addr and state.cache_dir up to date.
# Add your own settings below; the reference shows every option with its
# default. Keys you do not set use zebrad's defaults.

[network]
network = "{network}"

[rpc]
listen_addr = "127.0.0.1:{rpc_port}"
enable_cookie_auth = true
cookie_dir = "{DEFAULT_DATA_DIR}/rpc"

[metrics]
endpoint_addr = "127.0.0.1:9999"

[state]
cache_dir = "{state_dir}"
"#,
        network = network.as_str(),
    )
}

/// The text that makes the mock reject a config, and the error zebrad 7.0.0 prints for it.
const BAD_KEY: &str = "bogus";
const CONFIG_ERROR: &str = "error: Zebra could not load the provided configuration file and/or environment variables.\nerror: zebrad fatal error: I/O operation failed: Configuration error: unknown field `bogus`, expected one of `listen_addr`, `indexer_listen_addr`, `lightwalletd_listen_addr`, `parallel_cpu_threads`, `debug_force_finished_sync`, `cookie_dir`, `enable_cookie_auth`, `max_response_body_size` for key `rpc`";

/// A config containing this comment makes the mock report that the file changed on disk.
const CONFLICT_MARKER: &str = "# simulate-conflict";

/// A config containing this comment makes the mock fail the next `read_config`.
const READ_FAILURE_MARKER: &str = "# simulate-read-failure";

pub async fn read_config() -> Result<ConfigDocument, String> {
    sleep(150).await;
    let mock = mock();
    if mock.fail_next_read.swap(false, Ordering::Relaxed) {
        return Err("(mock) permission denied reading zebrad.toml".into());
    }
    Ok(mock.config.get_untracked())
}

pub async fn apply_config(text: String, token: String) -> Result<ApplyOutcome, ApplyError> {
    sleep(300).await;
    let mock = mock();
    let current = mock.config.get_untracked();
    if token != current.token || text.contains(CONFLICT_MARKER) {
        return Err(ApplyError {
            kind: ApplyErrorKind::Conflict,
            message: format!("{} changed on disk since it was read", current.path),
        });
    }
    mock.fail_next_read
        .store(text.contains(READ_FAILURE_MARKER), Ordering::Relaxed);
    mock.stop(true);
    sleep(2000).await;
    if text.contains(BAD_KEY) {
        return Ok(ApplyOutcome::RolledBack {
            error: CONFIG_ERROR.into(),
        });
    }
    mock.config.update(|config| {
        config.text = text;
        config.token = format!("mock-{}", js_sys::Date::now());
    });
    Ok(ApplyOutcome::Applied)
}

pub async fn reset_config() -> Result<ConfigDocument, String> {
    sleep(300).await;
    let mock = mock();
    mock.require_stopped()?;
    let network = mock.settings.with_untracked(|settings| settings.network);
    let text = minimal_config(network, &mock.state_dir());
    mock.config.update(|config| {
        config.text = text;
        config.token = format!("mock-{}", js_sys::Date::now());
    });
    Ok(mock.config.get_untracked())
}
