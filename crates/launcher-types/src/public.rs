//! Public services (docs/PUBLIC-SERVICES.md): a light client server that wallets reach from
//! the internet through Zeal's TLS front door. PS1 builds the light client server, served by
//! zebrad's own light wallet endpoint, on Linux. It is experimental, and labelled so.
//!
//! These are the wire and `launcher.json` types. The rules that read them (the setup plan,
//! pauses, evidence ages, the wallet check) are in `launcher-herd`'s `public` module, so the
//! backend and the browser mock run the same logic.

use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

use crate::{Network, NodeId};

/// The version of the consent text. A [`Consent`] given for an older version, or for another
/// name, is asked for again.
pub const CONSENT_VERSION: u32 = 1;

/// The port wallets use: the router forwards it to [`FRONT_DOOR_PORT`] on this computer.
pub const EXTERNAL_PORT: u16 = 443;

/// The port the front door listens on ([`crate::PortRole::PublicTls`]). The router's
/// translation from 443 means nothing needs elevated rights.
pub const FRONT_DOOR_PORT: u16 = 8443;

/// The loopback port Zeal gives zebrad's light wallet endpoint on `network`, or the next free
/// one above it: 9067 on Mainnet, 19067 on Testnet.
pub fn endpoint_port(network: Network) -> u16 {
    match network {
        Network::Mainnet => 9067,
        Network::Testnet => 19067,
    }
}

/// The services Zeal can make public. The DNS seeder joins in a later phase (PS4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicService {
    /// zebrad's light wallet endpoint behind the front door.
    LightServer,
}

/// A public DNS name: LDH labels of 1 to 63 characters, at least two of them, at most 253
/// bytes, lowercase. Not an address, and not a name that only works on a local network
/// (`.local`, `.home.arpa`, `.internal`, `.invalid`, `.localhost`, `.onion`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PublicHostname(String);

/// Why a name is not a [`PublicHostname`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostnameError {
    Empty,
    TooLong,
    /// One label, such as `localhost`.
    SingleLabel,
    /// A label that is empty, longer than 63 characters, or has a character other than a
    /// letter, a digit or an inner hyphen.
    BadLabel,
    /// An IP address, or a name whose last label is all digits.
    Address,
    /// A suffix that only works on a local network.
    LocalOnly,
}

impl fmt::Display for HostnameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HostnameError::Empty => "the name is empty",
            HostnameError::TooLong => "the name is longer than 253 characters",
            HostnameError::SingleLabel => "the name needs a dot, as in zec.example.com",
            HostnameError::BadLabel => {
                "each part of the name may use letters, digits and inner hyphens, up to 63 of them"
            }
            HostnameError::Address => "an address is not a name",
            HostnameError::LocalOnly => "this name only works on a local network",
        })
    }
}

impl std::error::Error for HostnameError {}

/// Name suffixes that never resolve on the public internet.
const LOCAL_SUFFIXES: [&str; 6] = [
    "local",
    "home.arpa",
    "internal",
    "invalid",
    "localhost",
    "onion",
];

impl PublicHostname {
    /// `name`, trimmed, lowercased and without a trailing dot, if it is a public name.
    pub fn parse(name: &str) -> Result<Self, HostnameError> {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() {
            return Err(HostnameError::Empty);
        }
        if name.parse::<IpAddr>().is_ok() {
            return Err(HostnameError::Address);
        }
        if name.len() > 253 {
            return Err(HostnameError::TooLong);
        }
        let labels: Vec<&str> = name.split('.').collect();
        if labels.len() < 2 {
            return Err(HostnameError::SingleLabel);
        }
        let label_ok = |label: &str| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        };
        if !labels.iter().all(|label| label_ok(label)) {
            return Err(HostnameError::BadLabel);
        }
        if labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(HostnameError::Address);
        }
        let local = LOCAL_SUFFIXES
            .iter()
            .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")));
        if local {
            return Err(HostnameError::LocalOnly);
        }
        Ok(PublicHostname(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PublicHostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for PublicHostname {
    type Error = HostnameError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        PublicHostname::parse(&name)
    }
}

impl From<PublicHostname> for String {
    fn from(name: PublicHostname) -> Self {
        name.0
    }
}

/// Who keeps the name's address record. deSEC and Cloudflare follow in later phases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsProvider {
    /// A free `name.duckdns.org`: Zeal sets and keeps its address with the user's token.
    DuckDns,
    /// A record the user keeps by hand, anywhere: Zeal only checks it.
    #[default]
    Manual,
}

