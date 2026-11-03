//! Where the explorer reads the chain: a closed set of read calls, so nothing the UI sends
//! names an RPC method or its parameters, and the source that answers them over zebrad's RPC.

use std::{future::Future, net::SocketAddr, path::PathBuf, sync::Arc};

use explorer_model::shapes::{BestBlock, BlockV1, Header, Subsidy, Tx};
use launcher_types::explorer::{BlockHash, BlockId, TxId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::rpc::{OwnedSession, REQUEST_TIMEOUT, RpcClient, RpcError};

/// One read of the chain. Every call the explorer makes is one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReadCall {
    /// `getbestblockheightandhash`.
    Best,
    /// `getblock <id> 1`.
    Block(BlockId),
    /// `getblockhash <height>`.
    BlockHash(u32),
    /// `getblockheader <id> true`.
    Header(BlockId),
    /// `getrawtransaction <txid> 1`.
    Tx(TxId),
    /// `getblocksubsidy <height>`.
    Subsidy(u32),
}

impl ReadCall {
    /// The RPC method.
    pub(crate) fn method(self) -> &'static str {
        match self {
            ReadCall::Best => "getbestblockheightandhash",
            ReadCall::Block(_) => "getblock",
            ReadCall::BlockHash(_) => "getblockhash",
            ReadCall::Header(_) => "getblockheader",
            ReadCall::Tx(_) => "getrawtransaction",
            ReadCall::Subsidy(_) => "getblocksubsidy",
        }
    }

    /// The RPC parameters, as zebrad takes them: heights and hashes for `getblock` and
    /// `getblockheader` are strings.
    pub(crate) fn params(self) -> Value {
        match self {
            ReadCall::Best => json!([]),
            ReadCall::Block(id) => json!([id.to_string(), 1]),
            ReadCall::BlockHash(height) => json!([height]),
            ReadCall::Header(id) => json!([id.to_string(), true]),
            ReadCall::Tx(txid) => json!([txid.to_hex(), 1]),
            ReadCall::Subsidy(height) => json!([height]),
        }
    }
}

/// The answer to a [`ReadCall`]. Large answers are shared, not copied.
#[derive(Clone, Debug)]
pub(crate) enum ReadResult {
    Best(BestBlock),
    Block(Arc<BlockV1>),
    BlockHash(BlockHash),
    Header(Header),
    Tx(Arc<Tx>),
    Subsidy(Arc<Subsidy>),
}

/// Why a read failed. Messages never carry what was asked for.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SourceError {
    /// The node does not have it (zebrad's -5 and -8).
    #[error("not found")]
    NotFound,
    /// Any other JSON-RPC error.
    #[error("RPC error {code}")]
    Rpc { code: i64 },
    /// The node did not answer in time.
    #[error("the node did not answer in time")]
    Timeout,
    /// The node could not be reached or refused the request.
    #[error("the node is not answering: {0}")]
    Unavailable(String),
    /// The answer was not in the expected shape.
    #[error("the node's answer could not be read")]
    Invalid,
}

impl SourceError {
    fn from_rpc(err: RpcError) -> Self {
        match err {
            RpcError::Rpc { code: -5 | -8, .. } => SourceError::NotFound,
            RpcError::Rpc { code, .. } => SourceError::Rpc { code },
            RpcError::Http(err) if err.is_timeout() => SourceError::Timeout,
            RpcError::InvalidResponse(_) => SourceError::Invalid,
            RpcError::Cookie { .. } => {
                SourceError::Unavailable("the RPC cookie could not be read".into())
            }
            RpcError::Http(_) | RpcError::Status(_) | RpcError::EmptyResponse => {
                SourceError::Unavailable("the request failed".into())
            }
        }
    }
}

/// Reads the chain. The explorer's actor holds one per node run; tests use a synthetic one.
pub(crate) trait ChainSource: Clone + Send + Sync + 'static {
    fn read(&self, call: ReadCall) -> impl Future<Output = Result<ReadResult, SourceError>> + Send;
}

/// A [`ChainSource`] over zebrad's cookie-authenticated RPC. The cookie is read on the first
/// call and again whenever zebrad refuses it.
#[derive(Clone)]
pub(crate) struct RpcSource {
    client: RpcClient,
    session: Arc<OnceCell<OwnedSession>>,
}

impl RpcSource {
    pub(crate) fn new(addr: SocketAddr, cookie_file: PathBuf) -> Result<Self, RpcError> {
        Ok(Self {
            client: RpcClient::new(addr, cookie_file)?,
            session: Arc::new(OnceCell::new()),
        })
    }

    async fn call<T: DeserializeOwned>(&self, call: ReadCall) -> Result<T, SourceError> {
        let session = self
            .session
            .get_or_try_init(|| self.client.owned_session())
            .await
            .map_err(SourceError::from_rpc)?;
        session
            .call_timeout(call.method(), call.params(), REQUEST_TIMEOUT)
            .await
            .map_err(SourceError::from_rpc)
    }
}

impl ChainSource for RpcSource {
    async fn read(&self, call: ReadCall) -> Result<ReadResult, SourceError> {
        Ok(match call {
            ReadCall::Best => ReadResult::Best(self.call(call).await?),
            ReadCall::Block(_) => ReadResult::Block(Arc::new(self.call(call).await?)),
            ReadCall::BlockHash(_) => ReadResult::BlockHash(self.call(call).await?),
            ReadCall::Header(_) => ReadResult::Header(self.call(call).await?),
            ReadCall::Tx(_) => ReadResult::Tx(Arc::new(self.call(call).await?)),
            ReadCall::Subsidy(_) => ReadResult::Subsidy(Arc::new(self.call(call).await?)),
        })
    }
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::Hash32;

    use super::*;

    #[test]
    fn calls_name_zebrads_methods_and_parameters() {
        let hash = Hash32([0xab; 32]);
        let cases = [
            (ReadCall::Best, "getbestblockheightandhash", json!([])),
            (
                ReadCall::Block(BlockId::Height(3_120_000)),
                "getblock",
                json!(["3120000", 1]),
            ),
            (
                ReadCall::Block(BlockId::Hash(hash)),
                "getblock",
                json!([hash.to_hex(), 1]),
            ),
            (ReadCall::BlockHash(7), "getblockhash", json!([7])),
            (
                ReadCall::Header(BlockId::Height(5)),
                "getblockheader",
                json!(["5", true]),
            ),
            (
                ReadCall::Tx(hash),
                "getrawtransaction",
                json!([hash.to_hex(), 1]),
            ),
            (ReadCall::Subsidy(9), "getblocksubsidy", json!([9])),
        ];
        for (call, method, params) in cases {
            assert_eq!(call.method(), method);
            assert_eq!(call.params(), params);
        }
    }

    #[test]
    fn errors_keep_codes_and_drop_details() {
        let rpc = |code| RpcError::Rpc {
            code,
            message: "Block not found: 00ab".into(),
        };
        assert_eq!(SourceError::from_rpc(rpc(-8)), SourceError::NotFound);
        assert_eq!(SourceError::from_rpc(rpc(-5)), SourceError::NotFound);
        assert_eq!(
            SourceError::from_rpc(rpc(-32601)),
            SourceError::Rpc { code: -32601 }
        );
        let invalid = SourceError::from_rpc(RpcError::InvalidResponse("a txid 00ab".into()));
        assert_eq!(invalid, SourceError::Invalid);
        assert!(!invalid.to_string().contains("00ab"));
        assert!(matches!(
            SourceError::from_rpc(RpcError::Status(401)),
            SourceError::Unavailable(_)
        ));
    }
}
