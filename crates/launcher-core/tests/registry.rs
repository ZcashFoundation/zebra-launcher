//! The node registry against the `fake-zebrad` binary: several nodes at once,
//! commands routed by id, adding and removing nodes, port conflicts and the
//! node list on disk.
//!
//! The nodes get ports that were free a moment before (bound and released),
//! so the tests run beside each other and beside a real zebrad. Every await
//! is bounded by [`within`].

use std::{
    fs,
    sync::Arc,
    time::{Duration, SystemTime},
};

use launcher_core::{
    config,
    nodes::{NodeError, NodeLayout, NodeSpec},
    paths::Paths,
    pidfile,
    policy::RestartPolicy,
    registry::{self, RegistryConfig, RegistryHandle, StartError},
    settings::{SettingsHandle, SettingsStore},
    supervisor::{Note, StartOptions, SupervisorError},
};
use launcher_types::{
    ExitClass, LogLevel, MAX_NODES, Network, NodeDraft, NodeId, NodeKind, NodePort, NodeProgram,
    NodeUpdate, Phase, PortRole, Settings,
};

#[path = "support/helpers.rs"]
mod support;
use support::{LIMIT, contains, fake_zebrad, messages, within};

/// `n` ports that were free a moment ago.
fn free_ports(n: usize) -> Vec<u16> {
    let held: Vec<std::net::TcpListener> = (0..n)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port"))
        .collect();
    held.iter()
        .map(|listener| listener.local_addr().expect("address").port())
        .collect()
}

/// A zebrad node's three ports, free a moment ago.
fn zebrad_ports() -> Vec<NodePort> {
    let ports = free_ports(3);
    vec![
        NodePort {
            role: PortRole::P2p,
            port: ports[0],
        },
        NodePort {
            role: PortRole::Rpc,
            port: ports[1],
        },
        NodePort {
            role: PortRole::Metrics,
            port: ports[2],
        },
    ]
}

struct Fixture {
    dir: tempfile::TempDir,
    paths: Paths,
    settings: SettingsHandle,
    registry: RegistryHandle,
}

/// The registry's configuration for fake-zebrad: a 50 ms heartbeat, fast
/// stops and restarts, fast polls, and no port counted as in use elsewhere
/// unless `in_use` says so.
fn config(paths: &Paths, in_use: fn(PortRole, u16) -> bool) -> RegistryConfig {
    config_with_modes(paths, in_use, |_| "ok")
}

/// [`config`], with the fake's mode for each node picked by `mode`.
fn config_with_modes(
    paths: &Paths,
    in_use: fn(PortRole, u16) -> bool,
    mode: fn(&NodeSpec) -> &'static str,
) -> RegistryConfig {
    let mut config = RegistryConfig::new(paths.clone(), fake_zebrad());
    config.customize = Some(Arc::new(move |spec, supervisor| {
        supervisor.command.env = vec![
            ("FAKE_ZEBRAD_MODE".into(), mode(spec).into()),
            ("FAKE_ZEBRAD_HEARTBEAT_MS".into(), "50".into()),
        ];
        supervisor.stop_grace = Duration::from_secs(3);
        supervisor.exit_drain = Duration::from_millis(500);
        supervisor.restart = RestartPolicy {
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_millis(200),
            window: Duration::from_secs(60),
            max_crashes: 3,
        };
    }));
    config.monitor_interval = Duration::from_millis(200);
    config.metrics_interval = Duration::from_millis(200);
    config.port_in_use = Arc::new(in_use);
    config
}

fn nothing_in_use(_: PortRole, _: u16) -> bool {
    false
}

/// A launcher in a temporary directory whose first node, on Mainnet, has
/// free ports; the first run is done unless `first_run` says otherwise.
async fn fixture_with(first_run_done: bool, in_use: fn(PortRole, u16) -> bool) -> Fixture {
    fixture_from(first_run_done, |paths| config(paths, in_use)).await
}

