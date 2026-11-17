//! `?public=…`: the light client server in the mock (docs/PUBLIC-SERVICES.md). The mock runs
//! launcher-herd's real rules over fake facts, so its screens are the backend's: each flag sets
//! up a spec and the facts around it, and the commands move them the way the backend would,
//! after short delays.
//!
//! | Flag | State |
//! | --- | --- |
//! | none | nothing set up: Settings → Public services offers the setup and "Check my connection" |
//! | `setup` | set up to the router forward, with Zebra still syncing |
//! | `serving` | open, reachable from outside 4 minutes ago, with connections and this month's upload |
//! | `unconfirmed` | open, with no evidence from outside yet |
//! | `cgnat` | the setup stopped at the network step: carrier-grade NAT |
//! | `expiring` | open, with a certificate that expires in 10 days and a renewal that failed |
//! | `keychain` | open, with a keychain that will not delete the token: "Turn off and forget" says it may still be there |
//! | `consent` | open, with a consent an earlier Zeal asked for: paused until the user agrees again |
//!
//! With zingod as the backend (docs/PUBLIC-SERVICES.md, "What can go wrong, and what the user
//! sees"), each flag sets up zingod's facts from the `/status` answers captured from zingod
//! 1.1.0 (`crates/launcher-core/tests/fixtures`), on a Testnet first node, and the rules word
//! them. Every flag but `zingod-choose`, `zingod-mainnet` and `zingod-absent` is an open
//! service.
//!
//! | Flag | State |
//! | --- | --- |
//! | `zingod-choose` | the consent screen with both servers to choose from |
//! | `zingod` | serving from zingod, with the Server card |
//! | `zingod-building` | zingod builds its index: block 1,204,331 of 4,612,019, with an ETA |
//! | `zingod-building-early` | zingod builds with no target yet (`syncing` with a `null` detail) |
//! | `zingod-catchup` | zingod `syncing` at the tip while zebrad's mempool is off |
//! | `zingod-restarting` | zebrad restarts, and zingod with it |
//! | `zingod-waiting` | zebrad hasn't reached the tip in this run: zingod starts then |
//! | `zingod-booting` | zingod opens its index (`provisioning`) |
//! | `zingod-nozebra` | zebrad is stopped: paused |
//! | `zingod-unreachable` | no validator has answered zingod for 40 s |
//! | `zingod-down` | zingod says `down`, with its message |
//! | `zingod-behind` | zingod serves 12 blocks behind zebrad |
//! | `zingod-stuck` | zingod stays at a block after its restart |
//! | `zingod-backoff` | zingod crashed, and Zeal starts it again after the backoff |
//! | `zingod-crashloop` | zingod crashed 5 times in 10 minutes |
//! | `zingod-config` | zingod refused Zeal's configuration |
//! | `zingod-cookie` | zingod can't read zebrad's cookie: it is a folder |
//! | `zingod-port` | other programs hold zingod's port and the next one |
//! | `zingod-locked` | another program holds zingod's index |
//! | `zingod-folder` | Zeal can't create zingod's folder: it isn't the user's to write |
//! | `zingod-disk` | 6 GB free: zingod waits for disk space |
//! | `zingod-broken` | zingod's index is damaged |
//! | `zingod-mismatch` | zingod's tip didn't match zebrad's: stopped until a rebuild |
//! | `zingod-tooold` | NU8 is active and zingod doesn't know it: paused |
//! | `zingod-mainnet` | the consent screen on Mainnet: zingod is being tested on Testnet first |
//! | `zingod-untested` | an open zingod service, with a zebrad zingod wasn't tested with |
//! | `zingod-absent` | the consent screen with no `zeal-zingod`: Linux only for now |

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::{Duration, SystemTime},
};

use launcher_herd::public::{
    self as rules, Checked, Facts,
    firewall::firewall_info,
    names,
    zingod::{
        Cookie, DiskReading, IndexerFacts, TipCheck, ZebradFacts, ZebradRun, ZingodFacts,
        ZingodResources,
    },
};
use launcher_types::{
    ExitClass, ExitInfo, Network, NetworkUpgrade, NodeId, Phase, UpgradeStatus,
    public::{
        AcmeProblem, CONSENT_VERSION, CertStatus, ConnectionReport, Consent, DnsProvider,
        DnsStatus, Evidence, EvidenceSource, FirewallTool, GatewayStats, LightBackend, LightdInfo,
        NetFacts, OpenError, PublicServiceSpec, PublicStatus, StepAnswer, StepId, StillPublic,
        WanKind, ZingodChoice, ZingodSpec,
    },
    zingod::{IndexerState, IndexerStatus, SyncProgress, ZingodStatus},
};
use leptos::prelude::*;

use super::{mock, query_params};
use crate::clock;

/// The mock's home addresses, from the documentation ranges.
const WAN: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 10);
const CGNAT_WAN: Ipv4Addr = Ipv4Addr::new(100, 72, 4, 9);
const LAN: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);
const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 1);
const NAME: &str = "zec-alice.duckdns.org";

/// What `?public=` starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublicFlag {
    Setup,
    Serving,
    Unconfirmed,
    Cgnat,
    Expiring,
    Keychain,
    Consent,
    Zingod(ZingodFlag),
}

/// `?public=zingod…`: zingod as the backend, one flag a row of the failure table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZingodFlag {
    Choose,
    Serving,
    Building,
    BuildingEarly,
    Catchup,
    Restarting,
    Waiting,
    Booting,
    NoZebra,
    Unreachable,
    Down,
    Behind,
    Stuck,
    Backoff,
    Crashloop,
    Config,
    Cookie,
    Port,
    Locked,
    Folder,
    Disk,
    Broken,
    Mismatch,
    TooOld,
    Mainnet,
    Untested,
    Absent,
}