impl DnsProvider {
    /// The provider's name for people.
    pub fn label(self) -> &'static str {
        match self {
            DnsProvider::DuckDns => "DuckDNS",
            DnsProvider::Manual => "by hand",
        }
    }
}

/// Where a provider token is kept: an entry in the OS keychain, never the token itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SecretRef {
    pub provider: DnsProvider,
    /// The keychain account, `dns:<provider>:<zone>`.
    pub account: String,
}

/// How Let's Encrypt checks that the name points here. DNS-01 follows in PS6.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Challenge {
    /// A TLS connection to the external port 443, answered by the front door.
    #[default]
    TlsAlpn01,
}

/// How the router forwards the port. Automatic mapping follows in PS6.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mapping {
    /// The user adds the forward in the router's settings.
    #[default]
    ByHand,
}

/// The front door's limits. The defaults are proposals for a home connection, to be tuned by
/// the wallet runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicLimits {
    /// Open connections from all clients together.
    pub max_connections: u32,
    /// Open connections from one IPv4 address or one IPv6 /64.
    pub per_address: u32,
    /// New connections a second from one address, after the burst.
    pub new_per_second: u32,
    /// New connections from one address in a burst.
    pub new_burst: u32,
    /// The upload budget in kbit/s, `None` for no limit.
    pub upload_kbit: Option<u32>,
    /// The upload allowed in a calendar month, in GB; the service pauses past it. `None` for
    /// no cap.
    pub monthly_gb: Option<u32>,
}

impl Default for PublicLimits {
    fn default() -> Self {
        PublicLimits {
            max_connections: 128,
            per_address: 8,
            new_per_second: 2,
            new_burst: 16,
            upload_kbit: Some(UploadPreset::Standard.kbit().unwrap_or(10_000)),
            monthly_gb: Some(300),
        }
    }
}

/// The upload presets the setup offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadPreset {
    /// 5 Mbit/s.
    Light,
    /// 10 Mbit/s.
    Standard,
    /// No limit.
    Unlimited,
}

impl UploadPreset {
    pub const ALL: [UploadPreset; 3] = [
        UploadPreset::Light,
        UploadPreset::Standard,
        UploadPreset::Unlimited,
    ];

    /// The preset's budget in kbit/s.
    pub fn kbit(self) -> Option<u32> {
        match self {
            UploadPreset::Light => Some(5_000),
            UploadPreset::Standard => Some(10_000),
            UploadPreset::Unlimited => None,
        }
    }

    /// The preset a budget matches, if any.
    pub fn of(kbit: Option<u32>) -> Option<UploadPreset> {
        UploadPreset::ALL
            .into_iter()
            .find(|preset| preset.kbit() == kbit)
    }
}

/// The user's agreement to what becomes public: for a text version and a name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    pub version: u32,
    pub at: SystemTime,
    pub hostname: String,
}

