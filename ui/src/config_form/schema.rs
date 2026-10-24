//! The schema of `zebrad.toml` for the bundled zebrad (7.0.0, commit
//! `e8db87eb69254e0d4eedd058eadd48bdfa09065a`): every section and key its config structs accept,
//! with zebrad's default and a plain-words explanation taken from the doc comment at that commit.
//!
//! Every zebrad config struct is `deny_unknown_fields`, so only keys that exist at that commit
//! are listed. Left out on purpose:
//! - `zcashd_compat`: the launcher does not support zcashd compatibility mode.
//! - `network.testnet_parameters` and a `network.network` table: they define custom test
//!   networks, which the launcher does not support (it runs Mainnet or the default Testnet).
//! - `state.elasticsearch_url`, `state.elasticsearch_username`, `state.elasticsearch_password`:
//!   only exist when zebrad is built with the `elasticsearch` feature, which the bundled zebrad
//!   is not, so zebrad would reject them.
//! - `consensus.debug_skip_parameter_preload`: accepted only so old files still load; zebrad
//!   ignores it.
//! - Deserialization aliases (`sync.max_concurrent_block_requests`, `sync.lookahead_limit`,
//!   `network.new_peer_interval`): old spellings of keys that are listed under their current
//!   name. A file using the old spelling shows it as an unknown key, kept as written.
//!
//! Defaults match `crates/launcher-core/tests/fixtures/zebrad-7.0.0-generate.toml` (checked by a
//! test). `rpc.cookie_dir` and `state.cache_dir` default to a per-user path there, so they are
//! `None` here: zebrad uses its cache directory when they are unset, and the launcher writes
//! them on every start anyway.

use super::{Control, Field, Kind, Section};

/// A `u32`.
const U32: Kind = Kind::Integer {
    min: Some(0),
    max: Some(4_294_967_295),
};
/// A `usize` or `u64` count where 0 is allowed (TOML integers stop at `i64::MAX`).
const COUNT: Kind = Kind::Integer {
    min: Some(0),
    max: None,
};
/// A `usize` count that zebrad raises to at least 1.
const AT_LEAST_ONE: Kind = Kind::Integer {
    min: Some(1),
    max: None,
};
/// A percentage.
const PERCENT: Kind = Kind::Integer {
    min: Some(0),
    max: Some(100),
};
/// A listener the launcher keeps on loopback (`LOOPBACK_KEYS` in launcher-core).
const LOOPBACK: Kind = Kind::SocketAddr {
    loopback_only: true,
};
/// A peer-to-peer address that may be public.
const PUBLIC_ADDR: Kind = Kind::SocketAddr {
    loopback_only: false,
};

/// Sections left out of the schema on purpose (see the module docs): `parse_config` keeps their
/// keys as written without reporting them as unknown.
pub const SKIPPED_SECTIONS: &[&str] = &["zcashd_compat"];

const COLOUR_NOTE: &str = "The launcher turns colour off so the Logs page can parse the output.";
const OUTPUT_NOTE: &str =
    "The launcher keeps zebrad's output on the Logs page and in its own log file.";

/// The sections, in display order.
pub const SECTIONS: &[Section] = &[
    Section {
        key: "network",
        title: "Network",
        doc: "How the node finds, connects to and listens for other Zcash nodes.",
    },
    Section {
        key: "state",
        title: "Chain state",
        doc: "Where the node keeps the blockchain it has downloaded and how it looks after \
              that data.",
    },
    Section {
        key: "sync",
        title: "Sync",
        doc: "How many blocks the node downloads and verifies at once while catching up with \
              the chain.",
    },
    Section {
        key: "consensus",
        title: "Consensus",
        doc: "How strictly the node verifies the chain while it is syncing.",
    },
    Section {
        key: "mempool",
        title: "Mempool",
        doc: "Limits on the unconfirmed transactions the node keeps in memory.",
    },
    Section {
        key: "rpc",
        title: "RPC",
        doc: "The JSON-RPC interface that wallets, tools and the launcher use to talk to the \
              node.",
    },
    Section {
        key: "mining",
        title: "Mining",
        doc: "Block templates and rewards for mining through this node.",
    },
    Section {
        key: "metrics",
        title: "Metrics",
        doc: "The Prometheus metrics endpoint that the Grafana tab reads.",
    },
    Section {
        key: "tracing",
        title: "Logging",
        doc: "What the node logs and where the logs go.",
    },
    Section {
        key: "health",
        title: "Health",
        doc: "An HTTP endpoint that reports whether the node is healthy and ready.",
    },
    Section {
        key: "notify",
        title: "Notifications",
        doc: "A command the node runs whenever its best chain tip changes.",
    },
];