impl ZingodFlag {
    const ALL: [(&'static str, ZingodFlag); 27] = [
        ("zingod-choose", ZingodFlag::Choose),
        ("zingod", ZingodFlag::Serving),
        ("zingod-building", ZingodFlag::Building),
        ("zingod-building-early", ZingodFlag::BuildingEarly),
        ("zingod-catchup", ZingodFlag::Catchup),
        ("zingod-restarting", ZingodFlag::Restarting),
        ("zingod-waiting", ZingodFlag::Waiting),
        ("zingod-booting", ZingodFlag::Booting),
        ("zingod-nozebra", ZingodFlag::NoZebra),
        ("zingod-unreachable", ZingodFlag::Unreachable),
        ("zingod-down", ZingodFlag::Down),
        ("zingod-behind", ZingodFlag::Behind),
        ("zingod-stuck", ZingodFlag::Stuck),
        ("zingod-backoff", ZingodFlag::Backoff),
        ("zingod-crashloop", ZingodFlag::Crashloop),
        ("zingod-config", ZingodFlag::Config),
        ("zingod-cookie", ZingodFlag::Cookie),
        ("zingod-port", ZingodFlag::Port),
        ("zingod-locked", ZingodFlag::Locked),
        ("zingod-folder", ZingodFlag::Folder),
        ("zingod-disk", ZingodFlag::Disk),
        ("zingod-broken", ZingodFlag::Broken),
        ("zingod-mismatch", ZingodFlag::Mismatch),
        ("zingod-tooold", ZingodFlag::TooOld),
        ("zingod-mainnet", ZingodFlag::Mainnet),
        ("zingod-untested", ZingodFlag::Untested),
        ("zingod-absent", ZingodFlag::Absent),
    ];
}

impl PublicFlag {
    fn parse(value: Option<&str>) -> Option<Self> {
        let value = value?;
        if let Some((_, flag)) = ZingodFlag::ALL.iter().find(|(name, _)| *name == value) {
            return Some(PublicFlag::Zingod(*flag));
        }
        match value {
            "setup" => Some(PublicFlag::Setup),
            "serving" => Some(PublicFlag::Serving),
            "unconfirmed" => Some(PublicFlag::Unconfirmed),
            "cgnat" => Some(PublicFlag::Cgnat),
            "expiring" => Some(PublicFlag::Expiring),
            "keychain" => Some(PublicFlag::Keychain),
            "consent" => Some(PublicFlag::Consent),
            _ => None,
        }
    }

    fn from_url() -> Option<Self> {
        let search = web_sys::window()
            .and_then(|window| window.location().search().ok())
            .unwrap_or_default();
        let params = query_params(&search);
        let value = params
            .iter()
            .find_map(|(key, value)| (key == "public").then_some(value.as_str()));
        Self::parse(value)
    }
}

/// The network the mock's first node runs: Testnet for the zingod flags, which zingod is
/// offered on, Mainnet otherwise and for `zingod-mainnet`.
pub(crate) fn first_network() -> Network {
    match PublicFlag::from_url() {
        Some(PublicFlag::Zingod(ZingodFlag::Mainnet)) | None => Network::Mainnet,
        Some(PublicFlag::Zingod(_)) => Network::Testnet,
        Some(_) => Network::Mainnet,
    }
}

/// The tip the mock's zebrad is at.
fn tip(network: Network) -> u64 {
    match network {
        Network::Mainnet => 3_544_812,
        Network::Testnet => 4_612_019,
    }
}

/// The zebrad run zingod reads in the mock.
const RUN: ZebradRun = ZebradRun {
    pid: 4242,
    started_at: SystemTime::UNIX_EPOCH,
};

const GB: u64 = 1_000_000_000;

/// zingod's port in the mock.
const ZINGOD_PORT: u16 = 18137;

/// A `/status` answer captured from zingod 1.1.0, at the mock's Testnet tip.
fn captured(json: &str, served: Option<u64>) -> IndexerStatus {
    let mut status = IndexerStatus::from(
        serde_json::from_str::<ZingodStatus>(json).expect("a captured zingod /status"),
    );
    status.network = "testnet".into();
    status.served = served.or(status.served.map(|_| tip(Network::Testnet)));
    status.tip = status.tip.map(|_| tip(Network::Testnet));
    if let IndexerState::Syncing {
        progress: Some(progress),
    } = &mut status.state
    {
        progress.height = status.served.unwrap_or(progress.height);
    }
    status
}

const SERVING: &str =
    include_str!("../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-status-serving.json");
const SYNCING_NULL: &str = include_str!(
    "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-status-syncing-null.json"
);
const SYNCING_TIP: &str = include_str!(
    "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-status-syncing-tip.json"
);
const NO_VALIDATOR: &str = include_str!(
    "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-status-no-validator.json"
);

/// zingod's last words, as captured from zingod 1.1.0 (`Error: ` taken off), with the folder
/// placeholder filled in.
fn captured_exit(text: &str, dir: &str) -> String {
    text.trim()
        .trim_start_matches("Error: ")
        .replace("<DIR>", dir)
}

/// zingod's index folder beside the mock node's data.
fn index_dir(network: Network) -> PathBuf {
    let data = mock().info.with_untracked(|info| info.data_dir.clone());
    launcher_herd::public::zingod::index_dir(std::path::Path::new(&data), network)
}

/// What the actor would know about a bundled zingod that serves at the tip.
fn zingod_serving(network: Network) -> IndexerFacts {
    let now = clock::now();
    IndexerFacts {
        bundled: true,
        version: launcher_types::zingod::version().to_owned(),
        dir: index_dir(network),
        disk: Some(DiskReading {
            free_bytes: 412 * GB,
            free_inodes: Some(24_000_000),
        }),
        network_fs: false,
        total_memory: Some(32 * GB),
        zebrad: Some(ZebradFacts {
            run: RUN,
            on_network: true,
            rpc_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 18232))),
            cookie: Cookie::Present,
            at_tip_once: true,
        }),
        runner: ZingodFacts {
            phase: Phase::Running,
            running_for: Some(RUN),
            status: Some(captured(SERVING, None)),
            disk_allows: true,
            index_bytes: Some((18_400_000_000, false)),
            resources: Some(ZingodResources {
                rss_bytes: 1_400_000_000,
                cpu_millicores: 30,
                read_bytes: 812_000_000,
                written_bytes: 18_900_000_000,
            }),
            port: Some(ZINGOD_PORT),
            ports_tried: vec![ZINGOD_PORT],
            listening: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, ZINGOD_PORT))),
            ..ZingodFacts::default()
        },
        behind_since: None,
        stuck: None,
        mismatch: None,
        tip: Some(Checked {
            at: now - Duration::from_secs(20),
            result: TipCheck::Match,
        }),
        restarting: false,
    }
}

