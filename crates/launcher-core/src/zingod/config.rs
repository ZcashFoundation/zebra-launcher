//! The `zingod.toml` Zeal writes before every start (docs/PUBLIC-SERVICES.md, "The managed
//! zingod.toml").
//!
//! Every table of zingod's configuration refuses unknown keys, so Zeal writes only keys the
//! pinned version has, sets every path, and writes the whole file each time. The tests check
//! the keys against `generate-config` output captured from the pin
//! (`tests/fixtures/zingod-1.1.0-generate.toml`), plus [`UNPRINTED_KEYS`].

use std::{
    fmt::Write as _,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use launcher_types::Network;

use crate::fsutil;

/// The indexes zingod builds, each in `<index dir>/indexes/<name>`. `utxo_set` goes beside
/// `compact_block` by itself.
pub const INDEXES: [&str; 5] = [
    "compact_block",
    "block_hash",
    "tree_state",
    "transparent_address",
    "txid_location",
];

/// Keys the pinned zingod reads but `generate-config` doesn't print, because they default to
/// unset. Check the list again at each bump (`packages/zingod/src/config.rs`).
pub const UNPRINTED_KEYS: [&str; 1] = ["trusted_validators.cookie_path"];

/// zingod's limit on connections. Every front-door client is 127.0.0.1 to zingod, so the
/// per-address limit is the total; zingod needs this plus 1,024 open files.
pub const MAX_CONNECTIONS: u32 = 256;

/// Streams a connection may open: zingod's default is 100.
pub const MAX_STREAMS_PER_CONNECTION: u32 = 16;

/// The network zingod is told it serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZingodNetwork {
    Mainnet,
    Testnet,
    /// For tests only. Only here may `finalised_depth` go below zebrad's reorg bound of 1,000;
    /// `None` keeps zingod's default.
    Regtest {
        finalised_depth: Option<u32>,
    },
}

impl ZingodNetwork {
    /// The network of a node Zeal runs.
    pub fn of(network: Network) -> Self {
        match network {
            Network::Mainnet => ZingodNetwork::Mainnet,
            Network::Testnet => ZingodNetwork::Testnet,
        }
    }

    /// zingod's lowercase name: it refuses `Testnet`.
    pub fn name(self) -> &'static str {
        match self {
            ZingodNetwork::Mainnet => "mainnet",
            ZingodNetwork::Testnet => "testnet",
            ZingodNetwork::Regtest { .. } => "regtest",
        }
    }
}

/// What Zeal writes into `zingod.toml`. zingod's network, its validator and that validator's
/// cookie come from the same node, so zingod never reads another network's zebrad.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZingodConfig {
    pub network: ZingodNetwork,
    /// The RPC address zebrad logged for this run.
    pub validator: SocketAddr,
    /// zebrad's RPC cookie, `NodePaths::rpc_cookie_file()`. Always written: without it a
    /// loopback validator gets zingod's own lookup, which finds nothing under Zeal's
    /// `XDG_CACHE_HOME`, and zingod would send no credentials and name no cause.
    pub cookie: PathBuf,
    /// Where zingod's gRPC listens: loopback, so with no TLS key it serves plaintext h2c to the
    /// front door.
    pub listen: SocketAddr,
    /// `<the node's data folder>/zingod/<network>`: the index, zingod's state and its home.
    pub index_dir: PathBuf,
}

/// The file can't be written as asked.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{} is not an absolute path", .0.display())]
    Relative(PathBuf),
    #[error("{} is not valid UTF-8", .0.display())]
    NotUtf8(PathBuf),
    #[error("zingod's listener {0} is not on loopback")]
    NotLoopback(SocketAddr),
}

impl From<ConfigError> for io::Error {
    fn from(err: ConfigError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}

impl ZingodConfig {
    /// zingod's hold and overload markers, which Zeal doesn't use.
    pub fn state_dir(&self) -> PathBuf {
        self.index_dir.join("state")
    }

    /// The indexes themselves.
    pub fn indexes_dir(&self) -> PathBuf {
        self.index_dir.join("indexes")
    }

    /// zingod's `HOME` and XDG folders: with every path set it writes nothing there.
    pub fn home_dir(&self) -> PathBuf {
        self.index_dir.join("home")
    }

