//! zingod's environment holds only the five variables Zeal sets: what the test process has,
//! zingod's own configuration variables in any case, a service manager's socket and a
//! malformed `RUST_LOG` among them, never reaches it. A test binary of its own, since it sets
//! variables in its process.

#![cfg(unix)]

use std::{
    collections::BTreeMap,
    net::{Ipv4Addr, SocketAddr, TcpListener},
};

use launcher_core::{
    supervisor,
    zingod::{ZingodCommand, ZingodConfig, ZingodNetwork},
};

#[path = "support/helpers.rs"]
mod support;
use support::{fake_zingod, fast_supervisor_config, within};

#[test]
fn only_the_five_variables_reach_zingod() {
    // SAFETY: the only test in this binary, which sets these before its runtime or any other
    // thread that could read the environment starts.
    unsafe {
        std::env::set_var("ZINGOD_CONFIG_SYNC__CONCURRENCY", "0");
        std::env::set_var("ZINGOD_CONFIG_NETWORK", "mainnet");
        std::env::set_var("zingod_config_x", "1");
        std::env::set_var("NOTIFY_SOCKET", "/run/systemd/notify");
        std::env::set_var("RUST_LOG", "zingod=[");
        std::env::set_var("MALLOC_CONF", "narenas:1");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let env = runtime.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = ZingodConfig {
            network: ZingodNetwork::Testnet,
            validator: SocketAddr::from((Ipv4Addr::LOCALHOST, 1)),
            cookie: dir.path().join("rpc/.cookie"),
            listen: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            index_dir: dir.path().join("zingod/testnet"),
        };
        let path = dir.path().join("zingod-testnet.toml");
        config.write(&path).unwrap();
        config.ensure_dirs().unwrap();
        std::fs::write(
            dir.path().join("fake-zingod.env"),
            "FAKE_ZINGOD_MODE=print-env\n",
        )
        .unwrap();
        let handle = supervisor::spawn(fast_supervisor_config(ZingodCommand::new(
            fake_zingod(),
            &path,
            config.home_dir(),
        )))
        .unwrap();
        within(handle.start()).await.expect("zingod starts");
        let history = within(handle.subscribe()).await.unwrap().history;
        within(handle.shutdown()).await.unwrap();
        let home = config.home_dir();
        let env: BTreeMap<String, String> = history
            .iter()
            .filter_map(|line| line.message.strip_prefix("env "))
            .filter_map(|pair| pair.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        (env, home)
    });
    let (env, home) = env;
    let home = home.display().to_string();
    assert_eq!(
        env,
        BTreeMap::from(
            [
                ("HOME", home.clone()),
                ("XDG_CONFIG_HOME", format!("{home}/.config")),
                ("XDG_DATA_HOME", format!("{home}/.local/share")),
                ("XDG_CACHE_HOME", format!("{home}/.cache")),
                ("RUST_LOG", "zingod=info".to_string()),
            ]
            .map(|(name, value)| (name.to_string(), value))
        )
    );
}