/// One public service as `launcher.json` keeps it, under `public.services`. No secrets.
///
/// Only the public-services commands change it: `set_settings` keeps the stored copy and
/// `get_settings` leaves it out, so neither a stale page nor the webview can switch a service
/// on or give consent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicServiceSpec {
    pub service: PublicService,
    /// The serving node: in PS1, Zeal's one zebrad (`zebrad-mainnet` or `zebrad-testnet`).
    pub node: NodeId,
    /// The network the service was set up on. In PS1 it serves only while the launcher runs
    /// zebrad on this network.
    pub network: Network,
    /// The user's switch. Off pauses everything at once.
    pub switch_on: bool,
    /// Open again when Zeal starts, if every gate passes.
    #[serde(default = "default_true")]
    pub resume_on_launch: bool,
    #[serde(default)]
    pub consent: Option<Consent>,
    #[serde(default)]
    pub name: Option<PublicHostname>,
    #[serde(default)]
    pub provider: DnsProvider,
    #[serde(default)]
    pub secret: Option<SecretRef>,
    /// The router's port wallets use.
    #[serde(default = "default_external_port")]
    pub external_port: u16,
    /// The front door's port on this computer.
    #[serde(default = "default_internal_port")]
    pub internal_port: u16,
    /// zebrad's light wallet endpoint on loopback, set only while the service is on: Zeal
    /// writes `rpc.lightwalletd_listen_addr` from it, and removes the key while it is `None`.
    #[serde(default)]
    pub backend_port: Option<u16>,
    /// The LAN address the user reserved in the router for the forward.
    #[serde(default)]
    pub lan_addr: Option<IpAddr>,
    /// The user added the router forward.
    #[serde(default)]
    pub forwarded: bool,
    /// The user went through the firewall step.
    #[serde(default)]
    pub firewall_done: bool,
    /// "Open to the internet" succeeded once: later resumes go straight to the gates.
    #[serde(default)]
    pub opened: bool,
    #[serde(default)]
    pub challenge: Challenge,
    #[serde(default)]
    pub mapping: Mapping,
    #[serde(default)]
    pub limits: PublicLimits,
    /// Keep this computer awake while serving.
    #[serde(default = "default_true")]
    pub keep_awake: bool,
}

fn default_true() -> bool {
    true
}

fn default_external_port() -> u16 {
    EXTERNAL_PORT
}

fn default_internal_port() -> u16 {
    FRONT_DOOR_PORT
}

impl PublicServiceSpec {
    /// A light client server just set up for `node` on `network`: switched on, waiting for
    /// consent, with the default ports and limits.
    pub fn new_light_server(node: NodeId, network: Network) -> Self {
        PublicServiceSpec {
            service: PublicService::LightServer,
            node,
            network,
            switch_on: true,
            resume_on_launch: true,
            consent: None,
            name: None,
            provider: DnsProvider::default(),
            secret: None,
            external_port: EXTERNAL_PORT,
            internal_port: FRONT_DOOR_PORT,
            backend_port: None,
            lan_addr: None,
            forwarded: false,
            firewall_done: false,
            opened: false,
            challenge: Challenge::default(),
            mapping: Mapping::default(),
            limits: PublicLimits::default(),
            keep_awake: true,
        }
    }

    /// Whether the stored consent covers the current text and name. Consent given before a
    /// name was chosen has an empty `hostname`, and the first name chosen is bound to it;
    /// another name asks again.
    pub fn consent_current(&self) -> bool {
        self.consent.as_ref().is_some_and(|consent| {
            consent.version == CONSENT_VERSION
                && (consent.hostname.is_empty()
                    || self
                        .name
                        .as_ref()
                        .is_some_and(|name| name.as_str() == consent.hostname))
        })
    }
}

/// `launcher.json`'s `public` section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PublicConfig {
    pub services: Vec<PublicServiceSpec>,
}

impl PublicConfig {
    /// Nothing is set up.
    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }

    /// The light client server, if one is set up.
    pub fn light_server(&self) -> Option<&PublicServiceSpec> {
        self.services
            .iter()
            .find(|spec| spec.service == PublicService::LightServer)
    }

    /// The light client server, to change it.
    pub fn light_server_mut(&mut self) -> Option<&mut PublicServiceSpec> {
        self.services
            .iter_mut()
            .find(|spec| spec.service == PublicService::LightServer)
    }
}

/// Where a service is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PublicState {
    /// Not set up.
    Off,
    /// It cannot run here.
    NotAvailable { why: NotAvailable },
    /// The guided setup is on this step.
    SettingUp { step: StepId },
    /// Open to the internet, and the backend answers.
    Serving,
    /// The front door is up, but tells wallets to try again.
    Degraded { why: DegradedReason },
    /// The front door is closed.
    Paused {
        why: PauseReason,
        since: SystemTime,
        resumes: Resume,
    },
}

/// Why a service cannot run here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotAvailable {
    /// PS1 runs on Linux only.
    NotOnThisSystem,
    /// The zebrad isn't one Zeal has tested as a light client server.
    UntestedZebrad { version: String },
}

/// Why the front door tells wallets to try again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DegradedReason {
    /// zebrad is restarting or its endpoint is not open yet.
    BackingRestarting,
    /// zebrad is behind the network's tip.
    BackingBehind { blocks: u64 },
    /// zebrad's light wallet endpoint gave a wrong answer to the wallet check.
    WalletCheckFailed,
    /// The certificate expired and could not be renewed.
    CertificateExpired,
}