/// A bundled zingod nobody chose yet.
fn zingod_idle(network: Network, bundled: bool) -> IndexerFacts {
    IndexerFacts {
        bundled,
        runner: ZingodFacts {
            disk_allows: true,
            ..ZingodFacts::default()
        },
        tip: None,
        ..zingod_serving(network)
    }
}

/// zingod building its index, `served` of the way, at 60 blocks a second.
fn building(served: u64) -> IndexerStatus {
    IndexerStatus {
        state: IndexerState::Syncing {
            progress: Some(SyncProgress {
                height: served,
                target: Some(tip(Network::Testnet)),
                milli_blocks_per_sec: Some(60_000),
            }),
        },
        ..captured(SYNCING_TIP, Some(served))
    }
}

/// The fake world the rules read.
#[derive(Clone, Debug)]
struct World {
    spec: Option<PublicServiceSpec>,
    endpoint: bool,
    syncing: bool,
    wan: Option<(WanKind, Ipv4Addr)>,
    name_resolves: bool,
    has_token: bool,
    cert: Option<CertStatus>,
    local_ok: bool,
    hairpin: Option<bool>,
    evidence: Vec<Evidence>,
    at_tip_since: Option<SystemTime>,
    gateway: GatewayStats,
    month_bytes_up: u64,
    /// The keychain refuses to delete the token.
    keychain_locked: bool,
    /// The zebrad the light client server reads.
    zebrad_version: String,
    phase: Phase,
    upgrades: Vec<NetworkUpgrade>,
    /// zingod, while it is bundled here: what the actor would know.
    zingod: Option<IndexerFacts>,
}

impl World {
    fn empty() -> Self {
        World {
            spec: None,
            endpoint: false,
            syncing: false,
            wan: Some((WanKind::Public, WAN)),
            name_resolves: false,
            has_token: false,
            cert: None,
            local_ok: false,
            hairpin: None,
            evidence: Vec::new(),
            at_tip_since: Some(clock::now() - Duration::from_secs(3 * 3600)),
            gateway: GatewayStats::default(),
            month_bytes_up: 0,
            keychain_locked: false,
            zebrad_version: "7.0.0".into(),
            phase: Phase::Running,
            upgrades: vec![NetworkUpgrade {
                name: "NU7".into(),
                branch_id: "77190ad9".into(),
                activation_height: 4_465_026,
                status: UpgradeStatus::Active,
            }],
            zingod: Some(zingod_idle(first_network(), true)),
        }
    }

    fn spec(network: Network) -> PublicServiceSpec {
        let node = NodeId::new(format!("zebrad-{}", network.as_str().to_ascii_lowercase()))
            .expect("a valid id");
        PublicServiceSpec::new_light_server(node, network)
    }