/// [`fixture_with`] for the registry `config` gives.
async fn fixture_from(
    first_run_done: bool,
    config: impl FnOnce(&Paths) -> RegistryConfig,
) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let paths = Paths::under(dir.path());
    paths.ensure_dirs().expect("dirs");
    let settings = SettingsStore::open(&paths).expect("settings");
    within(settings.save_nodes(vec![NodeSpec {
        ports: zebrad_ports(),
        ..NodeSpec::first(Network::Mainnet)
    }]))
    .await
    .expect("the first node");
    within(settings.update(move |s| s.first_run_done = first_run_done))
        .await
        .expect("first run");
    let registry = registry::spawn(config(&paths), settings.clone()).expect("registry");
    Fixture {
        dir,
        paths,
        settings,
        registry,
    }
}

async fn fixture() -> Fixture {
    fixture_with(true, nothing_in_use).await
}

fn draft(network: Network, name: &str) -> NodeDraft {
    NodeDraft {
        kind: NodeKind::Zebrad,
        network,
        name: name.into(),
        id: None,
        ports: Some(zebrad_ports()),
        data_dir: None,
        program: NodeProgram::Bundled,
        autostart: true,
    }
}

fn id(text: &str) -> NodeId {
    NodeId::new(text).expect("a valid id")
}

fn done() -> Settings {
    Settings {
        first_run_done: true,
        ..Settings::default()
    }
}