/// Why the front door is closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PauseReason {
    /// The switch is off.
    ByYou,
    /// This computer's LAN address is no longer the one the forward targets.
    LanAddressChanged { was: IpAddr },
    /// The name has pointed elsewhere for 15 minutes.
    NameElsewhere { points_to: Vec<IpAddr> },
    /// A VPN carries the default route.
    VpnRoute { interface: String },
    /// The month's upload reached the cap.
    DataCap,
    /// zebrad stopped, failed or halted.
    BackingStopped,
    /// The launcher runs zebrad on the other network now.
    NetworkChanged,
    /// Zeal started and did not open the service by itself.
    NotResumed,
}

/// How a paused service comes back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resume {
    /// By itself, once the cause clears.
    Automatic,
    /// Only by the user's switch.
    NeedsYou,
    /// When the next month starts.
    NextMonth,
}

/// What the front door accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayMode {
    /// Only Let's Encrypt's challenge for the pending name.
    ChallengeOnly,
    /// Challenges, and service connections from this computer.
    LocalOnly,
    /// Everyone, within the limits.
    Open,
    /// Challenges; service handshakes complete and every call gets `UNAVAILABLE`.
    Unavailable,
}

/// The steps of the guided setup, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    Consent,
    Service,
    Network,
    Name,
    Port,
    Firewall,
    Certificate,
    LocalTest,
    Open,
    KeepRunning,
    Share,
}

impl StepId {
    pub const ALL: [StepId; 11] = [
        StepId::Consent,
        StepId::Service,
        StepId::Network,
        StepId::Name,
        StepId::Port,
        StepId::Firewall,
        StepId::Certificate,
        StepId::LocalTest,
        StepId::Open,
        StepId::KeepRunning,
        StepId::Share,
    ];
}

/// One step of the setup with its checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: StepId,
    pub state: StepState,
    pub checks: Vec<CheckResult>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// An earlier step is not done.
    Waiting,
    /// Its checks are running.
    Running,
    Done,
    /// It waits for the user's answer or action.
    NeedsYou,
    /// A check failed; the finding says the fix.
    Failed,
}

/// Every check the setup and the watches run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    /// The consent covers the current text and name.
    Consent,
    /// The platform is one PS1 supports.
    Platform,
    /// zebrad is a version Zeal tested as a light client server.
    ZebradTested,
    /// zebrad logged its light wallet endpoint at the assigned address.
    EndpointOpened,
    /// The endpoint's `GetLightdInfo` passes the wallet check.
    WalletCheck,
    /// zebrad is at the network's tip.
    AtTip,
    /// This computer has a LAN address and a gateway.
    LanAddress,
    /// No VPN carries the default route.
    NoVpn,
    /// The router has a public address.
    WanAddress,
    /// The user reserved the LAN address in the router.
    LanReserved,
    /// The name is set and valid.
    NameChosen,
    /// The provider token is in the keychain.
    Token,
    /// The provider took the address update.
    AddressUpdate,
    /// The name resolves to the router's public address.
    NameResolves,
    /// The front door's port is free on this computer.
    PortFree,
    /// The user added the router forward.
    Forward,
    /// The firewall step was shown.
    Firewall,
    /// Let's Encrypt staging validated the name through the forward.
    StagingCertificate,
    /// A production certificate was issued and verifies.
    Certificate,
    /// A wallet's connection from this computer through the front door passes.
    LocalTest,
    /// zebrad has been at the tip for the last 10 minutes.
    TipSettled,
    /// A connection to the public address reached the front door.
    Hairpin,
    /// The keep-awake choice was made.
    KeepAwake,
}

/// A check's result, with when it ran.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    pub id: CheckId,
    pub outcome: CheckOutcome,
    pub at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CheckOutcome {
    NotRun,
    Running,
    Pass,
    Warn { finding: Finding },
    Fail { finding: Finding },
}

impl CheckOutcome {
    pub fn passed(&self) -> bool {
        matches!(self, CheckOutcome::Pass | CheckOutcome::Warn { .. })
    }
}