    /// A setup that went through the name, at the router forward.
    fn at_the_forward(network: Network) -> Self {
        let now = clock::now();
        let mut spec = Self::spec(network);
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: now - Duration::from_secs(120),
            hostname: NAME.into(),
            backend: Default::default(),
        });
        spec.name = names::parse_name(NAME, DnsProvider::DuckDns).ok();
        spec.provider = DnsProvider::DuckDns;
        spec.backend_port = Some(launcher_types::public::endpoint_port(network));
        spec.lan_addr = Some(LAN.into());
        World {
            spec: Some(spec),
            endpoint: true,
            syncing: true,
            name_resolves: true,
            has_token: true,
            at_tip_since: None,
            ..World::empty()
        }
    }

    fn certificate(days_left: u64) -> CertStatus {
        let now = clock::now();
        CertStatus {
            not_after: Some(now + Duration::from_secs(days_left * 86_400)),
            not_before: Some(now - Duration::from_secs((90 - days_left) * 86_400)),
            renew_at: Some(now + Duration::from_secs(days_left.saturating_sub(30) * 86_400)),
            staging_validated_at: Some(now - Duration::from_secs((90 - days_left) * 86_400)),
            ordering: None,
            next_try: None,
            last_problem: None,
        }
    }

    /// An open service.
    fn serving(network: Network) -> Self {
        let now = clock::now();
        let mut world = Self::at_the_forward(network);
        if let Some(spec) = &mut world.spec {
            spec.forwarded = true;
            spec.firewall_done = true;
            spec.opened = true;
        }
        world.syncing = false;
        world.at_tip_since = Some(now - Duration::from_secs(5 * 86_400));
        world.cert = Some(Self::certificate(61));
        world.local_ok = true;
        world.hairpin = Some(true);
        world.evidence = vec![Evidence {
            source: EvidenceSource::OutsideTlsConnection,
            at: now - Duration::from_secs(240),
        }];
        world.gateway = GatewayStats {
            open_connections: 3,
            client_networks: 2,
            handshakes: 1_204,
            handshakes_failed: 31,
            refused_sni: 412,
            refused_limits: 12,
            bytes_up: 1_830_000_000,
            bytes_down: 92_000_000,
        };
        world.month_bytes_up = 4_100_000_000;
        world
    }

    fn for_flag(flag: Option<PublicFlag>, network: Network) -> Self {
        match flag {
            None => World::empty(),
            Some(PublicFlag::Setup) => World::at_the_forward(network),
            Some(PublicFlag::Serving) => World::serving(network),
            Some(PublicFlag::Unconfirmed) => World {
                evidence: Vec::new(),
                hairpin: Some(false),
                ..World::serving(network)
            },
            Some(PublicFlag::Cgnat) => {
                let mut world = World::at_the_forward(network);
                world.wan = Some((WanKind::Cgnat, CGNAT_WAN));
                if let Some(spec) = &mut world.spec {
                    spec.lan_addr = None;
                }
                world.name_resolves = false;
                world
            }
            Some(PublicFlag::Expiring) => {
                let mut world = World::serving(network);
                let mut cert = World::certificate(10);
                cert.last_problem = Some(AcmeProblem::Connection);
                cert.next_try = Some(clock::now() + Duration::from_secs(4 * 3600));
                world.cert = Some(cert);
                world
            }
            Some(PublicFlag::Keychain) => World {
                keychain_locked: true,
                ..World::serving(network)
            },
            Some(PublicFlag::Consent) => {
                let mut world = World::serving(network);
                if let Some(consent) = world.spec.as_mut().and_then(|spec| spec.consent.as_mut()) {
                    consent.version = CONSENT_VERSION - 1;
                }
                world
            }
            Some(PublicFlag::Zingod(flag)) => World::zingod(flag, network),
        }
    }

    /// An open service on zingod, serving.
    fn zingod_open(network: Network) -> Self {
        let mut world = World::serving(network);
        if let Some(spec) = &mut world.spec {
            spec.backend = LightBackend::Zingod;
            spec.backend_port = None;
            spec.zingod = Some(ZingodSpec {
                port: ZINGOD_PORT,
                dir: index_dir(network),
            });
            if let Some(consent) = &mut spec.consent {
                consent.backend = LightBackend::Zingod;
            }
        }
        world.endpoint = false;
        world.zingod = Some(zingod_serving(network));
        world
    }

    /// A fresh setup at the consent screen.
    fn at_the_consent(network: Network, bundled: bool) -> Self {
        World {
            spec: Some(World::spec(network)),
            zingod: Some(zingod_idle(network, bundled)),
            ..World::empty()
        }
    }

    fn zingod(flag: ZingodFlag, network: Network) -> Self {
        let now = clock::now();
        let tip = tip(network);
        let dir = index_dir(network).display().to_string();
        let mut world = match flag {
            ZingodFlag::Choose => return World::at_the_consent(network, true),
            ZingodFlag::Mainnet => return World::at_the_consent(network, true),
            ZingodFlag::Absent => return World::at_the_consent(network, false),
            _ => World::zingod_open(network),
        };
        let fail = |world: &mut World, class: ExitClass, detail: String| {
            let runner = &mut world.zingod.as_mut().expect("zingod").runner;
            runner.phase = Phase::Failed { class };
            runner.status = None;
            runner.listening = None;
            runner.last_exit = Some(ExitInfo {
                code: Some(1),
                class,
                detail: Some(detail),
                at: now - Duration::from_secs(90),
                requested: false,
            });
        };
        fn zingod(world: &mut World) -> &mut IndexerFacts {
            world.zingod.as_mut().expect("zingod")
        }
        match flag {
            ZingodFlag::Choose | ZingodFlag::Mainnet | ZingodFlag::Absent | ZingodFlag::Serving => {
            }
            ZingodFlag::Building => {
                let runner = &mut zingod(&mut world).runner;
                runner.status = Some(building(1_204_331));
                runner.index_bytes = Some((4_900_000_000, false));
            }
            ZingodFlag::BuildingEarly => {
                let runner = &mut zingod(&mut world).runner;
                runner.status = Some(captured(SYNCING_NULL, None));
                runner.index_bytes = Some((12_000_000, false));
            }
            ZingodFlag::Catchup => {
                zingod(&mut world).runner.status = Some(captured(SYNCING_TIP, Some(tip)));
            }
            ZingodFlag::Restarting => {
                world.phase = Phase::Starting;
                let zingod = zingod(&mut world);
                zingod.runner.status = None;
                zingod.runner.listening = None;
                zingod.runner.phase = Phase::Stopped;
            }
            ZingodFlag::Waiting => {
                let zingod = zingod(&mut world);
                if let Some(zebrad) = &mut zingod.zebrad {
                    zebrad.at_tip_once = false;
                }
                zingod.runner = ZingodFacts {
                    index_bytes: zingod.runner.index_bytes,
                    disk_allows: true,
                    ..ZingodFacts::default()
                };
            }
            ZingodFlag::Booting => {
                zingod(&mut world).runner.status = Some(IndexerStatus {
                    state: IndexerState::Provisioning,
                    served: None,
                    ..captured(SERVING, None)
                });
            }
            ZingodFlag::NoZebra => {
                world.phase = Phase::Stopped;
                let zingod = zingod(&mut world);
                zingod.zebrad = None;
                zingod.runner.phase = Phase::Stopped;
                zingod.runner.status = None;
                zingod.runner.listening = None;
            }
            ZingodFlag::Unreachable => {
                let runner = &mut zingod(&mut world).runner;
                runner.status = Some(IndexerStatus {
                    answering: Some(0),
                    ..captured(SERVING, None)
                });
                runner.answering_zero_since = Some(now - Duration::from_secs(40));
            }
            ZingodFlag::Down => {
                zingod(&mut world).runner.status = Some(captured(NO_VALIDATOR, None));
            }
            ZingodFlag::Behind => {
                zingod(&mut world).runner.status = Some(captured(SERVING, Some(tip - 12)));
            }
            ZingodFlag::Stuck => {
                let zingod = zingod(&mut world);
                zingod.runner.status = Some(captured(SERVING, Some(tip - 17)));
                zingod.stuck = Some((tip - 17, tip));
            }
            ZingodFlag::Backoff => {
                let runner = &mut zingod(&mut world).runner;
                runner.phase = Phase::Backoff {
                    until: now + Duration::from_secs(4),
                    crashes: 1,
                };
                runner.status = None;
                runner.listening = None;
            }
            ZingodFlag::Crashloop => fail(
                &mut world,
                ExitClass::Crash,
                "task ended before shutdown".into(),
            ),
            ZingodFlag::Config => fail(
                &mut world,
                ExitClass::ConfigError,
                captured_exit(
                    include_str!(
                        "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-config-error.stderr.log"
                    ),
                    &dir,
                ),
            ),
            ZingodFlag::Cookie => {
                let node = mock().info.with_untracked(|info| info.data_dir.clone());
                fail(
                    &mut world,
                    ExitClass::ConfigError,
                    captured_exit(
                        include_str!(
                            "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-cookie-dir.stderr.log"
                        ),
                        &node,
                    )
                    .replace("cookie-folder/.cookie", "rpc/.cookie"),
                );
            }
            ZingodFlag::Port => {
                fail(
                    &mut world,
                    ExitClass::PortInUse,
                    captured_exit(
                        include_str!(
                            "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-port-in-use.stderr.log"
                        ),
                        &dir,
                    ),
                );
                zingod(&mut world).runner.ports_tried = vec![ZINGOD_PORT, ZINGOD_PORT + 1];
            }
            ZingodFlag::Locked => fail(
                &mut world,
                ExitClass::DbLocked,
                captured_exit(
                    include_str!(
                        "../../../crates/launcher-core/tests/fixtures/zingod-1.1.0-lock-held.stderr.log"
                    ),
                    &dir,
                ),
            ),
            ZingodFlag::Folder => {
                let runner = &mut zingod(&mut world).runner;
                runner.phase = Phase::Stopped;
                runner.status = None;
                runner.listening = None;
                runner.start_error = Some((
                    index_dir(network).join("home"),
                    "Permission denied (os error 13)".into(),
                ));
            }
            ZingodFlag::Disk => {
                let zingod = zingod(&mut world);
                zingod.disk = Some(DiskReading {
                    free_bytes: 6 * GB,
                    free_inodes: Some(24_000_000),
                });
                zingod.runner.disk_allows = false;
                zingod.runner.phase = Phase::Stopped;
                zingod.runner.status = None;
                zingod.runner.listening = None;
            }
            ZingodFlag::Broken => fail(
                &mut world,
                ExitClass::IndexBroken,
                "MANIFEST checksum mismatch".into(),
            ),
            ZingodFlag::Mismatch => {
                let zingod = zingod(&mut world);
                zingod.mismatch = Some(tip - 39);
                zingod.tip = Some(Checked {
                    at: now - Duration::from_secs(30),
                    result: TipCheck::Mismatch,
                });
                zingod.runner.phase = Phase::Stopped;
                zingod.runner.status = None;
                zingod.runner.listening = None;
            }
            ZingodFlag::TooOld => {
                world.upgrades.push(NetworkUpgrade {
                    name: "NU8".into(),
                    branch_id: "4d6e5538".into(),
                    activation_height: tip - 3_000,
                    status: UpgradeStatus::Active,
                });
                let runner = &mut zingod(&mut world).runner;
                runner.phase = Phase::Stopped;
                runner.status = None;
                runner.listening = None;
            }
            ZingodFlag::Untested => {
                world.zebrad_version = "7.1.0-alpha.0".into();
                let runner = &mut zingod(&mut world).runner;
                runner.phase = Phase::Stopped;
                runner.status = None;
                runner.listening = None;
            }
        }
        world
    }

    fn net(&self, now: SystemTime) -> Option<NetFacts> {
        let (wan, addr) = self.wan?;
        Some(NetFacts {
            lan_addr: Some(LAN.into()),
            gateway: Some(GATEWAY.into()),
            interface: Some("eth0".into()),
            vpn: None,
            wan,
            wan_addr: Some(addr.into()),
            checked_at: now,
        })
    }

    /// The facts the rules read, as the backend would gather them.
    fn facts(&self, network: Network, now: SystemTime) -> Facts {
        let mut facts = Facts::new(now, network);
        facts.zebrad_version = Some(self.zebrad_version.clone());
        facts.phase = self.phase.clone();
        facts.upgrades = self.upgrades.clone();
        facts.zingod = self.zingod.clone();
        let Some(spec) = &self.spec else {
            return facts;
        };
        let tip = tip(network);
        let height = if self.syncing { tip - 638_066 } else { tip };
        facts.height = Some(height);
        facts.tip = Some(tip);
        facts.eta_secs = self.syncing.then_some(9 * 3600);
        facts.at_tip_since = self.at_tip_since;
        if self.endpoint
            && let Some(port) = spec.backend_port
        {
            facts.endpoint = Some((Ipv4Addr::LOCALHOST, port).into());
            facts.wallet = Some(Checked {
                at: now,
                result: Ok(info(network, height)),
            });
        }
        if spec.backend == LightBackend::Zingod
            && let Some(zingod) = &self.zingod
            && zingod.runner.listening.is_some()
        {
            facts.wallet = Some(Checked {
                at: now,
                result: Ok(zingod_info(network, height)),
            });
        }
        facts.net = self.net(now);
        facts.dns = spec.name.as_ref().map(|name| DnsStatus {
            name: name.clone(),
            provider: spec.provider,
            has_token: self.has_token,
            resolved: if self.name_resolves {
                vec![IpAddr::from(WAN)]
            } else {
                Vec::new()
            },
            provider_saw: self.has_token.then_some(IpAddr::from(WAN)),
            updated_at: self.has_token.then_some(now - Duration::from_secs(90)),
            problem: None,
            checked_at: Some(now),
        });
        facts.port_free = Some(true);
        facts.firewall = Some(firewall_info(Some(FirewallTool::Ufw), spec.internal_port));
        facts.cert = self.cert.clone();
        facts.local_test = self.local_ok.then(|| Checked {
            at: now,
            result: Ok(info(network, height)),
        });
        facts.hairpin = self.hairpin.map(|result| Checked { at: now, result });
        facts.evidence = self.evidence.clone();
        facts.month_bytes_up = self.month_bytes_up;
        facts.gateway = self.gateway.clone();
        facts.awake_held = spec.opened && spec.keep_awake;
        facts
    }
}