/// Every field, grouped by section in display order.
pub const FIELDS: &[Field] = &[
    // [network]
    Field {
        section: "network",
        key: "listen_addr",
        label: "Listen address",
        kind: PUBLIC_ADDR,
        default: Some("[::]:8233"),
        doc: "The address and port on which the node accepts connections from other nodes. \
              The default [::]:8233 listens on every IPv4 and IPv6 address of this machine; \
              Testnet nodes use port 18233. Change it only to listen on one address or on \
              another port.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "external_addr",
        label: "External address",
        kind: PUBLIC_ADDR,
        default: None,
        doc: "The address and port other nodes should use to reach this node when it differs \
              from the listen address. Set it when the node is behind a router, firewall or \
              NAT and you want to accept incoming connections. Unset by default: nothing is \
              advertised.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "network",
        label: "Network",
        kind: Kind::Choice(&["Mainnet", "Testnet"]),
        default: Some("Mainnet"),
        doc: "The Zcash network the node joins: Mainnet carries real ZEC, Testnet is for \
              testing with coins that have no value.",
        control: Control::Managed {
            note: "Set from the Network setting above.",
        },
    },
    Field {
        section: "network",
        key: "initial_mainnet_peers",
        label: "Initial Mainnet peers",
        kind: Kind::StringList,
        default: Some(
            "dnsseed.str4d.xyz:8233\n\
             dnsseed.z.cash:8233\n\
             mainnet.seeder.shieldedinfra.net:8233\n\
             mainnet.seeder.zfnd.org:8233\n\
             seeder.zec.rocks:8233",
        ),
        doc: "The host:port addresses the node contacts first to find other Mainnet peers, one \
              per line. The defaults are the public DNS seeders run by the Zcash community. \
              Add your own node here to peer with it right away.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "initial_testnet_peers",
        label: "Initial Testnet peers",
        kind: Kind::StringList,
        default: Some(
            "dnsseed.testnet.z.cash:18233\n\
             seeder.testnet.zec.rocks:18233\n\
             testnet.seeder.zfnd.org:18233",
        ),
        doc: "The host:port addresses the node contacts first to find other Testnet peers, one \
              per line. The defaults are the public Testnet DNS seeders.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "cache_dir",
        label: "Peer cache directory",
        kind: Kind::BoolOrPath,
        default: Some("true"),
        doc: "Where the node saves the addresses of recently useful peers between runs. true \
              uses zebrad's cache directory, false keeps no peer cache, and a path uses that \
              folder. The cache lets the node reconnect quickly even when the DNS seeders are \
              unavailable.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "peerset_initial_target_size",
        label: "Peer set target size",
        kind: COUNT,
        default: Some("25"),
        doc: "How many peer connections the node aims to keep. It also caps the total inbound \
              and outbound connections and the size of the peer cache. Lower it on a slow \
              connection or to use less bandwidth.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "crawl_new_peer_interval",
        label: "Peer crawl interval",
        kind: Kind::Duration,
        default: Some("1m 1s"),
        doc: "How often the node asks its peers for more peer addresses, written as a duration \
              such as 1m 1s. It also asks whenever it runs out of addresses to try.",
        control: Control::User,
    },
    Field {
        section: "network",
        key: "max_connections_per_ip",
        label: "Connections per IP",
        kind: AT_LEAST_ONE,
        default: Some("1"),
        doc: "How many connections the node keeps with the same IP address (or the same /64 \
              block for IPv6) before dropping extra ones. The default and minimum is 1. \
              Raising it weakens the node's protection against one operator running many \
              peers.",
        control: Control::User,
    },
    // [state]
    Field {
        section: "state",
        key: "cache_dir",
        label: "Data folder",
        kind: Kind::Path,
        default: None,
        doc: "The folder where the node stores the blockchain it has downloaded. Unset, zebrad \
              uses its own cache directory; the launcher always writes this key. Deleting the \
              folder makes the node sync again from the start.",
        control: Control::Managed {
            note: "Set from the data folder above.",
        },
    },
    Field {
        section: "state",
        key: "ephemeral",
        label: "Ephemeral database",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When true the node keeps the chain in a temporary folder that is deleted when it \
              exits normally, so every start syncs from the beginning. The data folder is \
              ignored while this is on. Only useful for testing.",
        control: Control::User,
    },
    Field {
        section: "state",
        key: "should_backup_non_finalized_state",
        label: "Back up recent blocks",
        kind: Kind::Bool,
        default: Some("true"),
        doc: "When true the node saves its most recent, not yet final blocks to disk every few \
              seconds, so a restart does not download them again. Turning it off makes the \
              node fetch those blocks from the network after every restart.",
        control: Control::User,
    },
    Field {
        section: "state",
        key: "delete_old_database",
        label: "Delete old databases",
        kind: Kind::Bool,
        default: Some("true"),
        doc: "When true the node removes database folders left behind by older zebrad versions \
              when it starts. Turn it off to keep them, for example to run two versions side \
              by side.",
        control: Control::User,
    },
    Field {
        section: "state",
        key: "debug_stop_at_height",
        label: "Stop at height",
        kind: U32,
        default: None,
        doc: "Stops the node once its final chain reaches this block height. Unset by default, \
              so the node keeps syncing. A testing option.",
        control: Control::User,
    },
    Field {
        section: "state",
        key: "debug_validity_check_interval",
        label: "Validity check interval",
        kind: Kind::Duration,
        default: None,
        doc: "How often the running node re-checks that its database format is valid, written \
              as a duration such as 1h. Unset by default: it checks only at startup and \
              shutdown. A testing option.",
        control: Control::User,
    },
    Field {
        section: "state",
        key: "debug_skip_non_finalized_state_backup_task",
        label: "Write recent block backups immediately",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When true the node writes its recent-block backup on every new block instead of \
              in the background every few seconds. Slower, and meant for tests that commit \
              blocks quickly.",
        control: Control::User,
    },
    // [sync]
    Field {
        section: "sync",
        key: "download_concurrency_limit",
        label: "Download concurrency limit",
        kind: AT_LEAST_ONE,
        default: Some("50"),
        doc: "How many block downloads run at the same time. The default is low to avoid \
              network contention; a fast connection may sync quicker with more. zebrad uses at \
              least 1.",
        control: Control::User,
    },
    Field {
        section: "sync",
        key: "checkpoint_verify_concurrency_limit",
        label: "Checkpoint verification limit",
        kind: AT_LEAST_ONE,
        default: Some("1000"),
        doc: "How many blocks wait in the checkpoint verifier at once. Higher values use more \
              memory but smooth over failed downloads; lower values save memory. zebrad uses \
              at least 400.",
        control: Control::User,
    },
    Field {
        section: "sync",
        key: "full_verify_concurrency_limit",
        label: "Full verification limit",
        kind: AT_LEAST_ONE,
        default: Some("20"),
        doc: "How many blocks the full verifier checks at the same time once the checkpoints \
              end. The default is low so that large blocks verify within their time limit; a \
              machine with many cores may go higher. zebrad uses at least 1.",
        control: Control::User,
    },
    Field {
        section: "sync",
        key: "parallel_cpu_threads",
        label: "Verification threads",
        kind: COUNT,
        default: Some("0"),
        doc: "How many threads verify signatures, proofs and other CPU-heavy work. 0 means \
              automatic: one thread per logical core.",
        control: Control::User,
    },
    // [consensus]
    Field {
        section: "consensus",
        key: "checkpoint_sync",
        label: "Checkpoint sync",
        kind: Kind::Bool,
        default: Some("true"),
        doc: "When true the node uses zebrad's built-in checkpoints to follow the main chain \
              while syncing, which protects against some chain attacks. Turning it off starts \
              full verification earlier and is slower and less secure; some checkpoints always \
              stay active.",
        control: Control::User,
    },
    // [mempool]
    Field {
        section: "mempool",
        key: "tx_cost_limit",
        label: "Transaction cost limit",
        kind: COUNT,
        default: Some("80000000"),
        doc: "The total size, in bytes, of unconfirmed transactions the node keeps. When the \
              limit is reached, transactions are evicted at random. The default of 80000000 \
              bytes follows ZIP-401.",
        control: Control::User,
    },
    Field {
        section: "mempool",
        key: "eviction_memory_time",
        label: "Eviction memory time",
        kind: Kind::Duration,
        default: Some("1h"),
        doc: "How long the node remembers that it evicted a transaction and refuses it again, \
              written as a duration such as 1h. The default follows ZIP-401.",
        control: Control::User,
    },
    Field {
        section: "mempool",
        key: "debug_enable_at_height",
        label: "Enable at height",
        kind: U32,
        default: None,
        doc: "Enables the mempool as soon as the chain reaches this block height, even while \
              the node is still syncing. Unset by default: the mempool opens when the node is \
              close to the chain tip. A testing option.",
        control: Control::User,
    },
    Field {
        section: "mempool",
        key: "max_datacarrier_bytes",
        label: "Max OP_RETURN size",
        kind: U32,
        default: Some("83"),
        doc: "The largest OP_RETURN data script, in bytes, that the node accepts in a standard \
              transaction, including the opcode and push overhead. The default of 83 matches \
              zcashd.",
        control: Control::User,
    },
    // [rpc]
    Field {
        section: "rpc",
        key: "listen_addr",
        label: "Listen address",
        kind: LOOPBACK,
        default: None,
        doc: "The address and port of the JSON-RPC server. Unset, the RPC is off; the launcher \
              sets it so that it can talk to the node. Anyone who can reach this port can read \
              the node's state and send transactions through it.",
        control: Control::Managed {
            note: "The launcher keeps the RPC on 127.0.0.1, port 8232 on Mainnet and 18232 on \
                   Testnet.",
        },
    },
    Field {
        section: "rpc",
        key: "indexer_listen_addr",
        label: "Indexer listen address",
        kind: LOOPBACK,
        default: None,
        doc: "The address and port of the indexer RPC, which only works when zebrad is built \
              with the indexer feature. Unset by default, so it is off. The launcher requires \
              a loopback address.",
        control: Control::User,
    },
    Field {
        section: "rpc",
        key: "lightwalletd_listen_addr",
        label: "Lightwalletd listen address",
        kind: LOOPBACK,
        default: None,
        doc: "The address and port of the experimental lightwalletd-compatible gRPC server, \
              which lets light wallets connect directly to the node. Unset by default, so it \
              is off. It has no encryption or authentication, so the launcher requires a \
              loopback address.",
        control: Control::User,
    },
    Field {
        section: "rpc",
        key: "parallel_cpu_threads",
        label: "RPC threads",
        kind: COUNT,
        default: Some("0"),
        doc: "Formerly the number of threads that handled RPC requests. zebrad ignores it now \
              and keeps the key only so that older files still load.",
        control: Control::User,
    },
    Field {
        section: "rpc",
        key: "debug_force_finished_sync",
        label: "Pretend sync is finished",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When true the RPC reports the node as fully synced whatever its real height. A \
              testing option.",
        control: Control::User,
    },
    Field {
        section: "rpc",
        key: "cookie_dir",
        label: "Cookie directory",
        kind: Kind::Path,
        default: None,
        doc: "The folder where the node writes the cookie file that RPC clients must present. \
              Unset, zebrad uses its cache directory; the launcher always writes this key.",
        control: Control::Managed {
            note: "Where the launcher reads the RPC cookie.",
        },
    },
    Field {
        section: "rpc",
        key: "enable_cookie_auth",
        label: "Cookie authentication",
        kind: Kind::Bool,
        default: Some("true"),
        doc: "When true RPC clients must send the cookie from the cookie directory with every \
              request, so only programs on this machine that can read the file get access.",
        control: Control::Managed {
            note: "Always on: the launcher authenticates to the RPC with the cookie.",
        },
    },
    Field {
        section: "rpc",
        key: "max_response_body_size",
        label: "Max response size",
        kind: U32,
        default: Some("52428800"),
        doc: "The largest RPC response the server sends, in bytes. The default is 50 MiB. Raise \
              it only if a client needs bigger responses, such as whole blocks with every \
              transaction.",
        control: Control::User,
    },
    // [mining]
    Field {
        section: "mining",
        key: "miner_address",
        label: "Miner address",
        kind: Kind::Text,
        default: None,
        doc: "The Zcash address that receives the block reward and fees in blocks built by the \
              getblocktemplate RPC. Unset by default; set it to mine through this node.",
        control: Control::User,
    },
    Field {
        section: "mining",
        key: "extra_coinbase_data",
        label: "Extra coinbase data",
        kind: Kind::Text,
        default: None,
        doc: "A short tag added to the coinbase input of every block this node builds, after \
              zebrad's own marker. Unset by default. Keep it short: the coinbase input has \
              under 100 bytes for the height, the marker and this tag, and zebrad refuses to \
              start when the tag does not fit.",
        control: Control::User,
    },
    Field {
        section: "mining",
        key: "miner_memo",
        label: "Miner memo",
        kind: Kind::Text,
        default: None,
        doc: "A memo of up to 512 bytes placed in the shielded output of the coinbase \
              transactions this node builds. Used only when the miner address has a shielded \
              part.",
        control: Control::User,
    },
    Field {
        section: "mining",
        key: "internal_miner",
        label: "Internal miner",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When true zebrad mines blocks itself, without a mining pool or solver. \
              Experimental and only supported on regtest, where proofs of work are not \
              checked. Off by default.",
        control: Control::User,
    },
    // [metrics]
    Field {
        section: "metrics",
        key: "endpoint_addr",
        label: "Metrics address",
        kind: LOOPBACK,
        default: None,
        doc: "The address and port where the node serves Prometheus metrics. Unset, metrics \
              are off; the launcher sets it so that the Grafana tab has data.",
        control: Control::Managed {
            note: "The launcher keeps metrics on 127.0.0.1:9999 for the Grafana tab.",
        },
    },
    // [tracing]
    Field {
        section: "tracing",
        key: "use_color",
        label: "Coloured output",
        kind: Kind::Bool,
        default: Some("true"),
        doc: "When true the node colours its log output when it writes to a terminal.",
        control: Control::Forced { note: COLOUR_NOTE },
    },
    Field {
        section: "tracing",
        key: "force_use_color",
        label: "Force coloured output",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When true the node colours its log output even when it is not writing to a \
              terminal.",
        control: Control::Forced { note: COLOUR_NOTE },
    },
    Field {
        section: "tracing",
        key: "filter",
        label: "Log filter",
        kind: Kind::Text,
        default: None,
        doc: "Which log messages the node writes, as a tracing filter such as debug or \
              warn,zebrad=info,zebra_network=debug. Unset, the node logs at the info level.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "buffer_limit",
        label: "Log buffer limit",
        kind: Kind::Integer {
            min: Some(100),
            max: None,
        },
        default: Some("128000"),
        doc: "How many log lines can wait in memory before the node drops lines it cannot write \
              out fast enough. zebrad uses at least 100.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "endpoint_addr",
        label: "Filter endpoint address",
        kind: LOOPBACK,
        default: None,
        doc: "The address and port of an HTTP endpoint for changing the log filter while the \
              node runs. Only works when zebrad is built with the filter-reload feature; unset \
              by default, so it is off. The launcher requires a loopback address because the \
              endpoint has no authentication.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "flamegraph",
        label: "Flamegraph file",
        kind: Kind::Path,
        default: None,
        doc: "Writes a flamegraph of the node's tracing spans to this path, with .folded and \
              .svg added, when the node exits. Only works when zebrad is built with the \
              flamegraph feature; unset by default.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "progress_bar",
        label: "Progress bars",
        kind: Kind::Choice(&["summary", "detailed"]),
        default: None,
        doc: "Shows progress bars for syncing, the mempool and peers in the terminal and moves \
              the logs to a file. Only works when zebrad is built with the progress-bar \
              feature.",
        control: Control::Forced { note: OUTPUT_NOTE },
    },
    Field {
        section: "tracing",
        key: "log_file",
        label: "Log file",
        kind: Kind::Path,
        default: None,
        doc: "Writes the node's logs to this file instead of the terminal.",
        control: Control::Forced { note: OUTPUT_NOTE },
    },
    Field {
        section: "tracing",
        key: "use_journald",
        label: "Log to journald",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When true the node also sends its logs to systemd-journald on Linux systems that \
              use systemd. Only works when zebrad is built with the journald feature.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "opentelemetry_endpoint",
        label: "OpenTelemetry endpoint",
        kind: Kind::Text,
        default: None,
        doc: "The OTLP HTTP URL, such as http://localhost:4318, that traces are exported to. \
              Only works when zebrad is built with the opentelemetry feature; unset by \
              default, so export is off.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "opentelemetry_service_name",
        label: "OpenTelemetry service name",
        kind: Kind::Text,
        default: None,
        doc: "The service name reported to the OpenTelemetry collector. Unset, zebrad reports \
              zebra.",
        control: Control::User,
    },
    Field {
        section: "tracing",
        key: "opentelemetry_sample_percent",
        label: "Trace sample percentage",
        kind: PERCENT,
        default: None,
        doc: "The percentage of traces exported to OpenTelemetry, from 0 to 100. Unset means \
              100; lower it on a busy node to reduce collector traffic.",
        control: Control::User,
    },
    // [health]
    Field {
        section: "health",
        key: "listen_addr",
        label: "Listen address",
        kind: LOOPBACK,
        default: None,
        doc: "The address and port of the health HTTP server with its /healthy and /ready \
              endpoints. Unset by default, so it is off. The launcher requires a loopback \
              address.",
        control: Control::User,
    },
    Field {
        section: "health",
        key: "min_connected_peers",
        label: "Minimum connected peers",
        kind: COUNT,
        default: Some("1"),
        doc: "How many recently active peers the node needs for /healthy to report it as \
              healthy.",
        control: Control::User,
    },
    Field {
        section: "health",
        key: "ready_max_blocks_behind",
        label: "Ready blocks behind",
        kind: Kind::Integer {
            min: None,
            max: None,
        },
        default: Some("2"),
        doc: "How many blocks behind the network tip the node may be while /ready still \
              reports it as ready.",
        control: Control::User,
    },
    Field {
        section: "health",
        key: "enforce_on_test_networks",
        label: "Enforce on test networks",
        kind: Kind::Bool,
        default: Some("false"),
        doc: "When false, /ready always succeeds on Testnet and regtest. Turn it on to apply \
              the readiness checks there too.",
        control: Control::User,
    },
    Field {
        section: "health",
        key: "ready_max_tip_age",
        label: "Ready max tip age",
        kind: Kind::Duration,
        default: Some("5m"),
        doc: "How old the latest block may be, written as a duration such as 5m, before /ready \
              reports the node as not ready.",
        control: Control::User,
    },
    // [notify]
    Field {
        section: "notify",
        key: "block_notify_command",
        label: "Block notify command",
        kind: Kind::Text,
        default: None,
        doc: "A shell command the node runs each time its best chain tip changes once it is \
              close to the network tip, like zcashd's -blocknotify. Every %s in the command \
              is replaced by the new block hash. Unset by default.",
        control: Control::User,
    },
];