/// What a check found, as typed facts; the UI words it (`ui/src/public_text.rs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Finding {
    ConsentNeeded,
    NotOnThisSystem,
    UntestedZebrad {
        version: String,
    },
    ZebradNotRunning,
    /// The endpoint did not open yet after zebrad's start.
    EndpointNotOpen,
    /// zebrad could not bind the endpoint's port.
    EndpointPortTaken {
        port: u16,
    },
    /// Zeal could not write the endpoint into `zebrad.toml` and restart zebrad, for example
    /// while a configuration change was being applied.
    EndpointNotWritten {
        detail: String,
    },
    WalletCheck {
        problem: WalletProblem,
    },
    /// zebrad is still syncing: wallets can use the server once it reaches the tip.
    Syncing {
        height: u64,
        tip: u64,
        eta_secs: Option<u64>,
    },
    NoLanAddress,
    Vpn {
        interface: String,
    },
    /// Carrier-grade NAT: no inbound IPv4.
    Cgnat {
        wan: IpAddr,
    },
    /// The router's own address is private: a second router sits in front of it.
    BehindAnotherRouter {
        wan: IpAddr,
    },
    /// The router did not say its public address.
    WanUnknown,
    LanNotReserved {
        lan: IpAddr,
    },
    NameMissing,
    BadName {
        error: HostnameError,
    },
    TokenMissing,
    /// The keychain could not keep the token.
    KeychainUnavailable,
    AddressUpdateFailed {
        problem: DdnsProblem,
    },
    /// The name does not resolve yet.
    NameNotFound,
    NamePointsElsewhere {
        points_to: Vec<IpAddr>,
        expected: Option<IpAddr>,
    },
    /// An IPv6 record Zeal did not check: Let's Encrypt tries IPv6 first.
    NameHasIpv6 {
        addrs: Vec<IpAddr>,
    },
    /// What the provider saw differs from what the router says (CGNAT or a second router).
    ProviderSawOther {
        provider_saw: IpAddr,
        router: IpAddr,
    },
    PortTaken {
        port: u16,
    },
    ForwardNotConfirmed,
    /// A firewall tool is on this computer: the commands to allow the port.
    Firewall {
        tool: FirewallTool,
        port: u16,
    },
    Acme {
        problem: AcmeProblem,
    },
    LocalTestFailed {
        problem: LocalTestProblem,
    },
    /// zebrad must stay at the tip for 10 minutes first.
    TipNotSettled {
        since: Option<SystemTime>,
    },
    /// The router does not send this computer's own connections to the public address back
    /// in: a phone at home then needs mobile data. It proves nothing about the forward.
    NoHairpin,
    BehindTip {
        blocks: u64,
    },
}

/// What was wrong with an endpoint's `GetLightdInfo` against zebrad's own answer, as Zodl's
/// `validateServerEndpoint` checks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WalletProblem {
    /// No answer, or not a gRPC answer.
    NoAnswer {
        detail: String,
    },
    WrongChain {
        got: String,
        want: String,
    },
    WrongSaplingHeight {
        got: u64,
        want: u64,
    },
    WrongBranch {
        got: String,
        want: String,
    },
    /// The endpoint's height is too far from zebrad's.
    HeightOff {
        got: u64,
        want: u64,
    },
}

/// A dynamic DNS update's failure, by kind: no URL, token or address in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DdnsProblem {
    /// The provider refused the token or the name (DuckDNS's `KO`).
    Refused,
    /// The provider could not be reached.
    Unreachable,
    /// The provider answered something Zeal does not understand.
    BadAnswer,
    /// Too many updates: Zeal stopped asking for a while.
    TooMany,
}

/// The firewall tools Zeal shows commands for. It never runs them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirewallTool {
    Ufw,
    Firewalld,
}

/// An ACME failure, mapped to its fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AcmeProblem {
    /// Let's Encrypt could not connect: the forward, the firewall, or the provider blocking
    /// 443.
    Connection,
    /// The name points elsewhere, or has not updated yet.
    Dns,
    /// The challenge answer was wrong or missing.
    Unauthorized,
    /// A CAA record forbids Let's Encrypt.
    Caa,
    /// Rate-limited: Zeal tries again after `retry_at`.
    RateLimited { retry_at: Option<SystemTime> },
    /// Zeal's own budget of orders is spent until `retry_at`.
    AttemptsSpent { retry_at: SystemTime },
    /// The issued chain does not verify for the name.
    Verify,
    /// Let's Encrypt could not be reached.
    Unreachable,
    /// Anything else, with Let's Encrypt's own words.
    Other { detail: String },
}

