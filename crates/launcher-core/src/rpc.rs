//! A small client for zebrad's cookie-authenticated JSON-RPC interface.
//!
//! zebrad writes `__cookie__:<secret>` to `<rpc.cookie_dir>/.cookie` on every
//! start and expects it back as HTTP Basic credentials. The cookie changes on
//! each start, so [`RpcClient`] reads the file before every call; a batch of
//! calls shares one read through an [`RpcSession`].
//!
//! Only the fields the launcher shows are decoded; zebrad may add others.
//! Beyond the handful of required fields every field is optional, so a field
//! a zebrad release renames or drops cannot fail the whole poll.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use launcher_types::PeerInfo;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

/// How long one call may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A call to zebrad failed.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The cookie file could not be read, usually because zebrad has not
    /// written it yet.
    #[error("could not read the RPC cookie {path}: {source}")]
    Cookie {
        /// The cookie file.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// The HTTP request failed: connection refused, timeout, and so on.
    #[error("RPC request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// zebrad answered with an HTTP error status.
    #[error("RPC request failed with HTTP status {0}")]
    Status(u16),
    /// zebrad returned a JSON-RPC error.
    #[error("RPC error {code}: {message}")]
    Rpc {
        /// The JSON-RPC error code, such as `-32601` for an unknown method.
        code: i64,
        /// zebrad's message.
        message: String,
    },
    /// The response was not the expected JSON.
    #[error("unexpected RPC response: {0}")]
    InvalidResponse(String),
}

/// The parts of `getblockchaininfo` the launcher uses.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct BlockchainInfo {
    /// The height of the best verified block.
    pub blocks: u64,
    /// zebrad's estimate of the network chain tip height.
    #[serde(rename = "estimatedheight")]
    pub estimated_height: u64,
    /// Sync progress from 0.0 to 1.0.
    #[serde(rename = "verificationprogress")]
    pub verification_progress: f64,
    /// `main`, `test` or `regtest`.
    pub chain: String,
    /// The hash of the best block, hex.
    #[serde(default, rename = "bestblockhash")]
    pub best_block_hash: Option<String>,
    /// The height of the best chain's headers.
    #[serde(default)]
    pub headers: Option<u64>,
    /// The network difficulty at the tip.
    #[serde(default)]
    pub difficulty: Option<f64>,
    /// zebrad's estimate of the chain state's size on disk, in bytes.
    #[serde(default)]
    pub size_on_disk: Option<u64>,
    /// The issued supply: the sum of the value pools.
    #[serde(default, rename = "chainSupply")]
    pub chain_supply: Option<PoolBalance>,
    /// The value pools and their balances.
    #[serde(default, rename = "valuePools")]
    pub value_pools: Vec<PoolBalance>,
    /// The network upgrades by consensus branch id (hex).
    #[serde(default)]
    pub upgrades: BTreeMap<String, UpgradeInfo>,
}

/// A value pool's balance: `chainSupply` and the `valuePools` entries.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PoolBalance {
    /// The pool: `transparent`, `sprout`, `sapling`, `orchard`, `lockbox` or
    /// `ironwood`; empty for the chain supply.
    #[serde(default)]
    pub id: String,
    /// The balance in zatoshis.
    #[serde(default, rename = "chainValueZat")]
    pub chain_value_zat: Option<u64>,
}

/// A network upgrade's activation, from `upgrades`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct UpgradeInfo {
    /// The upgrade, such as `Sapling` or `NU6`.
    #[serde(default)]
    pub name: String,
    /// Its activation height.
    #[serde(default, rename = "activationheight")]
    pub activation_height: u64,
    /// `active`, `pending` or `disabled`.
    #[serde(default)]
    pub status: String,
}

/// Zatoshis in one ZEC.
pub const ZATOSHIS_PER_ZEC: u64 = 100_000_000;

/// `zat` as ZEC. Exact for every amount zebrad can report: the maximum
/// supply needs 51 bits and an `f64` has 53.
pub fn zat_to_zec(zat: u64) -> f64 {
    zat as f64 / ZATOSHIS_PER_ZEC as f64
}