/// What zebrad's endpoint tells wallets in the mock.
fn info(network: Network, height: u64) -> LightdInfo {
    use launcher_herd::public::wallet;
    LightdInfo {
        version: "v7.0.0+git.e8db87e".into(),
        vendor: "ZcashFoundation/zebra".into(),
        chain_name: wallet::chain_name(network).into(),
        sapling_activation_height: wallet::sapling_activation_height(network),
        consensus_branch_id: wallet::branch_id_at(network, height).unwrap_or_default(),
        block_height: height,
        zcashd_build: "v7.0.0+git.e8db87e".into(),
        zcashd_subversion: "/Zebra:7.0.0/".into(),
    }
}

/// What zingod tells wallets in the mock: no zcashd fields.
fn zingod_info(network: Network, height: u64) -> LightdInfo {
    LightdInfo {
        version: launcher_types::zingod::version().into(),
        vendor: "zingod".into(),
        zcashd_build: String::new(),
        zcashd_subversion: String::new(),
        ..info(network, height)
    }
}

#[derive(Clone)]
struct MockPublic {
    world: ArcRwSignal<World>,
    statuses: ArcRwSignal<Vec<PublicStatus>>,
}

thread_local! {
    static PUBLIC: MockPublic = MockPublic::new();
}

fn public() -> MockPublic {
    PUBLIC.with(MockPublic::clone)
}

