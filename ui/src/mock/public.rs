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

use std::{
    net::{IpAddr, Ipv4Addr},
    time::{Duration, SystemTime},
};

use launcher_herd::public::{self as rules, Checked, Facts, firewall::firewall_info, names};
use launcher_types::{
    Network, NodeId, Phase,
    public::{
        AcmeProblem, CONSENT_VERSION, CertStatus, ConnectionReport, Consent, DnsProvider,
        DnsStatus, Evidence, EvidenceSource, FirewallTool, GatewayStats, LightdInfo, NetFacts,
        OpenError, PublicServiceSpec, PublicStatus, StepAnswer, StepId, StillPublic, WanKind,
    },
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
}

impl PublicFlag {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value? {
            "setup" => Some(PublicFlag::Setup),
            "serving" => Some(PublicFlag::Serving),
            "unconfirmed" => Some(PublicFlag::Unconfirmed),
            "cgnat" => Some(PublicFlag::Cgnat),
            "expiring" => Some(PublicFlag::Expiring),
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
        }
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
        facts.zebrad_version = Some("7.0.0".into());
        facts.phase = Phase::Running;
        let Some(spec) = &self.spec else {
            return facts;
        };
        let tip = 3_544_812;
        let height = if self.syncing { 2_906_746 } else { tip };
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

/// The network the mock's zebrad runs on.
fn network() -> Network {
    mock().settings.with_untracked(|settings| settings.network)
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
            public.spec_mut(|spec| {
                spec.consent = Some(Consent {
                    version,
                    at: clock::now(),
                    hostname: spec
                        .name
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                })
            })?;
            // zebrad restarts with its endpoint.
            public.later(Duration::from_millis(1500), |world| {
                if let Some(spec) = &mut world.spec {
                    spec.backend_port = Some(launcher_types::public::endpoint_port(spec.network));
                }
                world.endpoint = true;
            });
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
    }
    public.advance();
    Ok(())
}

pub async fn run_public_checks(_step: StepId) -> Result<(), String> {
    let public = public();
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

pub async fn forget_public_service(_remove_token: bool) -> Result<StillPublic, String> {
    let public = public();
    let spec = public
        .world
        .with_untracked(|world| world.spec.clone())
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
    })
}

pub async fn open_signup(_provider: DnsProvider) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse() {
        assert_eq!(
            PublicFlag::parse(Some("serving")),
            Some(PublicFlag::Serving)
        );
        assert_eq!(PublicFlag::parse(Some("cgnat")), Some(PublicFlag::Cgnat));
        assert_eq!(PublicFlag::parse(Some("nonsense")), None);
        assert_eq!(PublicFlag::parse(None), None);
    }
}