/// The name of the active network upgrade: among the entries with status
/// `active`, the one with the highest activation height.
pub fn active_upgrade(upgrades: &BTreeMap<String, UpgradeInfo>) -> Option<String> {
    upgrades
        .values()
        .filter(|upgrade| upgrade.status == "active")
        .max_by_key(|upgrade| upgrade.activation_height)
        .map(|upgrade| upgrade.name.clone())
}

/// The parts of `getinfo` the launcher uses.
///
/// Every field is optional, so a field zebrad renames or drops cannot fail
/// the whole poll; a missing field decodes as zero or empty.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Info {
    /// The numeric node version.
    #[serde(default)]
    pub version: u64,
    /// The release, such as `v7.0.0`.
    #[serde(default)]
    pub build: String,
    /// The P2P user agent, such as `/Zebra:7.0.0/`.
    #[serde(default)]
    pub subversion: String,
    /// The P2P protocol version.
    #[serde(default, rename = "protocolversion")]
    pub protocol_version: Option<u32>,
    /// Inbound and outbound peer connections.
    #[serde(default)]
    pub connections: u64,
    /// The last warning or error, or `no errors`.
    #[serde(default)]
    pub errors: String,
}

impl Info {
    /// The last warning or error, if zebrad reports one.
    pub fn last_error(&self) -> Option<&str> {
        let errors = self.errors.trim();
        (!errors.is_empty() && errors != "no errors").then_some(errors)
    }

    /// The release for display, such as `7.0.0`.
    pub fn release(&self) -> String {
        let build = self.build.trim().trim_start_matches('v');
        if !build.is_empty() {
            return build.to_string();
        }
        self.subversion
            .trim_matches('/')
            .trim_start_matches("Zebra:")
            .to_string()
    }
}

/// The parts of `getmempoolinfo` the launcher uses.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct MempoolInfo {
    /// Transactions in the mempool.
    pub size: u64,
    /// Their size in bytes.
    #[serde(default)]
    pub bytes: Option<u64>,
    /// The memory the mempool uses, in bytes.
    #[serde(default)]
    pub usage: Option<u64>,
}

/// One `getpeerinfo` entry, in zebrad's field names.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Peer {
    /// The peer's address, `ip:port`.
    #[serde(default)]
    pub addr: String,
    /// The peer connected to us (true) or we to it (false).
    #[serde(default)]
    pub inbound: bool,
    /// The user agent, such as `/Zebra:7.0.0/`; empty when unknown.
    #[serde(default)]
    pub subver: String,
    /// The negotiated protocol version; 0 when unknown.
    #[serde(default)]
    pub version: u32,
    /// The connection state, such as `connected`.
    #[serde(default, alias = "state")]
    pub connection_state: String,
    /// Seconds since the epoch of the last message received; 0 for never.
    #[serde(default)]
    pub lastrecv: u64,
    /// Seconds since the epoch of the last message sent; 0 for never.
    #[serde(default)]
    pub lastsend: u64,
    /// The round-trip ping time in seconds.
    #[serde(default)]
    pub pingtime: Option<f64>,
}

impl From<Peer> for PeerInfo {
    /// The wire form: empty strings and zeros become `None`.
    fn from(peer: Peer) -> Self {
        let last_seen = peer.lastrecv.max(peer.lastsend);
        PeerInfo {
            addr: peer.addr,
            inbound: peer.inbound,
            user_agent: (!peer.subver.is_empty()).then_some(peer.subver),
            protocol_version: (peer.version != 0).then_some(peer.version),
            state: (!peer.connection_state.is_empty()).then_some(peer.connection_state),
            last_seen: (last_seen != 0).then(|| UNIX_EPOCH + Duration::from_secs(last_seen)),
            ping_secs: peer.pingtime,
        }
    }
}

/// `getdeprecationinfo`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DeprecationInfo {
    /// Present on Mainnet only, where end of support is enforced.
    #[serde(default)]
    pub end_of_service: Option<EndOfService>,
}

/// When this zebrad release stops running.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct EndOfService {
    /// The last supported block height.
    pub block_height: u64,
    /// The estimated time of the halt, in seconds since the Unix epoch.
    pub estimated_time: i64,
}

/// A JSON-RPC client for one zebrad.
#[derive(Clone, Debug)]
pub struct RpcClient {
    http: reqwest::Client,
    url: String,
    cookie_file: PathBuf,
}