/// Waits until `node`'s status satisfies `predicate`.
async fn wait_for(
    registry: &RegistryHandle,
    node: &NodeId,
    what: &str,
    predicate: impl FnMut(&launcher_types::Status) -> bool,
) -> launcher_types::Status {
    let handle = registry.node(node).expect("the node exists");
    support::wait_status(&handle.supervisor, LIMIT, what, predicate).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_nodes_run_side_by_side_on_their_own_ports_folders_and_cookies() {
    let fx = fixture().await;
    let first = fx.registry.first().expect("the first node");
    let testnet = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add a Testnet node");
    assert_eq!(testnet.id.as_str(), "zebrad-testnet");
    assert_eq!(fx.registry.infos().len(), 2);
    let second = fx.registry.node(&testnet.id).expect("lookup by id");

    within(first.start(&done(), StartOptions::default()))
        .await
        .expect("start the first node");
    within(second.start(&done(), StartOptions::default()))
        .await
        .expect("start the second node");
    let a = wait_for(&fx.registry, &first.id, "the first node's RPC", |s| {
        s.rpc_addr.is_some() && s.metrics_addr.is_some()
    })
    .await;
    let b = wait_for(&fx.registry, &second.id, "the second node's RPC", |s| {
        s.rpc_addr.is_some() && s.metrics_addr.is_some()
    })
    .await;
    // Each zebrad listens on its own node's ports.
    let port = |spec: &NodeSpec, role| spec.port(role).expect("a port");
    assert_eq!(
        a.rpc_addr.unwrap().port(),
        port(&first.spec(), PortRole::Rpc)
    );
    assert_eq!(
        b.rpc_addr.unwrap().port(),
        port(&second.spec(), PortRole::Rpc)
    );
    assert_eq!(
        b.metrics_addr.unwrap().port(),
        port(&second.spec(), PortRole::Metrics)
    );
    assert_ne!(a.pid, b.pid);

    // Each has its own files: zebrad.toml, cookie, state folder.
    let folder = fx.paths.nodes_dir().join("zebrad-testnet");
    assert_eq!(second.paths.zebrad_config(), folder.join("zebrad.toml"));
    let toml = fs::read_to_string(second.paths.zebrad_config()).expect("its zebrad.toml");
    for needle in [
        "network = \"Testnet\"".to_string(),
        format!(
            "listen_addr = \"127.0.0.1:{}\"",
            port(&second.spec(), PortRole::Rpc)
        ),
        format!(
            "listen_addr = \"[::]:{}\"",
            port(&second.spec(), PortRole::P2p)
        ),
        format!(
            "endpoint_addr = \"127.0.0.1:{}\"",
            port(&second.spec(), PortRole::Metrics)
        ),
    ] {
        assert!(toml.contains(&needle), "{needle} missing from:\n{toml}");
    }
    assert!(
        toml.contains(&*folder.join("rpc").to_string_lossy()),
        "{toml}"
    );
    assert!(
        toml.contains(&*folder.join("state").to_string_lossy()),
        "{toml}"
    );
    assert_eq!(
        first.paths,
        fx.paths.launcher_node(),
        "the first node keeps its files"
    );
    let cookie_a = fs::read_to_string(first.paths.rpc_cookie_file()).expect("first cookie");
    let cookie_b = fs::read_to_string(second.paths.rpc_cookie_file()).expect("second cookie");
    assert_ne!(cookie_a, cookie_b, "two cookies");

    // Each node's monitor reads its own zebrad.
    let mut stats_a = first.stats.clone();
    let mut stats_b = second.stats.clone();
    within(stats_a.wait_for(Option::is_some))
        .await
        .expect("first stats");
    within(stats_b.wait_for(Option::is_some))
        .await
        .expect("second stats");

    // Commands reach the node they name: stopping one leaves the other.
    let lookup = fx.registry.node(&first.id).expect("lookup");
    within(lookup.supervisor.stop())
        .await
        .expect("stop the first");
    assert_eq!(first.supervisor.status().phase, Phase::Stopped);
    assert_eq!(second.supervisor.status().phase, Phase::Running);
    let history_a = within(first.supervisor.history_since(0))
        .await
        .expect("history");
    let history_b = within(second.supervisor.history_since(0))
        .await
        .expect("history");
    let started = |pid: Option<u32>| format!("zebrad started (pid {})", pid.expect("a pid"));
    assert!(
        contains(&history_a, &started(a.pid)),
        "{:#?}",
        messages(&history_a)
    );
    assert!(!contains(&history_a, &started(b.pid)), "a log of its own");
    assert!(contains(&history_b, &started(b.pid)));
    assert!(matches!(
        fx.registry.node(&id("zebrad-nowhere")),
        Err(NodeError::NotFound(_))
    ));

    within(fx.registry.shutdown()).await;
    assert_eq!(second.supervisor.status().phase, Phase::Stopped);
    drop(fx.dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn adding_refuses_a_draft_that_breaks_a_rule_and_leaves_nothing_behind() {
    fn held(_: PortRole, port: u16) -> bool {
        port == 1
    }
    let fx = fixture_with(true, held).await;
    let first = fx.registry.first().expect("first").spec();
    let before = fs::read(fx.paths.settings_file()).expect("launcher.json");
    let refused = async |draft: NodeDraft| {
        within(fx.registry.add(draft))
            .await
            .expect_err("the draft is refused")
    };

    assert_eq!(
        refused(NodeDraft {
            kind: NodeKind::Zakura,
            ..draft(Network::Mainnet, "Zakura")
        })
        .await,
        NodeError::KindNotAddable(NodeKind::Zakura)
    );
    assert_eq!(
        refused(draft(Network::Testnet, &first.name)).await,
        NodeError::NameTaken(first.name.clone())
    );
    assert_eq!(
        refused(NodeDraft {
            id: Some(first.id.clone()),
            ..draft(Network::Mainnet, "Again")
        })
        .await,
        NodeError::IdTaken(first.id.clone())
    );
    // A port of the first node's, and a port another program holds.
    let mut ports = zebrad_ports();
    ports[1].port = first.port(PortRole::Rpc).unwrap();
    assert!(matches!(
        refused(NodeDraft {
            ports: Some(ports.clone()),
            ..draft(Network::Testnet, "Zebra · Testnet")
        })
        .await,
        NodeError::PortTaken { role: PortRole::Rpc, holder, .. } if holder == first.name
    ));
    ports[1].port = 1;
    assert_eq!(
        refused(NodeDraft {
            ports: Some(ports),
            ..draft(Network::Testnet, "Zebra · Testnet")
        })
        .await,
        NodeError::PortInUse {
            role: PortRole::Rpc,
            port: 1
        }
    );
    // The first node's data folder.
    assert!(matches!(
        refused(NodeDraft {
            data_dir: Some(fx.paths.data_dir().display().to_string()),
            ..draft(Network::Testnet, "Zebra · Testnet")
        })
        .await,
        NodeError::DataDirTaken { .. }
    ));
    // Nothing was written for any of them.
    assert!(!fx.paths.nodes_dir().exists());
    assert_eq!(
        fs::read(fx.paths.settings_file()).expect("launcher.json"),
        before
    );

    // Up to eight nodes; the ninth is refused.
    for n in 2..=MAX_NODES {
        within(
            fx.registry
                .add(draft(Network::Testnet, &format!("Node {n}"))),
        )
        .await
        .unwrap_or_else(|err| panic!("node {n}: {err}"));
    }
    assert_eq!(fx.registry.infos().len(), MAX_NODES);
    assert_eq!(
        refused(draft(Network::Testnet, "One more")).await,
        NodeError::TooMany
    );
    let ids: Vec<String> = fx
        .registry
        .infos()
        .into_iter()
        .map(|info| info.id.to_string())
        .collect();
    assert_eq!(ids[1], "zebrad-testnet");
    assert_eq!(ids[2], "zebrad-testnet-2");
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_port_another_program_holds_fails_the_start_as_port_in_use() {
    let fx = fixture().await;
    let first = fx.registry.first().expect("first");
    let rpc = first.spec().port(PortRole::Rpc).unwrap();
    let holder = std::net::TcpListener::bind(("127.0.0.1", rpc)).expect("hold the RPC port");
    let err = within(first.start(&done(), StartOptions::default()))
        .await
        .expect_err("the port is taken");
    assert!(
        matches!(err, StartError::Supervisor(SupervisorError::PortInUse(_))),
        "{err}"
    );
    assert_eq!(
        first.supervisor.status().phase,
        Phase::Failed {
            class: ExitClass::PortInUse
        }
    );
    drop(holder);
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_stops_the_node_and_keeps_or_deletes_its_data() {
    let fx = fixture().await;
    let kept = within(fx.registry.add(draft(Network::Testnet, "Kept")))
        .await
        .expect("add");
    let kept = fx.registry.node(&kept.id).expect("lookup");
    within(kept.start(&done(), StartOptions::default()))
        .await
        .expect("start");
    let chain = kept.state_dir().join("v29").join("testnet");
    fs::create_dir_all(&chain).expect("a chain");
    fs::write(chain.join("000001.sst"), b"blocks").expect("a block file");

    // Removed with its data kept: stopped, gone from the list, files stay.
    assert_eq!(within(fx.registry.remove(&kept.id, false)).await, Ok(0));
    assert_eq!(kept.supervisor.status().phase, Phase::Stopped);
    assert!(matches!(
        within(kept.supervisor.start()).await,
        Err(SupervisorError::Closed)
    ));
    assert!(fx.registry.node(&kept.id).is_err());
    assert_eq!(fx.settings.nodes().len(), 1, "launcher.json lists one node");
    assert!(chain.join("000001.sst").exists(), "the data stays");
    // Its id is not reused while its folder is there.
    let next = within(fx.registry.add(draft(Network::Testnet, "Next")))
        .await
        .expect("add");
    assert_eq!(next.id.as_str(), "zebrad-testnet-2");

    // Removed with its data deleted: the state and the node's folder go.
    let next = fx.registry.node(&next.id).expect("lookup");
    let state = next.state_dir();
    fs::create_dir_all(state.join("v29")).expect("a chain");
    within(fx.registry.remove(&next.id, true))
        .await
        .expect("remove and delete");
    assert!(!state.exists(), "the chain state is deleted");
    assert!(
        !next.paths.data_dir().exists(),
        "and so is the node's folder"
    );
    assert!(
        fx.paths.launcher_node().zebrad_config().exists(),
        "the first node's stay"
    );

    // The last node cannot go.
    let first = fx.registry.first().expect("first");
    assert!(matches!(
        within(fx.registry.remove(&first.id, false)).await,
        Err(NodeError::LastNode(_))
    ));
    within(fx.registry.shutdown()).await;
}

/// A removal saves the list before it deletes anything, so what cannot be
/// deleted leaves the node off the list, named in the error, with its
/// folder keeping its id from a new node.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removal_leaves_the_list_first_and_names_what_it_could_not_delete() {
    use std::os::unix::fs::PermissionsExt;

    let fx = fixture().await;
    let added = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let node = fx.registry.node(&added.id).expect("lookup");
    // A folder in the chain state whose files cannot be removed.
    let locked = node.state_dir().join("v29").join("testnet");
    fs::create_dir_all(&locked).expect("a chain");
    fs::write(locked.join("000001.sst"), b"blocks").expect("a block file");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).expect("lock it");
    if fs::write(locked.join("probe"), b"").is_ok() {
        // Permissions do not bind this user (root): nothing to test.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("unlock");
        within(fx.registry.shutdown()).await;
        return;
    }

    let err = within(fx.registry.remove(&added.id, true))
        .await
        .expect_err("the locked folder stays");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).expect("unlock");
    assert!(
        matches!(
            &err,
            NodeError::LeftBehind { name, path, .. }
                if name == "Zebra · Testnet" && *path == node.state_dir().display().to_string()
        ),
        "{err}"
    );
    assert!(
        fx.registry.node(&added.id).is_err(),
        "the node left the list"
    );
    assert_eq!(fx.settings.nodes().len(), 1, "and launcher.json");
    assert!(node.paths.data_dir().exists(), "its folder stays");
    let next = within(fx.registry.add(draft(Network::Testnet, "Next")))
        .await
        .expect("add");
    assert_eq!(
        next.id.as_str(),
        "zebrad-testnet-2",
        "the folder keeps its id"
    );
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zebrad_left_by_an_earlier_launcher_run_keeps_its_node_from_being_removed() {
    let fx = fixture().await;
    let added = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let node = fx.registry.node(&added.id).expect("lookup");
    let chain = node.state_dir().join("v29").join("testnet");
    fs::create_dir_all(&chain).expect("a chain");
    fs::write(chain.join("000001.sst"), b"blocks").expect("a block file");
    // A zebrad "left behind" by an earlier run: started outside the
    // supervisor, with the node's pid file naming it.
    let mut orphan = std::process::Command::new(fake_zebrad())
        .args([
            "-c",
            &node.paths.zebrad_config().display().to_string(),
            "start",
        ])
        .env("FAKE_ZEBRAD_MODE", "ok")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn the orphan");
    pidfile::write(&node.paths.pidfile(), orphan.id(), &fake_zebrad()).expect("pid file");

    let refused = within(fx.registry.remove(&added.id, true))
        .await
        .expect_err("the orphan is still running");
    assert!(
        refused
            .to_string()
            .contains(&format!("(pid {})", orphan.id())),
        "{refused}"
    );
    assert!(fx.registry.node(&added.id).is_ok(), "the node stays listed");
    assert_eq!(fx.settings.nodes().len(), 2);
    assert!(chain.join("000001.sst").exists(), "with its data");

    // Once it is gone, the node can go.
    orphan.kill().expect("kill the orphan");
    orphan.wait().expect("reap the orphan");
    within(fx.registry.remove(&added.id, true))
        .await
        .expect("removed");
    assert!(!chain.exists());
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_node_being_removed_does_not_count_as_the_one_that_stays() {
    // The node called "Slow" ignores the stop request: its removal waits out
    // the stop grace before the kill.
    let fx = fixture_from(true, |paths| {
        config_with_modes(paths, nothing_in_use, |spec| {
            if spec.name == "Slow" {
                "hang-on-signal"
            } else {
                "ok"
            }
        })
    })
    .await;
    let first = fx.registry.first().expect("first");
    let chain = first.state_dir().join("v29").join("mainnet");
    fs::create_dir_all(&chain).expect("a chain");
    fs::write(chain.join("000001.sst"), b"blocks").expect("a block file");
    let slow = within(fx.registry.add(draft(Network::Testnet, "Slow")))
        .await
        .expect("add");
    within(
        fx.registry
            .node(&slow.id)
            .expect("lookup")
            .start(&done(), StartOptions::default()),
    )
    .await
    .expect("start");
    let removal = tokio::spawn({
        let registry = fx.registry.clone();
        let id = slow.id.clone();
        async move { registry.remove(&id, true).await }
    });
    wait_for(&fx.registry, &slow.id, "the removal's stop", |s| {
        s.phase == Phase::Stopping
    })
    .await;

    // The other node is the one that stays: it cannot go as well.
    assert_eq!(
        within(fx.registry.remove(&first.id, true)).await,
        Err(NodeError::LastNode(first.spec().name))
    );
    assert!(chain.join("000001.sst").exists(), "its data is untouched");
    within(removal)
        .await
        .expect("join")
        .expect("the slow node is removed");
    assert_eq!(fx.registry.infos(), vec![first.info()]);
    assert_eq!(fx.settings.nodes().len(), 1);
    assert!(chain.join("000001.sst").exists());
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_run_gives_the_first_node_its_networks_id_and_ports() {
    let fx = fixture_with(false, nothing_in_use).await;
    let first = fx.registry.first().expect("first");
    assert_eq!(first.id.as_str(), "zebrad-mainnet");
    let to_testnet = NodeUpdate {
        network: Some(Network::Testnet),
        ..NodeUpdate::default()
    };
    let info = within(fx.registry.update(&first.id, to_testnet))
        .await
        .expect("the first run picks Testnet");
    assert_eq!(info.id.as_str(), "zebrad-testnet");
    assert_eq!(info.name, "Zebra · Testnet");
    assert_eq!(info.ports, NodeKind::Zebrad.default_ports(Network::Testnet));
    let renamed = fx.registry.node(&info.id).expect("the new id");
    assert_ne!(renamed.serial, first.serial, "a new handle");
    assert!(fx.registry.node(&first.id).is_err());
    let toml = fs::read_to_string(renamed.paths.zebrad_config()).expect("zebrad.toml");
    assert!(toml.contains("network = \"Testnet\""), "{toml}");
    assert!(toml.contains("listen_addr = \"127.0.0.1:18232\""), "{toml}");
    assert_eq!(
        config::read_p2p_port(&renamed.paths, Network::Testnet),
        18233,
        "{toml}"
    );

    // Once the first run is done the id stays.
    within(fx.settings.update(|s| s.first_run_done = true))
        .await
        .expect("first run done");
    let back = within(fx.registry.update(
        &info.id,
        NodeUpdate {
            network: Some(Network::Mainnet),
            ..NodeUpdate::default()
        },
    ))
    .await
    .expect("back to Mainnet");
    assert_eq!(back.id.as_str(), "zebrad-testnet");
    assert_eq!(back.network, Network::Mainnet);
    assert_eq!(back.name, "Zebra · Mainnet");
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_node_list_and_its_changes_survive_a_restart() {
    let fx = fixture().await;
    let added = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let renamed = within(fx.registry.update(
        &added.id,
        NodeUpdate {
            name: Some("Test bench".into()),
            autostart: Some(false),
            ..NodeUpdate::default()
        },
    ))
    .await
    .expect("update");
    assert_eq!(renamed.name, "Test bench");
    assert!(!renamed.autostart);
    within(fx.registry.shutdown()).await;
    drop(fx.registry);
    drop(fx.settings);

    let settings = SettingsStore::open(&fx.paths).expect("reopen");
    let registry =
        registry::spawn(config(&fx.paths, nothing_in_use), settings.clone()).expect("registry");
    let infos = registry.infos();
    assert_eq!(infos.len(), 2);
    assert_eq!(infos[1], renamed);
    assert_eq!(settings.nodes()[1].layout, NodeLayout::Folder);
    within(registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_update_stops_the_running_nodes_and_remembers_them_once() {
    let fx = fixture().await;
    let second = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let first = fx.registry.first().expect("first");
    within(first.start(&done(), StartOptions::default()))
        .await
        .expect("start");
    assert_eq!(
        fx.registry.running_nodes(),
        std::slice::from_ref(&first.id),
        "what the marker lists"
    );
    within(fx.registry.stop_for_update())
        .await
        .expect("stop all");
    assert_eq!(first.supervisor.status().phase, Phase::Stopped);
    assert!(fx.registry.running_nodes().is_empty());
    let stopped = within(fx.registry.take_stopped_for_update()).await;
    assert_eq!(
        stopped,
        std::slice::from_ref(&first.id),
        "only the running node comes back"
    );
    assert!(
        within(fx.registry.take_stopped_for_update())
            .await
            .is_empty()
    );
    assert_eq!(
        fx.registry
            .node(&second.id)
            .unwrap()
            .supervisor
            .status()
            .phase,
        Phase::Stopped
    );

    // A node removed since its id was noted is not brought back.
    let noted = [second.id.clone(), first.id.clone()];
    let ids = |nodes: Vec<registry::NodeHandle>| -> Vec<NodeId> {
        nodes.into_iter().map(|node| node.id).collect()
    };
    assert_eq!(
        ids(fx.registry.nodes_of(&noted)),
        noted,
        "in the order noted"
    );
    within(fx.registry.remove(&second.id, false))
        .await
        .expect("remove");
    assert_eq!(
        ids(fx.registry.nodes_of(&noted)),
        std::slice::from_ref(&first.id)
    );
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_note_goes_to_the_node_it_names_or_to_every_node() {
    let fx = fixture().await;
    let second = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let note = |message: &str| Note {
        at: SystemTime::now(),
        level: LogLevel::Info,
        target: "zeal::test".into(),
        message: message.into(),
    };
    fx.registry
        .note(Some(&second.id), note("about the second node"));
    fx.registry.note(None, note("about every node"));
    let first = fx.registry.first().expect("first");
    let second = fx.registry.node(&second.id).expect("lookup");
    let history_a = support::within_limit(LIMIT, "first history", async {
        loop {
            let lines = first.supervisor.history_since(0).await.expect("history");
            if contains(&lines, "about every node") {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(!contains(&history_a, "about the second node"));
    let history_b = within(second.supervisor.history_since(0))
        .await
        .expect("history");
    assert!(contains(&history_b, "about the second node"));
    assert!(contains(&history_b, "about every node"));
    within(fx.registry.shutdown()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_end_of_support_follower_waits_once_the_mainnet_nodes_runtime_ended() {
    // The Testnet node ignores the stop request, so the shutdown waits out
    // its stop grace after the Mainnet node's runtime ended.
    let fx = fixture_from(true, |paths| {
        config_with_modes(paths, nothing_in_use, |spec| {
            if spec.network == Network::Testnet {
                "hang-on-signal"
            } else {
                "ok"
            }
        })
    })
    .await;
    let testnet = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let testnet = fx.registry.node(&testnet.id).expect("lookup");
    within(testnet.start(&done(), StartOptions::default()))
        .await
        .expect("start");
    let first = fx.registry.first().expect("first");
    let mut eos = fx.registry.eos_stats();
    let shutdown = tokio::spawn({
        let registry = fx.registry.clone();
        async move { registry.shutdown().await }
    });
    // The Mainnet node's monitor ends with its runtime.
    support::within_limit(LIMIT, "the Mainnet node's monitor to end", async {
        while first.stats.has_changed().is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(
        testnet.supervisor.status().phase,
        Phase::Stopping,
        "the Testnet node is still stopping"
    );
    eos.borrow_and_update();
    let mut changes = 0;
    let quiet = tokio::time::Instant::now() + Duration::from_secs(1);
    while let Ok(Ok(())) = tokio::time::timeout_at(quiet, eos.changed()).await {
        changes += 1;
    }
    assert!(changes <= 1, "{changes} changes while nothing changed");
    assert_eq!(*eos.borrow(), None);
    within(shutdown).await.expect("join");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_updater_follows_the_herd_and_the_mainnet_nodes_statistics() {
    let fx = fixture().await;
    let testnet = within(fx.registry.add(draft(Network::Testnet, "Zebra · Testnet")))
        .await
        .expect("add");
    let mut herd = fx.registry.watch_herd();
    let mut eos = fx.registry.eos_stats();
    assert_eq!(herd.borrow().phase, Phase::Stopped);

    // Only the Testnet node runs: the herd runs, but nothing reports an end of support.
    let testnet = fx.registry.node(&testnet.id).expect("lookup");
    within(testnet.start(&done(), StartOptions::default()))
        .await
        .expect("start");
    within(herd.wait_for(|status| status.phase == Phase::Running))
        .await
        .expect("the herd runs");
    let mut testnet_stats = testnet.stats.clone();
    within(testnet_stats.wait_for(Option::is_some))
        .await
        .expect("Testnet stats");
    assert_eq!(*eos.borrow(), None, "not the Testnet node's statistics");

    // The Mainnet node's statistics are the ones.
    let first = fx.registry.first().expect("first");
    within(first.start(&done(), StartOptions::default()))
        .await
        .expect("start");
    within(eos.wait_for(Option::is_some))
        .await
        .expect("the Mainnet node's statistics");
    within(fx.registry.stop_all()).await.expect("stop all");
    within(herd.wait_for(|status| status.phase == Phase::Stopped))
        .await
        .expect("the herd stops");
    within(eos.wait_for(Option::is_none))
        .await
        .expect("no statistics while it is stopped");
    within(fx.registry.shutdown()).await;
}