/// Whether a light client server is set up: it serves from the mock's first node.
pub(crate) fn light_server_set_up() -> bool {
    public().world.with_untracked(|world| world.spec.is_some())
}

/// The network the mock's zebrad runs on.
fn network() -> Network {
    mock().info.with_untracked(|info| info.network)
}

impl MockPublic {
    fn new() -> Self {
        let world = ArcRwSignal::new(World::for_flag(PublicFlag::from_url(), network()));
        let statuses = ArcRwSignal::new(Vec::new());
        let mock = MockPublic { world, statuses };
        mock.publish();
        let ticker = mock.clone();
        set_interval(move || ticker.publish(), Duration::from_secs(1));
        mock
    }

    /// Runs the rules over the world and publishes the status, as the backend's actor does.
    fn publish(&self) {
        let now = clock::now();
        let network = network();
        let next = self.world.with_untracked(|world| {
            world
                .spec
                .as_ref()
                .map(|spec| rules::status(spec, &world.facts(network, now), now))
                .into_iter()
                .collect::<Vec<_>>()
        });
        if self.statuses.with_untracked(|current| *current != next) {
            self.statuses.set(next);
        }
    }

    fn update(&self, f: impl FnOnce(&mut World)) {
        self.world.update(f);
        self.publish();
    }

    /// `f` after `delay`, then a new status: the backend's work.
    fn later(&self, delay: Duration, f: impl FnOnce(&mut World) + 'static) {
        let me = self.clone();
        set_timeout(move || me.update(f), delay);
    }

    fn spec_mut(&self, f: impl FnOnce(&mut PublicServiceSpec)) -> Result<(), String> {
        let mut result = Err("no light client server is set up".to_owned());
        self.update(|world| {
            if let Some(spec) = &mut world.spec {
                f(spec);
                result = Ok(());
            }
        });
        result
    }

    /// Moves the setup along when the step it is on is the backend's to finish.
    fn advance(&self) {
        let now = clock::now();
        let network = network();
        let step = self.world.with_untracked(|world| {
            world.spec.as_ref().and_then(|spec| {
                rules::current_step(&rules::plan(spec, &world.facts(network, now)))
            })
        });
        match step {
            Some(StepId::Certificate) => {
                self.later(Duration::from_secs(2), |world| {
                    world.cert = Some(CertStatus {
                        not_after: None,
                        not_before: None,
                        renew_at: None,
                        staging_validated_at: Some(clock::now()),
                        ordering: Some(launcher_types::public::CertIssuer::Production),
                        next_try: None,
                        last_problem: None,
                    });
                    world.evidence.push(Evidence {
                        source: EvidenceSource::TlsAlpn01Validation,
                        at: clock::now(),
                    });
                });
                self.later(Duration::from_secs(4), |world| {
                    world.cert = Some(World::certificate(90));
                });
                self.later(Duration::from_secs(5), |world| world.local_ok = true);
            }
            Some(StepId::Open) => {
                self.later(Duration::from_secs(3), |world| {
                    world.syncing = false;
                    world.at_tip_since = Some(clock::now() - Duration::from_secs(11 * 60));
                });
            }
            _ => {}
        }
    }
}

