//! Where the agreement actor asks its probes: a closed set of one call, `getblockhash`, over
//! each node's own RPC. Tests use a scripted source instead.

use std::{future::Future, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use launcher_herd::herd::ProbeResult;
use serde_json::json;
use tokio::sync::OnceCell;

use crate::rpc::{OwnedSession, RpcClient, RpcError};

/// zebrad's code for a height above its tip ("Provided index is greater than the current
/// tip").
const ABOVE_TIP: i64 = -32602;
/// zebrad's code for a block it does not have ("Block not found").
const NOT_FOUND: i64 = -8;

/// Answers probes for one node's run.
pub trait TipSource: Clone + Send + Sync + 'static {
    /// The hash of the block at `height` on the node's best chain, within `timeout`.
    fn hash_at(&self, height: u32, timeout: Duration) -> impl Future<Output = ProbeResult> + Send;
}

/// A [`TipSource`] over zebrad's cookie-authenticated RPC. The cookie is read on the first
/// probe and again whenever zebrad refuses it.
#[derive(Clone, Debug)]
pub struct RpcTipSource {
    client: RpcClient,
    session: Arc<OnceCell<OwnedSession>>,
}

impl RpcTipSource {
    pub fn new(addr: SocketAddr, cookie_file: PathBuf) -> Result<Self, RpcError> {
        Ok(Self {
            client: RpcClient::new(addr, cookie_file)?,
            session: Arc::new(OnceCell::new()),
        })
    }
}

impl TipSource for RpcTipSource {
    async fn hash_at(&self, height: u32, timeout: Duration) -> ProbeResult {
        let session = match self
            .session
            .get_or_try_init(|| self.client.owned_session())
            .await
        {
            Ok(session) => session,
            Err(err) => {
                tracing::debug!(%err, "the herd's probe could not read the RPC cookie");
                return ProbeResult::Failed;
            }
        };
        result_of(
            session
                .call_timeout::<String>("getblockhash", json!([height]), timeout)
                .await,
        )
    }
}

/// A `getblockhash` answer as a probe's result. Logs name the method only.
fn result_of(answer: Result<String, RpcError>) -> ProbeResult {
    match answer {
        Ok(hash) => ProbeResult::Hash(hash),
        Err(RpcError::Rpc {
            code: ABOVE_TIP, ..
        }) => ProbeResult::AboveTip,
        Err(RpcError::Rpc {
            code: NOT_FOUND, ..
        }) => ProbeResult::NotFound,
        Err(err) => {
            tracing::debug!(%err, "getblockhash failed");
            ProbeResult::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zebrads_answers_become_probe_results() {
        let rpc = |code| RpcError::Rpc {
            code,
            message: String::new(),
        };
        assert_eq!(
            result_of(Ok("00ab".into())),
            ProbeResult::Hash("00ab".into())
        );
        assert_eq!(result_of(Err(rpc(-32602))), ProbeResult::AboveTip);
        assert_eq!(result_of(Err(rpc(-8))), ProbeResult::NotFound);
        assert_eq!(result_of(Err(rpc(-32601))), ProbeResult::Failed);
        assert_eq!(
            result_of(Err(RpcError::InvalidResponse("x".into()))),
            ProbeResult::Failed
        );
    }
}