impl RpcClient {
    /// A client for the zebrad listening on `addr` that authenticates with the
    /// cookie in `cookie_file`.
    pub fn new(addr: SocketAddr, cookie_file: PathBuf) -> Result<Self, RpcError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // zebrad is always on loopback; never route through a system proxy.
            .no_proxy()
            .build()?;
        Ok(Self {
            http,
            url: format!("http://{addr}/"),
            cookie_file,
        })
    }

    /// The cookie file this client reads.
    pub fn cookie_file(&self) -> &Path {
        &self.cookie_file
    }

    /// Reads the cookie once and returns a session that reuses it, for a batch
    /// of calls.
    pub async fn session(&self) -> Result<RpcSession<'_>, RpcError> {
        let cookie = tokio::fs::read_to_string(&self.cookie_file)
            .await
            .map_err(|source| RpcError::Cookie {
                path: self.cookie_file.clone(),
                source,
            })?;
        Ok(RpcSession {
            client: self,
            authorization: format!("Basic {}", STANDARD.encode(cookie.trim())),
        })
    }

    /// `getblockchaininfo`.
    pub async fn get_blockchain_info(&self) -> Result<BlockchainInfo, RpcError> {
        self.session().await?.get_blockchain_info().await
    }

    /// `getinfo`.
    pub async fn get_info(&self) -> Result<Info, RpcError> {
        self.session().await?.get_info().await
    }

    /// `getmempoolinfo`.
    pub async fn get_mempool_info(&self) -> Result<MempoolInfo, RpcError> {
        self.session().await?.get_mempool_info().await
    }

    /// `getpeerinfo`.
    pub async fn get_peer_info(&self) -> Result<Vec<Peer>, RpcError> {
        self.session().await?.get_peer_info().await
    }

    /// `getdeprecationinfo`.
    pub async fn get_deprecation_info(&self) -> Result<DeprecationInfo, RpcError> {
        self.session().await?.get_deprecation_info().await
    }

    /// Calls `method` with `params` and decodes the result.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, RpcError> {
        self.session().await?.call(method, params).await
    }
}

/// An [`RpcClient`] with the cookie read once, from [`RpcClient::session`].
///
/// zebrad writes a new cookie on every start, so keep a session only for one
/// batch of calls.
#[derive(Clone)]
pub struct RpcSession<'a> {
    client: &'a RpcClient,
    authorization: String,
}

impl std::fmt::Debug for RpcSession<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credentials are left out on purpose.
        f.debug_struct("RpcSession")
            .field("client", self.client)
            .finish_non_exhaustive()
    }
}

impl RpcSession<'_> {
    /// `getblockchaininfo`.
    pub async fn get_blockchain_info(&self) -> Result<BlockchainInfo, RpcError> {
        self.call("getblockchaininfo", json!([])).await
    }

    /// `getinfo`.
    pub async fn get_info(&self) -> Result<Info, RpcError> {
        self.call("getinfo", json!([])).await
    }

    /// `getmempoolinfo`.
    pub async fn get_mempool_info(&self) -> Result<MempoolInfo, RpcError> {
        self.call("getmempoolinfo", json!([])).await
    }

    /// `getpeerinfo`: one entry per peer; the count is the length.
    pub async fn get_peer_info(&self) -> Result<Vec<Peer>, RpcError> {
        self.call("getpeerinfo", json!([])).await
    }

    /// `getdeprecationinfo`.
    pub async fn get_deprecation_info(&self) -> Result<DeprecationInfo, RpcError> {
        self.call("getdeprecationinfo", json!([])).await
    }

    /// Calls `method` with `params` and decodes the result.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, RpcError> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let response = self
            .client
            .http
            .post(&self.client.url)
            .header(reqwest::header::AUTHORIZATION, &self.authorization)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(RpcError::Status(status.as_u16()));
        }
        let bytes = response.bytes().await?;
        decode(&bytes)
    }
}