/// Why the test on this computer failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LocalTestProblem {
    /// No TLS connection to the front door.
    Connect {
        detail: String,
    },
    /// The certificate does not verify for the name.
    Certificate {
        detail: String,
    },
    Wallet {
        problem: WalletProblem,
    },
}

/// What produced evidence that the service is reachable from the internet. DNS-01 issuance
/// is never evidence, so there is no variant for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    /// Let's Encrypt connected to the external port and completed the TLS-ALPN-01 challenge.
    TlsAlpn01Validation,
    /// A public address, not the router's, completed TLS with the right name.
    OutsideTlsConnection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub at: SystemTime,
}

/// Whether the service is reachable from the internet, and from what evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reachability {
    NotChecked,
    /// Checked, with no evidence yet.
    NotYet,
    /// Evidence under 24 hours old.
    Proven {
        latest: Evidence,
    },
    /// Evidence up to 7 days old: "last confirmed 3 days ago".
    Recent {
        latest: Evidence,
    },
    /// Older evidence.
    Stale {
        latest: Evidence,
    },
    /// A check that passed now fails.
    Lost {
        since: SystemTime,
        why: LostReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LostReason {
    /// Let's Encrypt could not connect for a renewal.
    AcmeConnection,
}

/// How the router reaches the internet, from its own answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WanKind {
    /// A public IPv4 address.
    Public,
    /// 100.64.0.0/10: carrier-grade NAT, no inbound IPv4.
    Cgnat,
    /// A private address: another router sits in front.
    BehindAnotherRouter,
    /// The router did not answer.
    Unknown,
}

/// What this computer and its router say about the network: read-only checks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetFacts {
    /// This computer's address on the default route.
    pub lan_addr: Option<IpAddr>,
    /// The default route's gateway.
    pub gateway: Option<IpAddr>,
    /// The default route's interface.
    pub interface: Option<String>,
    /// The VPN interface carrying the default route, if any (a heuristic).
    pub vpn: Option<String>,
    pub wan: WanKind,
    /// The router's public address, from NAT-PMP.
    pub wan_addr: Option<IpAddr>,
    pub checked_at: SystemTime,
}

/// The name's state: what it resolves to and what the provider did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsStatus {
    pub name: PublicHostname,
    pub provider: DnsProvider,
    /// A token is in the keychain for this name. The token itself never leaves the backend.
    pub has_token: bool,
    /// The addresses the system resolver returns.
    pub resolved: Vec<IpAddr>,
    /// The address the provider saw, from its answer.
    pub provider_saw: Option<IpAddr>,
    /// The last address update the provider took.
    pub updated_at: Option<SystemTime>,
    pub problem: Option<DdnsProblem>,
    pub checked_at: Option<SystemTime>,
}

/// Which Let's Encrypt a certificate comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertIssuer {
    /// Let's Encrypt's staging environment: validates, but no wallet trusts it.
    Staging,
    /// The real one.
    Production,
}

/// The certificate's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertStatus {
    /// The production certificate's expiry, once there is one.
    pub not_after: Option<SystemTime>,
    pub not_before: Option<SystemTime>,
    /// When Zeal renews it.
    pub renew_at: Option<SystemTime>,
    /// The last staging validation through the forward.
    pub staging_validated_at: Option<SystemTime>,
    /// An order is in flight.
    pub ordering: Option<CertIssuer>,
    /// When Zeal tries again after a failure.
    pub next_try: Option<SystemTime>,
    pub last_problem: Option<AcmeProblem>,
}

