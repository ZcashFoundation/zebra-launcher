//! A small client for zebrad's cookie-authenticated JSON-RPC interface.
//!
//! zebrad writes `__cookie__:<secret>` to `<rpc.cookie_dir>/.cookie` on every
//! start and expects it back as HTTP Basic credentials. The cookie changes on
//! each start, so [`RpcClient`] reads the file before every call; a batch of
//! calls shares one read through an [`RpcSession`].
//!
//! Only the fields the launcher shows are decoded; zebrad may add others.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
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

    /// The number of peers in `getpeerinfo`.
    pub async fn get_peer_info(&self) -> Result<usize, RpcError> {
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

    /// The number of peers in `getpeerinfo`.
    pub async fn get_peer_info(&self) -> Result<usize, RpcError> {
        let peers: Vec<serde::de::IgnoredAny> = self.call("getpeerinfo", json!([])).await?;
        Ok(peers.len())
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
        assert_eq!(info, MempoolInfo { size: 3 });

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
    }

    #[test]
    fn info_helpers() {
        let mut info = Info {
            version: 7_000_050,
            build: "v7.0.0".into(),
            subversion: "/Zebra:7.0.0/".into(),
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
}