    /// The file's text. Refuses a relative or non-UTF-8 path and a listener off loopback,
    /// where zingod would refuse plaintext.
    pub fn render(&self) -> Result<String, ConfigError> {
        if !self.listen.ip().is_loopback() {
            return Err(ConfigError::NotLoopback(self.listen));
        }
        let path = |path: &Path| -> Result<String, ConfigError> {
            if !path.is_absolute() {
                return Err(ConfigError::Relative(path.to_path_buf()));
            }
            path.to_str()
                .map(toml_string)
                .ok_or_else(|| ConfigError::NotUtf8(path.to_path_buf()))
        };
        let mut out = String::new();
        let mut line = |text: &str| {
            out.push_str(text);
            out.push('\n');
        };
        line("# Managed by Zeal: rewritten before every start.");
        line(&format!("network = {}", toml_string(self.network.name())));
        line("validator_client = \"legacy_rpc\"");
        line("");
        line("[[trusted_validators]]");
        line(&format!(
            "jsonrpc_address = {}",
            toml_string(&self.validator.to_string())
        ));
        line(&format!("cookie_path = {}", path(&self.cookie)?));
        line("");
        line("[serve]");
        line(&format!(
            "grpc_listen_address = {}",
            toml_string(&self.listen.to_string())
        ));
        line(&format!("state_dir = {}", path(&self.state_dir())?));
        line("");
        line("[grpc]");
        line(&format!("max_connections = {MAX_CONNECTIONS}"));
        line(&format!("max_connections_per_ip = {MAX_CONNECTIONS}"));
        line(&format!(
            "max_streams_per_connection = {MAX_STREAMS_PER_CONNECTION}"
        ));
        line("");
        line("# Off: zingod would read the pressure of a cgroup it may share with zebrad.");
        line("[grpc.overload]");
        line("cpu_pressure_percent = 0");
        line("io_pressure_percent = 0");
        line("");
        line("[p2p]");
        line("enabled = false");
        if let ZingodNetwork::Regtest {
            finalised_depth: Some(depth),
        } = self.network
        {
            line("");
            line("[sync]");
            line(&format!("finalised_depth = {depth}"));
        }
        for index in INDEXES {
            line("");
            line(&format!("[index.{index}]"));
            line(&format!(
                "path = {}",
                path(&self.indexes_dir().join(index))?
            ));
        }
        Ok(out)
    }

    /// Writes the file at `path` atomically, creating its folder.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let text = self.render()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        fsutil::write_atomic(path, text.as_bytes())
    }

    /// Creates the index folder and zingod's home. zingod creates the rest.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        std::fs::create_dir_all(self.home_dir())
    }
}