pub async fn get_public_status() -> Vec<PublicStatus> {
    public().statuses.get_untracked()
}

pub fn on_public_status(handler: impl Fn(PublicStatus) + 'static) {
    let statuses = public().statuses;
    Effect::new(move |_| {
        for status in statuses.get() {
            handler(status);
        }
    });
}

pub async fn check_connection() -> Result<ConnectionReport, String> {
    let now = clock::now();
    let world = public().world.get_untracked();
    let net = world
        .net(now)
        .unwrap_or_else(|| World::empty().net(now).expect("a network"));
    let mut spec = World::spec(network());
    spec.lan_addr = net.lan_addr;
    let mut facts = Facts::new(now, network());
    facts.net = Some(net.clone());
    let checks = rules::plan(&spec, &facts)
        .into_iter()
        .find(|step| step.id == StepId::Network)
        .map(|step| {
            step.checks
                .into_iter()
                .filter(|check| check.id != launcher_types::public::CheckId::LanReserved)
                .collect()
        })
        .unwrap_or_default();
    Ok(ConnectionReport {
        net,
        checks,
        firewall: firewall_info(
            Some(FirewallTool::Ufw),
            launcher_types::public::FRONT_DOOR_PORT,
        ),
    })
}

pub async fn begin_public_setup(network_asked: Network) -> Result<NodeId, String> {
    let public = public();
    if let Some(spec) = public.world.with_untracked(|world| world.spec.clone()) {
        return Ok(spec.node);
    }
    if network_asked != network() {
        return Err(format!(
            "Zeal runs zebrad on the other network; switch to {network_asked} first"
        ));
    }
    let spec = World::spec(network_asked);
    let node = spec.node.clone();
    public.update(|world| {
        *world = World::empty();
        world.spec = Some(spec);
    });
    Ok(node)
}

pub async fn set_public_switch(on: bool) -> Result<(), String> {
    let public = public();
    public.spec_mut(|spec| spec.switch_on = on)?;
    public.later(Duration::from_secs(1), move |world| world.endpoint = on);
    Ok(())
}

pub async fn answer_public_step(answer: StepAnswer) -> Result<(), String> {
    let public = public();
    match answer {
        StepAnswer::Consent { version } => {
            if version != CONSENT_VERSION {
                return Err("the consent text changed; read it again".into());
            }
            let mut backend = LightBackend::ZebradEndpoint;
            public.spec_mut(|spec| {
                backend = spec.backend;
                spec.consent = Some(Consent {
                    version,
                    at: clock::now(),
                    hostname: spec
                        .name
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    backend: spec.backend,
                })
            })?;
            match backend {
                // zebrad restarts with its endpoint.
                LightBackend::ZebradEndpoint => {
                    public.later(Duration::from_millis(1500), |world| {
                        if let Some(spec) = &mut world.spec {
                            spec.backend_port =
                                Some(launcher_types::public::endpoint_port(spec.network));
                        }
                        world.endpoint = true;
                        if let Some(zingod) = &mut world.zingod {
                            zingod.runner = zingod_idle(network(), zingod.bundled).runner;
                            zingod.runner.index_bytes = Some((18_400_000_000, false));
                        }
                    });
                }
                // zebrad restarts without it, and zingod starts building once Zebra is at the
                // tip.
                LightBackend::Zingod => {
                    public.later(Duration::from_millis(1500), |world| {
                        if let Some(spec) = &mut world.spec {
                            spec.backend_port = None;
                        }
                        world.endpoint = false;
                        let mut zingod = zingod_serving(network());
                        zingod.runner.status = Some(building(1_204_331));
                        zingod.runner.index_bytes = Some((4_900_000_000, false));
                        zingod.tip = None;
                        world.zingod = Some(zingod);
                    });
                }
            }
        }
        StepAnswer::Name { name, provider } => {
            let name = names::parse_name(&name, provider).map_err(|err| err.to_string())?;
            public.spec_mut(|spec| {
                if let Some(consent) = &mut spec.consent
                    && consent.hostname.is_empty()
                {
                    consent.hostname = name.to_string();
                }
                spec.name = Some(name);
                spec.provider = provider;
            })?;
            if provider == DnsProvider::Manual {
                public.later(Duration::from_secs(2), |world| world.name_resolves = true);
            }
        }
        StepAnswer::Token { token } => {
            if !names::plausible_duckdns_token(&token) {
                return Err(
                    "a DuckDNS token looks like a7c4d0ad-114e-40ef-ba1d-d217904a50f2".into(),
                );
            }
            public.update(|world| world.has_token = true);
            public.later(Duration::from_secs(2), |world| world.name_resolves = true);
        }
        StepAnswer::LanReserved => public.spec_mut(|spec| spec.lan_addr = Some(LAN.into()))?,
        StepAnswer::ForwardedByHand => public.spec_mut(|spec| spec.forwarded = true)?,
        StepAnswer::FirewallDone => public.spec_mut(|spec| spec.firewall_done = true)?,
        StepAnswer::KeepAwake { on } => public.spec_mut(|spec| spec.keep_awake = on)?,
        StepAnswer::ResumeOnLaunch { on } => public.spec_mut(|spec| spec.resume_on_launch = on)?,
        StepAnswer::Upload { preset } => {
            public.spec_mut(|spec| spec.limits.upload_kbit = preset.kbit())?
        }
        StepAnswer::Backend { backend } => {
            let now = clock::now();
            let network = network();
            if backend == LightBackend::Zingod {
                let choice = public.world.with_untracked(|world| {
                    rules::zingod::choice(&world.facts(network, now), network)
                });
                if let Some(refused) = crate::public_text::zingod_choice_text(&choice)
                    .filter(|_| choice != ZingodChoice::Offered)
                {
                    return Err(refused);
                }
            }
            public.spec_mut(|spec| {
                spec.backend = backend;
                if backend == LightBackend::Zingod && spec.zingod.is_none() {
                    spec.zingod = Some(ZingodSpec {
                        port: ZINGOD_PORT,
                        dir: index_dir(network),
                    });
                }
            })?;
        }
    }
    public.advance();
    Ok(())
}