/// Decodes a JSON-RPC response body.
fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, RpcError> {
    #[derive(Deserialize)]
    struct Response {
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<ErrorObject>,
    }
    #[derive(Deserialize)]
    struct ErrorObject {
        code: i64,
        #[serde(default)]
        message: String,
    }

    let response: Response =
        serde_json::from_slice(bytes).map_err(|err| RpcError::InvalidResponse(err.to_string()))?;
    if let Some(ErrorObject { code, message }) = response.error {
        return Err(RpcError::Rpc { code, message });
    }
    let result = response
        .result
        .ok_or_else(|| RpcError::InvalidResponse("no result".to_string()))?;
    serde_json::from_value(result).map_err(|err| RpcError::InvalidResponse(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_results_and_errors() {
        let info: MempoolInfo =
            decode(br#"{"jsonrpc":"2.0","id":1,"result":{"size":3,"bytes":10}}"#).expect("ok");
        assert_eq!(
            info,
            MempoolInfo {
                size: 3,
                bytes: Some(10),
                usage: None
            }
        );

        let err = decode::<MempoolInfo>(
            br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#,
        )
        .expect_err("error");
        assert!(matches!(err, RpcError::Rpc { code: -32601, .. }), "{err:?}");

        assert!(matches!(
            decode::<MempoolInfo>(b"not json"),
            Err(RpcError::InvalidResponse(_))
        ));
        assert!(matches!(
            decode::<MempoolInfo>(br#"{"result":{"bytes":1}}"#),
            Err(RpcError::InvalidResponse(_))
        ));
    }

    #[test]
    fn info_tolerates_missing_fields() {
        let info: Info =
            decode(br#"{"jsonrpc":"2.0","id":1,"result":{"build":"v7.1.0"}}"#).expect("ok");
        assert_eq!(info.release(), "7.1.0");
        assert_eq!(info.last_error(), None);
        assert_eq!(info.connections, 0);
        assert_eq!(info.protocol_version, None);
    }

    #[test]
    fn info_helpers() {
        let mut info = Info {
            version: 7_000_050,
            build: "v7.0.0".into(),
            subversion: "/Zebra:7.0.0/".into(),
            protocol_version: Some(170_140),
            connections: 8,
            errors: "no errors".into(),
        };
        assert_eq!(info.last_error(), None);
        assert_eq!(info.release(), "7.0.0");
        info.build.clear();
        info.errors = "disk almost full".into();
        assert_eq!(info.release(), "7.0.0");
        assert_eq!(info.last_error(), Some("disk almost full"));
    }

    #[test]
    fn blockchain_info_with_and_without_the_optional_fields() {
        // An older zebrad: the four required fields only.
        let bare: BlockchainInfo = decode(
            br#"{"result":{"chain":"test","blocks":5,"estimatedheight":9,"verificationprogress":0.5}}"#,
        )
        .expect("ok");
        assert_eq!(bare.blocks, 5);
        assert_eq!(bare.best_block_hash, None);
        assert_eq!(bare.chain_supply, None);
        assert!(bare.value_pools.is_empty());
        assert!(bare.upgrades.is_empty());
        assert_eq!(active_upgrade(&bare.upgrades), None);

        let full: BlockchainInfo = decode(
            br#"{"result":{"chain":"main","blocks":5,"headers":5,"difficulty":2.5,
                "verificationprogress":1.0,"estimatedheight":5,"size_on_disk":4096,
                "bestblockhash":"00aa",
                "chainSupply":{"chainValue":1.5,"chainValueZat":150000000,"monitored":true},
                "valuePools":[{"id":"transparent","chainValue":1.0,"chainValueZat":100000000,"monitored":true},
                              {"id":"sapling","chainValue":0.5,"chainValueZat":50000000,"monitored":true}],
                "upgrades":{"76b809bb":{"name":"Sapling","activationheight":419200,"status":"active"},
                            "c8e71055":{"name":"NU6","activationheight":2726400,"status":"active"},
                            "4dec4df0":{"name":"NU6.1","activationheight":3146400,"status":"pending"}},
                "consensus":{"chaintip":"c8e71055","nextblock":"c8e71055"}}}"#,
        )
        .expect("ok");
        assert_eq!(full.best_block_hash.as_deref(), Some("00aa"));
        assert_eq!(full.headers, Some(5));
        assert_eq!(full.difficulty, Some(2.5));
        assert_eq!(full.size_on_disk, Some(4096));
        assert_eq!(
            full.chain_supply,
            Some(PoolBalance {
                id: String::new(),
                chain_value_zat: Some(150_000_000)
            })
        );
        assert_eq!(full.value_pools.len(), 2);
        assert_eq!(full.value_pools[1].id, "sapling");
        assert_eq!(full.value_pools[1].chain_value_zat, Some(50_000_000));
        assert_eq!(active_upgrade(&full.upgrades).as_deref(), Some("NU6"));
    }

    #[test]
    fn the_active_upgrade_is_the_highest_active_one() {
        fn upgrade(name: &str, height: u64, status: &str) -> UpgradeInfo {
            UpgradeInfo {
                name: name.into(),
                activation_height: height,
                status: status.into(),
            }
        }
        let mut upgrades = BTreeMap::new();
        assert_eq!(active_upgrade(&upgrades), None);
        // Map order (by branch id) does not matter; a pending upgrade at a
        // greater height is not active yet.
        upgrades.insert("zz".to_string(), upgrade("NU6.1", 3_146_400, "pending"));
        upgrades.insert("aa".to_string(), upgrade("NU6", 2_726_400, "active"));
        upgrades.insert("mm".to_string(), upgrade("Sapling", 419_200, "active"));
        upgrades.insert("bb".to_string(), upgrade("Overwinter", 347_500, "active"));
        assert_eq!(active_upgrade(&upgrades).as_deref(), Some("NU6"));
        upgrades.insert("cc".to_string(), upgrade("Nowhere", 9_000_000, "disabled"));
        assert_eq!(active_upgrade(&upgrades).as_deref(), Some("NU6"));
        upgrades.clear();
        upgrades.insert("zz".to_string(), upgrade("NU6.1", 3_146_400, "pending"));
        assert_eq!(active_upgrade(&upgrades), None);
    }

    #[test]
    fn zatoshis_to_zec() {
        assert_eq!(zat_to_zec(0), 0.0);
        assert_eq!(zat_to_zec(1), 0.000_000_01);
        assert_eq!(zat_to_zec(ZATOSHIS_PER_ZEC), 1.0);
        assert_eq!(zat_to_zec(1_593_750_000_000_000), 15_937_500.0);
        // MAX_MONEY, 21 million ZEC, is exact.
        assert_eq!(zat_to_zec(2_100_000_000_000_000), 21_000_000.0);
    }

    #[test]
    fn peers_decode_in_zebrad_shape_and_convert() {
        let peers: Vec<Peer> = decode(
            br#"{"result":[
                {"addr":"192.0.2.1:8233","services":"0000000000000001","lastrecv":1800000000,
                 "inbound":true,"banscore":0,"subver":"/Zebra:7.0.0/","version":170140,
                 "connection_state":"connected","pingtime":0.012,"pingwait":0.0},
                {"addr":"192.0.2.2:8233","inbound":false,"lastsend":1800000010,"state":"failed"},
                {}
            ]}"#,
        )
        .expect("ok");
        assert_eq!(peers.len(), 3);
        let infos: Vec<PeerInfo> = peers.into_iter().map(PeerInfo::from).collect();
        assert_eq!(
            infos[0],
            PeerInfo {
                addr: "192.0.2.1:8233".into(),
                inbound: true,
                user_agent: Some("/Zebra:7.0.0/".into()),
                protocol_version: Some(170_140),
                state: Some("connected".into()),
                last_seen: Some(UNIX_EPOCH + Duration::from_secs(1_800_000_000)),
                ping_secs: Some(0.012),
            }
        );
        // `state` is accepted for `connection_state`; `lastsend` counts as seen.
        assert_eq!(infos[1].state.as_deref(), Some("failed"));
        assert_eq!(
            infos[1].last_seen,
            Some(UNIX_EPOCH + Duration::from_secs(1_800_000_010))
        );
        assert_eq!(infos[1].user_agent, None);
        assert_eq!(infos[1].protocol_version, None);
        assert_eq!(infos[1].ping_secs, None);
        // Nothing known: nothing invented.
        assert_eq!(infos[2].addr, "");
        assert!(!infos[2].inbound);
        assert_eq!(infos[2].last_seen, None);
        assert_eq!(infos[2].state, None);
    }
}