/// The front door's counters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStats {
    pub open_connections: u32,
    /// Distinct client networks (IPv4 addresses or IPv6 /64s) among the open connections.
    pub client_networks: u32,
    pub handshakes: u64,
    pub handshakes_failed: u64,
    /// Handshakes refused for a wrong or missing name.
    pub refused_sni: u64,
    /// Connections refused by the limits.
    pub refused_limits: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// The light wallet endpoint as the backend sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightStats {
    /// The address zebrad logged as opened, on loopback.
    pub endpoint: Option<SocketAddr>,
    /// zebrad's height and the network's, from the RPC monitor.
    pub height: Option<u64>,
    pub tip: Option<u64>,
    /// What wallets are told, from the last `GetLightdInfo`.
    pub info: Option<LightdInfo>,
    pub checked_at: Option<SystemTime>,
    pub gateway: GatewayStats,
    /// The upload this month, in bytes.
    pub month_bytes_up: u64,
}

/// The fields of the lightwalletd protocol's `LightdInfo` that Zeal reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LightdInfo {
    pub version: String,
    pub vendor: String,
    pub chain_name: String,
    pub sapling_activation_height: u64,
    pub consensus_branch_id: String,
    pub block_height: u64,
    pub zcashd_build: String,
    pub zcashd_subversion: String,
}

/// What wallets enter, per wallet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareInfo {
    /// Zodl: `host:port`, Custom server in Manual mode.
    pub zodl: String,
    /// Zingo and Zkool: `https://host:port`.
    pub zingo: String,
}

/// The firewall step's facts: the tool on `PATH` and its commands. Zeal never runs them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallInfo {
    pub tool: Option<FirewallTool>,
    /// The commands that allow the front door's port, to copy.
    pub allow: Vec<String>,
    /// The commands that remove the rule again.
    pub remove: Vec<String>,
}

/// One service's state for the UI: the `public_status` event and `get_public_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicStatus {
    pub node: NodeId,
    pub service: PublicService,
    pub network: Network,
    pub state: PublicState,
    pub switch_on: bool,
    /// The front door's mode, `None` while it is stopped.
    pub gateway: Option<GatewayMode>,
    pub reach: Reachability,
    pub steps: Vec<Step>,
    pub name: Option<PublicHostname>,
    pub provider: DnsProvider,
    pub external_port: u16,
    pub internal_port: u16,
    pub lan_addr: Option<IpAddr>,
    pub consent: Option<Consent>,
    pub opened: bool,
    pub keep_awake: bool,
    /// The keep-awake inhibitor is held.
    pub awake_held: bool,
    pub limits: PublicLimits,
    pub cert: Option<CertStatus>,
    pub net: Option<NetFacts>,
    pub dns: Option<DnsStatus>,
    pub light: Option<LightStats>,
    pub firewall: Option<FirewallInfo>,
    pub share: Option<ShareInfo>,
    /// The zebrad version wallets are told, for the consent screen.
    pub zebrad_version: Option<String>,
}

/// Every service's state: `get_public_status`. Empty while nothing is set up.
pub type PublicStatuses = Vec<PublicStatus>;

/// The user's input for a setup step: `answer_public_step`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum StepAnswer {
    /// The consent screen's tick, for the text version the screen showed.
    Consent { version: u32 },
    /// The name and who keeps it.
    Name { name: String, provider: DnsProvider },
    /// A provider token: into the keychain at once, never echoed or stored elsewhere.
    Token { token: String },
    /// The user reserved this computer's LAN address in the router.
    LanReserved,
    /// The user added the router forward.
    ForwardedByHand,
    /// The user went through the firewall step.
    FirewallDone,
    /// Keep this computer awake while serving.
    KeepAwake { on: bool },
    /// The upload budget.
    Upload { preset: UploadPreset },
    /// Open again when Zeal starts.
    ResumeOnLaunch { on: bool },
}

/// `open_public_service` refused: the gate that failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenError {
    pub check: Option<CheckId>,
    pub message: String,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// "Check my connection": the read-only network facts and a verdict, without setting
/// anything up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionReport {
    pub net: NetFacts,
    /// The checks that decide whether a light client server can work here.
    pub checks: Vec<CheckResult>,
    pub firewall: FirewallInfo,
}

