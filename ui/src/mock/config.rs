//! The mock's `zebrad.toml`: the document the mock starts with, the minimal file `reset_config`
//! writes, and the marker texts that make a save fail in each of the backend's ways.

use std::sync::atomic::Ordering;

use launcher_types::{
    ApplyError, ApplyErrorKind, ApplyOutcome, ConfigDocument, Network, NodeInfo, PortRole,
};
use leptos::prelude::*;

use super::{DEFAULT_DATA_DIR, mock, sleep};

/// The config document the mock starts with: what the launcher's minimal managed file looks like.
pub(super) const SAMPLE_CONFIG: &str = r#"# Managed by Zeal. Keys you add are kept; the RPC, metrics and
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
cache_dir = "/home/user/.local/share/org.zfnd.zeal/zebra"

[tracing]
use_color = false
"#;

/// What `reset_config` writes for a node, like the backend's `config::render_minimal`: the
/// managed keys for its network, ports, cookie and state folder. The P2P port is written only
/// when it is not the one zebrad uses without it.
pub(super) fn minimal_config(info: &NodeInfo, cookie_dir: &str) -> String {
    let port = |role| info.port(role).unwrap_or_default();
    let default_p2p = match info.network {
        Network::Mainnet => 8233,
        Network::Testnet => 18233,
    };
    let p2p = match port(PortRole::P2p) {
        p2p if p2p == default_p2p => String::new(),
        p2p => format!("listen_addr = \"[::]:{p2p}\"\n"),
    };
    format!(
        r#"# zebrad configuration managed by Zeal.
#
# The launcher keeps network.network, the port of network.listen_addr,
# rpc.listen_addr, rpc.enable_cookie_auth, rpc.cookie_dir,
# metrics.endpoint_addr and state.cache_dir up to date.
# Add your own settings below; the reference shows every option with its
# default. Keys you do not set use zebrad's defaults.

[network]
network = "{network}"
{p2p}
[rpc]
listen_addr = "127.0.0.1:{rpc}"
enable_cookie_auth = true
cookie_dir = "{cookie_dir}"

[metrics]
endpoint_addr = "127.0.0.1:{metrics}"

[state]
cache_dir = "{state}"
"#,
        network = info.network.as_str(),
        rpc = port(PortRole::Rpc),
        metrics = port(PortRole::Metrics),
        state = crate::format::state_dir(&info.data_dir),
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
    let info = mock.info.get_untracked();
    let text = minimal_config(&info, &format!("{DEFAULT_DATA_DIR}/rpc"));
    mock.config.update(|config| {
        config.text = text;
        config.token = format!("mock-{}", js_sys::Date::now());
    });
    Ok(mock.config.get_untracked())
}

#[cfg(test)]
mod tests {
    use launcher_types::NodePort;

    use super::{super::nodes::first_node, *};

    #[test]
    fn the_minimal_config_carries_the_nodes_own_ports_and_folders() {
        let first = first_node(DEFAULT_DATA_DIR);
        let text = minimal_config(&first, &format!("{DEFAULT_DATA_DIR}/rpc"));
        assert!(text.contains("network = \"Mainnet\"\n\n[rpc]"), "{text}");
        assert!(text.contains("listen_addr = \"127.0.0.1:8232\""), "{text}");
        assert!(
            text.contains("endpoint_addr = \"127.0.0.1:9999\""),
            "{text}"
        );
        let second = NodeInfo {
            network: Network::Testnet,
            data_dir: format!("{DEFAULT_DATA_DIR}/nodes/zebrad-testnet"),
            ports: vec![
                NodePort {
                    role: PortRole::P2p,
                    port: 28233,
                },
                NodePort {
                    role: PortRole::Rpc,
                    port: 18232,
                },
                NodePort {
                    role: PortRole::Metrics,
                    port: 19999,
                },
            ],
            ..first
        };
        let text = minimal_config(&second, "/nodes/zebrad-testnet/rpc");
        assert!(text.contains("listen_addr = \"[::]:28233\""), "{text}");
        assert!(
            text.contains("endpoint_addr = \"127.0.0.1:19999\""),
            "{text}"
        );
        assert!(text.contains("nodes/zebrad-testnet/state"), "{text}");
    }
}