/// `text` as a TOML basic string.
fn toml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use toml_edit::{DocumentMut, Item};

    use super::*;

    const GENERATED: &str = include_str!("../../tests/fixtures/zingod-1.1.0-generate.toml");

    fn config(network: ZingodNetwork, rpc: u16, port: u16, name: &str) -> ZingodConfig {
        ZingodConfig {
            network,
            validator: SocketAddr::from(([127, 0, 0, 1], rpc)),
            cookie: PathBuf::from("/zeal/nodes/zebrad/rpc/.cookie"),
            listen: SocketAddr::from(([127, 0, 0, 1], port)),
            index_dir: PathBuf::from(format!("/zeal/nodes/zebrad/zingod/{name}")),
        }
    }

    fn mainnet() -> ZingodConfig {
        config(ZingodNetwork::Mainnet, 8232, 8137, "mainnet")
    }

    fn testnet() -> ZingodConfig {
        config(ZingodNetwork::Testnet, 18232, 18137, "testnet")
    }

    /// Every key as a dotted path: `trusted_validators.cookie_path`, `index.tree_state.path`.
    fn keys(text: &str) -> BTreeSet<String> {
        fn walk(prefix: &str, item: &Item, out: &mut BTreeSet<String>) {
            let join = |key: &str| {
                if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                }
            };
            match item {
                Item::Table(table) => {
                    for (key, item) in table.iter() {
                        walk(&join(key), item, out);
                    }
                }
                Item::ArrayOfTables(tables) => {
                    for table in tables.iter() {
                        for (key, item) in table.iter() {
                            walk(&join(key), item, out);
                        }
                    }
                }
                Item::Value(_) => {
                    out.insert(prefix.to_string());
                }
                Item::None => {}
            }
        }
        let doc: DocumentMut = text.parse().expect("valid TOML");
        let mut out = BTreeSet::new();
        walk("", doc.as_item(), &mut out);
        out
    }

    /// The paths are Unix ones: zingod runs on Linux only.
    #[cfg(unix)]
    #[test]
    fn the_files_match_their_golden_copies() {
        for (config, golden) in [
            (mainnet(), include_str!("testdata/mainnet.toml")),
            (testnet(), include_str!("testdata/testnet.toml")),
        ] {
            assert_eq!(config.render().unwrap(), golden, "{:?}", config.network);
        }
    }

    #[cfg(unix)]
    #[test]
    fn every_key_is_one_the_pinned_zingod_has() {
        let known = keys(GENERATED);
        assert!(known.contains("index.compact_block.path"), "{known:?}");
        for config in [
            mainnet(),
            testnet(),
            config(
                ZingodNetwork::Regtest {
                    finalised_depth: Some(10),
                },
                1,
                2,
                "regtest",
            ),
        ] {
            for key in keys(&config.render().unwrap()) {
                assert!(
                    known.contains(&key) || UNPRINTED_KEYS.contains(&key.as_str()),
                    "{key} is not in zingod 1.1.0's generate-config output"
                );
            }
        }
        for key in UNPRINTED_KEYS {
            assert!(
                !known.contains(key),
                "{key} is printed now: take it off UNPRINTED_KEYS"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_cookie_is_always_the_nodes_and_every_path_absolute() {
        for config in [mainnet(), testnet()] {
            let doc: DocumentMut = config.render().unwrap().parse().unwrap();
            let validators = doc["trusted_validators"]
                .as_array_of_tables()
                .expect("[[trusted_validators]]");
            assert_eq!(validators.len(), 1);
            let validator = validators.get(0).unwrap();
            assert_eq!(
                validator["cookie_path"].as_str(),
                Some("/zeal/nodes/zebrad/rpc/.cookie")
            );
            let mut paths = vec![
                validator["cookie_path"].as_str().unwrap().to_string(),
                doc["serve"]["state_dir"].as_str().unwrap().to_string(),
            ];
            for index in INDEXES {
                paths.push(doc["index"][index]["path"].as_str().unwrap().to_string());
            }
            assert_eq!(paths.len(), 7);
            for path in paths {
                assert!(Path::new(&path).is_absolute(), "{path}");
            }
        }
        let mut relative = testnet();
        relative.index_dir = PathBuf::from("zingod/testnet");
        assert!(matches!(relative.render(), Err(ConfigError::Relative(_))));
        let mut relative = testnet();
        relative.cookie = PathBuf::from(".cookie");
        assert!(matches!(relative.render(), Err(ConfigError::Relative(_))));
        let mut open = testnet();
        open.listen = SocketAddr::from(([0, 0, 0, 0], 18137));
        assert!(matches!(open.render(), Err(ConfigError::NotLoopback(_))));
    }

    #[cfg(unix)]
    #[test]
    fn regtest_values_stay_on_regtest() {
        for config in [mainnet(), testnet()] {
            let text = config.render().unwrap();
            let doc: DocumentMut = text.parse().unwrap();
            assert!(doc.get("sync").is_none(), "{text}");
            assert!(!text.contains("finalised_depth"), "{text}");
            assert!(!text.contains("regtest"), "{text}");
        }
        let regtest = config(
            ZingodNetwork::Regtest {
                finalised_depth: Some(10),
            },
            1,
            2,
            "regtest",
        );
        let doc: DocumentMut = regtest.render().unwrap().parse().unwrap();
        assert_eq!(doc["network"].as_str(), Some("regtest"));
        assert_eq!(doc["sync"]["finalised_depth"].as_integer(), Some(10));
        let default_depth = config(
            ZingodNetwork::Regtest {
                finalised_depth: None,
            },
            1,
            2,
            "regtest",
        );
        assert!(!default_depth.render().unwrap().contains("[sync]"));
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(toml_string(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(toml_string("x\ny\u{1}"), "\"x\\ny\\u0001\"");
        let doc: DocumentMut = format!("k = {}", toml_string("/a \"b\"\\c"))
            .parse()
            .unwrap();
        assert_eq!(doc["k"].as_str(), Some("/a \"b\"\\c"));
    }

    #[test]
    fn the_file_is_written_whole() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = testnet();
        config.index_dir = dir.path().join("zingod");
        config.cookie = dir.path().join("rpc/.cookie");
        let path = dir.path().join("public").join("zingod-testnet.toml");
        config.write(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            config.render().unwrap()
        );
        config.ensure_dirs().unwrap();
        assert!(config.home_dir().is_dir());
    }

    #[test]
    fn networks_have_zingods_names() {
        assert_eq!(ZingodNetwork::of(Network::Mainnet).name(), "mainnet");
        assert_eq!(ZingodNetwork::of(Network::Testnet).name(), "testnet");
        assert_eq!(
            ZingodNetwork::Regtest {
                finalised_depth: None
            }
            .name(),
            "regtest"
        );
    }
}