pub async fn run_public_checks(step: StepId) -> Result<(), String> {
    let public = public();
    // zingod's Try again: it starts and serves again.
    let zingod = public.world.with_untracked(|world| {
        world
            .spec
            .as_ref()
            .is_some_and(|spec| spec.backend == LightBackend::Zingod)
    });
    if step == StepId::Service && zingod {
        public.later(Duration::from_secs(2), |world| {
            let mut zingod = zingod_serving(network());
            if let Some(old) = &world.zingod {
                zingod.mismatch = old.mismatch;
            }
            world.zingod = Some(zingod);
            world.phase = Phase::Running;
            world.zebrad_version = "7.0.0".into();
        });
    }
    // "Try again" on the Serving panel: the renewal goes through this time.
    let failing = public.world.with_untracked(|world| {
        world.spec.as_ref().is_some_and(|spec| spec.opened)
            && world
                .cert
                .as_ref()
                .is_some_and(|cert| cert.last_problem.is_some())
    });
    if step == StepId::Certificate && failing {
        public.later(Duration::from_secs(2), |world| {
            world.cert = Some(World::certificate(90));
        });
    }
    public.publish();
    public.advance();
    Ok(())
}

pub async fn open_public_service() -> Result<(), OpenError> {
    let public = public();
    let now = clock::now();
    let network = network();
    let gates = public.world.with_untracked(|world| match &world.spec {
        Some(spec) => rules::open_gates(spec, &world.facts(network, now)),
        None => Err(OpenError {
            check: None,
            message: "no light client server is set up".into(),
        }),
    });
    gates?;
    let _ = public.spec_mut(|spec| spec.opened = true);
    public.later(Duration::from_secs(1), |world| world.hairpin = Some(true));
    public.later(Duration::from_secs(4), |world| {
        world.evidence.push(Evidence {
            source: EvidenceSource::OutsideTlsConnection,
            at: clock::now(),
        });
        world.gateway.open_connections = 1;
        world.gateway.client_networks = 1;
        world.gateway.handshakes = 1;
    });
    Ok(())
}

pub async fn forget_public_service(
    remove_token: bool,
    _remove_index: bool,
) -> Result<StillPublic, String> {
    let public = public();
    let (spec, locked) = public
        .world
        .with_untracked(|world| world.spec.clone().map(|spec| (spec, world.keychain_locked)))
        .ok_or("no light client server is set up")?;
    public.update(|world| *world = World::empty());
    Ok(StillPublic {
        forward: spec
            .forwarded
            .then_some((spec.external_port, spec.lan_addr, spec.internal_port)),
        firewall_remove: if spec.firewall_done {
            firewall_info(Some(FirewallTool::Ufw), spec.internal_port).remove
        } else {
            Vec::new()
        },
        name: spec.name,
        token_left: remove_token && locked,
    })
}

pub async fn open_signup(_provider: DnsProvider) -> Result<(), String> {
    Ok(())
}

pub async fn rebuild_public_index() -> Result<(), String> {
    let public = public();
    public.update(|world| {
        if let Some(zingod) = &mut world.zingod {
            zingod.mismatch = None;
            zingod.stuck = None;
            zingod.tip = None;
            zingod.runner = ZingodFacts {
                status: Some(captured(SYNCING_NULL, None)),
                index_bytes: Some((0, false)),
                ..zingod_serving(network()).runner
            };
        }
    });
    public.later(Duration::from_secs(4), |world| {
        if let Some(zingod) = &mut world.zingod {
            zingod.runner.status = Some(building(12_000));
            zingod.runner.index_bytes = Some((40_000_000, false));
        }
    });
    Ok(())
}

pub async fn delete_public_index() -> Result<(), String> {
    let public = public();
    let zingod = public.world.with_untracked(|world| {
        world
            .spec
            .as_ref()
            .is_some_and(|spec| spec.backend == LightBackend::Zingod)
    });
    if zingod {
        return Err(
            "zingod serves the light client server: switch back to Zebra's built-in server first"
                .into(),
        );
    }
    public.update(|world| {
        if let Some(zingod) = &mut world.zingod {
            zingod.runner.index_bytes = None;
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_zingod_flag_parses() {
        for (name, flag) in ZingodFlag::ALL {
            assert_eq!(
                PublicFlag::parse(Some(name)),
                Some(PublicFlag::Zingod(flag)),
                "{name}"
            );
        }
    }

    #[test]
    fn flags_parse() {
        assert_eq!(
            PublicFlag::parse(Some("serving")),
            Some(PublicFlag::Serving)
        );
        assert_eq!(PublicFlag::parse(Some("cgnat")), Some(PublicFlag::Cgnat));
        assert_eq!(
            PublicFlag::parse(Some("keychain")),
            Some(PublicFlag::Keychain)
        );
        assert_eq!(
            PublicFlag::parse(Some("consent")),
            Some(PublicFlag::Consent)
        );
        assert_eq!(PublicFlag::parse(Some("nonsense")), None);
        assert_eq!(PublicFlag::parse(None), None);
    }
}