/// What `forget_public_service` could not undo: the steps the user took by hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StillPublic {
    /// The router forward, external port to LAN address and port.
    pub forward: Option<(u16, Option<IpAddr>, u16)>,
    /// The commands that reverse the firewall step.
    pub firewall_remove: Vec<String>,
    /// The name, which stays in DNS history services and in certificate logs.
    pub name: Option<PublicHostname>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_are_public_ldh_names() {
        for (name, want) in [
            ("zec-alice.duckdns.org", "zec-alice.duckdns.org"),
            (" ZEC.Example.COM. ", "zec.example.com"),
            ("a.b", "a.b"),
            ("xn--bcher-kva.example", "xn--bcher-kva.example"),
        ] {
            assert_eq!(PublicHostname::parse(name).unwrap().as_str(), want);
        }
        for (name, err) in [
            ("", HostnameError::Empty),
            ("localhost", HostnameError::SingleLabel),
            ("203.0.113.10", HostnameError::Address),
            ("::1", HostnameError::Address),
            ("host.123", HostnameError::Address),
            ("-a.example.com", HostnameError::BadLabel),
            ("a-.example.com", HostnameError::BadLabel),
            ("a..example.com", HostnameError::BadLabel),
            ("a_b.example.com", HostnameError::BadLabel),
            ("zec.example.com:443", HostnameError::BadLabel),
            ("printer.local", HostnameError::LocalOnly),
            ("nas.home.arpa", HostnameError::LocalOnly),
            ("x.internal", HostnameError::LocalOnly),
            ("abc.onion", HostnameError::LocalOnly),
            ("app.localhost", HostnameError::LocalOnly),
        ] {
            assert_eq!(PublicHostname::parse(name), Err(err), "{name:?}");
        }
        let long_label = format!("{}.example.com", "a".repeat(64));
        assert_eq!(
            PublicHostname::parse(&long_label),
            Err(HostnameError::BadLabel)
        );
        let too_long = format!("{}.com", vec!["a".repeat(60); 5].join("."));
        assert_eq!(
            PublicHostname::parse(&too_long),
            Err(HostnameError::TooLong)
        );
    }

    #[test]
    fn hostnames_deserialize_only_when_valid() {
        let name: PublicHostname = serde_json::from_str("\"Zec.Example.com\"").unwrap();
        assert_eq!(name.as_str(), "zec.example.com");
        assert!(serde_json::from_str::<PublicHostname>("\"printer.local\"").is_err());
    }

    #[test]
    fn a_spec_without_newer_fields_loads_with_defaults() {
        let json = r#"{"service":"light_server","node":"zebrad-mainnet","network":"mainnet","switch_on":true}"#;
        let spec: PublicServiceSpec = serde_json::from_str(json).unwrap();
        assert_eq!(
            spec,
            PublicServiceSpec::new_light_server(
                NodeId::new("zebrad-mainnet").unwrap(),
                Network::Mainnet
            )
        );
    }

    #[test]
    fn consent_follows_the_text_version_and_the_name() {
        let mut spec = PublicServiceSpec::new_light_server(
            NodeId::new("zebrad-testnet").unwrap(),
            Network::Testnet,
        );
        assert!(!spec.consent_current());
        // Given before a name was chosen: it covers the first one.
        spec.consent = Some(Consent {
            version: CONSENT_VERSION,
            at: SystemTime::UNIX_EPOCH,
            hostname: String::new(),
        });
        assert!(spec.consent_current());
        spec.name = Some(PublicHostname::parse("zec.example.com").unwrap());
        assert!(spec.consent_current());
        spec.consent.as_mut().unwrap().hostname = "zec.example.com".into();
        assert!(spec.consent_current());
        spec.name = Some(PublicHostname::parse("other.example.com").unwrap());
        assert!(!spec.consent_current());
        spec.name = Some(PublicHostname::parse("zec.example.com").unwrap());
        spec.consent.as_mut().unwrap().version = CONSENT_VERSION - 1;
        assert!(!spec.consent_current());
    }

    #[test]
    fn upload_presets_round_trip() {
        for preset in UploadPreset::ALL {
            assert_eq!(UploadPreset::of(preset.kbit()), Some(preset));
        }
        assert_eq!(
            UploadPreset::of(PublicLimits::default().upload_kbit),
            Some(UploadPreset::Standard)
        );
        assert_eq!(UploadPreset::of(Some(1)), None);
    }

    #[test]
    fn endpoint_ports_differ_per_network() {
        assert_eq!(endpoint_port(Network::Mainnet), 9067);
        assert_eq!(endpoint_port(Network::Testnet), 19067);
    }
}
