export const EXAMPLE_LOGS = [
  `Thank you for running a mainnet zebrad 1.6.0 node!`,
  `You're helping to strengthen the network and contributing to a social good :)`,
  `2024-02-29T20:01:26.271158Z  INFO zebrad::components::tracing::component: started tracing component filter="info" TRACING_STATIC_MAX_LEVEL=LevelFilter::INFO LOG_STATIC_MAX_LEVEL=Info`,
  `2024-02-29T20:01:26.271180Z  INFO zebrad::components::tracing::component: set 'tracing.progress_bar ="summary"' in zebrad.toml to activate progress bars`,
  `2024-02-29T20:01:26.271182Z  INFO zebrad::application: Diagnostic Metadata:`,
  `version: 1.6.0`,
  `Zcash network: Mainnet`,
  `running state version: 25.3.0`,
  `initial disk state version: 25.3.0`,
  `features: default,getblocktemplate_rpcs,howudoin,indicatif,progress_bar,release_max_level_info,shielded_scan,zebra_scan`,
  `target triple: x86_64-unknown-linux-gnu`,
  `rust compiler: 1.76.0`,
  `rust release date: 2024-02-04`,
  `optimization level: 3`,
  `debug checks: false`,
  `2024-02-29T20:01:26.271188Z  INFO zebrad::application: loaded zebrad config config_path=Some("/home/ar/.config/zebrad.toml") config=ZebradConfig { consensus: Config { checkpoint_sync: true }, metrics: Config { endpoint_addr: None }, network: Config { listen_addr: 0.0.0.0:8233, network: Mainnet, initial_mainnet_peers: {"dnsseed.z.cash:8233", "dnsseed.str4d.xyz:8233", "mainnet.seeder.zfnd.org:8233", "mainnet.is.yolo.money:8233"}, initial_testnet_peers: {"dnsseed.testnet.z.cash:18233", "testnet.seeder.zfnd.org:18233", "testnet.is.yolo.money:18233"}, cache_dir: IsEnabled(true), peerset_initial_target_size: 25, crawl_new_peer_interval: 61s, max_connections_per_ip: 1 }, state: Config { cache_dir: "/home/ar/.cache/zebra", ephemeral: false, delete_old_database: true, debug_stop_at_height: None, debug_validity_check_interval: None }, tracing: Config { inner: InnerConfig { use_color: true, force_use_color: false, filter: None, buffer_limit: 128000, endpoint_addr: None, flamegraph: None, progress_bar: None, log_file: None, use_journald: false } }, sync: Config { download_concurrency_limit: 50, checkpoint_verify_concurrency_limit: 1000, full_verify_concurrency_limit: 20, parallel_cpu_threads: 0 }, mempool: Config { tx_cost_limit: 80000000, eviction_memory_time: 3600s, debug_enable_at_height: None }, rpc: Config { listen_addr: None, parallel_cpu_threads: 0, debug_force_finished_sync: false }, mining: Config { miner_address: None, extra_coinbase_data: None, debug_like_zcashd: true }, shielded_scan: Config { sapling_keys_to_scan: 0, db_config: Config { cache_dir: "/home/ar/.cache/zebra-scan", ephemeral: false, delete_old_database: true, debug_stop_at_height: None, debug_validity_check_interval: None } } }`,
  `2024-02-29T20:01:26.271231Z  INFO {net="Main"}: zebrad::application: initialized rayon thread pool for CPU-bound tasks num_threads=32`,
  `2024-02-29T20:01:26.272366Z  INFO {net="Main"}: zebrad::commands::start: Starting zebrad`,
  `2024-02-29T20:01:26.272400Z  INFO {net="Main"}: zebrad::commands::start: initializing node state`,
  `2024-02-29T20:01:26.275189Z  INFO {net="Main"}: zebrad::commands::start: opening database, this may take a few minutes`,
  `2024-02-29T20:01:26.275415Z  INFO zebra_state::service::finalized_state::disk_format::upgrade: trying to open current database format running_version=25.3.0`,
  `2024-02-29T20:01:26.275644Z  INFO zebra_state::service::finalized_state::disk_db: the open file limit is high enough for Zebra current_limit=1048576 min_limit=512 ideal_limit=1024`,
  `2024-02-29T20:01:26.271188Z  INFO zebrad::application: loaded zebrad config config_path=Some("/home/ar/.config/zebrad.toml") config=ZebradConfig { consensus: Config { checkpoint_sync: true }, metrics: Config { endpoint_addr: None }, network: Config { listen_addr: 0.0.0.0:8233, network: Mainnet, initial_mainnet_peers: {"dnsseed.z.cash:8233", "dnsseed.str4d.xyz:8233", "mainnet.seeder.zfnd.org:8233", "mainnet.is.yolo.money:8233"}, initial_testnet_peers: {"dnsseed.testnet.z.cash:18233", "testnet.seeder.zfnd.org:18233", "testnet.is.yolo.money:18233"}, cache_dir: IsEnabled(true), peerset_initial_target_size: 25, crawl_new_peer_interval: 61s, max_connections_per_ip: 1 }, state: Config { cache_dir: "/home/ar/.cache/zebra", ephemeral: false, delete_old_database: true, debug_stop_at_height: None, debug_validity_check_interval: None }, tracing: Config { inner: InnerConfig { use_color: true, force_use_color: false, filter: None, buffer_limit: 128000, endpoint_addr: None, flamegraph: None, progress_bar: None, log_file: None, use_journald: false } }, sync: Config { download_concurrency_limit: 50, checkpoint_verify_concurrency_limit: 1000, full_verify_concurrency_limit: 20, parallel_cpu_threads: 0 }, mempool: Config { tx_cost_limit: 80000000, eviction_memory_time: 3600s, debug_enable_at_height: None }, rpc: Config { listen_addr: None, parallel_cpu_threads: 0, debug_force_finished_sync: false }, mining: Config { miner_address: None, extra_coinbase_data: None, debug_like_zcashd: true }, shielded_scan: Config { sapling_keys_to_scan: 0, db_config: Config { cache_dir: "/home/ar/.cache/zebra-scan", ephemeral: false, delete_old_database: true, debug_stop_at_height: None, debug_validity_check_interval: None } } }`,
];

export const EXAMPLE_CONFIG_CONTENTS = `
# Default configuration for zebrad.

[consensus]
checkpoint_sync = true

[mempool]
eviction_memory_time = "1h"
tx_cost_limit = 80000000

[metrics]

[mining]
debug_like_zcashd = true

[network]
cache_dir = true
crawl_new_peer_interval = "1m 1s"
initial_mainnet_peers = [
    "dnsseed.z.cash:8233",
    "dnsseed.str4d.xyz:8233",
    "mainnet.seeder.zfnd.org:8233",
    "mainnet.is.yolo.money:8233",
]
initial_testnet_peers = [
    "dnsseed.testnet.z.cash:18233",
    "testnet.seeder.zfnd.org:18233",
    "testnet.is.yolo.money:18233",
]
listen_addr = "0.0.0.0:8233"
max_connections_per_ip = 1
network = "Mainnet"
peerset_initial_target_size = 25

[rpc]
debug_force_finished_sync = false
parallel_cpu_threads = 0

[state]
cache_dir = "/home/ar/.cache/zebra"
delete_old_database = true
ephemeral = false

[sync]
checkpoint_verify_concurrency_limit = 1000
download_concurrency_limit = 50
full_verify_concurrency_limit = 20
parallel_cpu_threads = 0

[tracing]
buffer_limit = 128000
force_use_color = false
use_color = true
use_journald = false
`;
