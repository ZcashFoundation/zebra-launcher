//! The `LightMonitor`: zebrad's light wallet endpoint as wallets see it. It calls
//! `GetLightdInfo` over h2c on loopback when the endpoint opens, every 10 minutes and when
//! asked, and runs the wallet check against zebrad's own height.

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime},
};

use launcher_herd::public::{Checked, wallet};
use launcher_types::{
    Network, NodeStats, Status,
    public::{LightdInfo, WalletProblem},
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
};

use crate::grpc;

/// How often the endpoint is checked while it is open.
pub const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// One wallet check of the endpoint at `addr`.
pub async fn check(
    addr: SocketAddr,
    network: Network,
    node_height: Option<u64>,
) -> Result<LightdInfo, WalletProblem> {
    let no_answer = |detail: String| WalletProblem::NoAnswer { detail };
    let stream = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr))
        .await
        .map_err(|_| no_answer("no answer".into()))?
        .map_err(|err| no_answer(err.to_string()))?;
    let info = grpc::get_lightd_info(stream, &addr.to_string(), false)
        .await
        .map_err(|err| no_answer(err.to_string()))?;
    wallet::check(&info, network, node_height)?;
    Ok(info)
}

/// The running monitor. Dropping it stops the monitor.
#[derive(Debug)]
pub struct LightMonitor {
    pub result: watch::Receiver<Option<Checked<Result<LightdInfo, WalletProblem>>>>,
    pub now: mpsc::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LightMonitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LightMonitor {
    pub fn spawn(
        network: Network,
        status: watch::Receiver<Status>,
        stats: watch::Receiver<Option<NodeStats>>,
    ) -> Self {
        let (result_tx, result) = watch::channel(None);
        let (now, now_rx) = mpsc::channel(2);
        let task = tokio::spawn(run(network, status, stats, result_tx, now_rx));
        LightMonitor { result, now, task }
    }
}

async fn run(
    network: Network,
    mut status: watch::Receiver<Status>,
    stats: watch::Receiver<Option<NodeStats>>,
    result: watch::Sender<Option<Checked<Result<LightdInfo, WalletProblem>>>>,
    mut now: mpsc::Receiver<()>,
) {
    let mut last: Option<SocketAddr> = None;
    let mut due = true;
    loop {
        let endpoint = status.borrow_and_update().lightwalletd_addr;
        if endpoint != last {
            last = endpoint;
            result.send_replace(None);
            due = true;
        }
        if due && let Some(addr) = endpoint {
            let height = stats.borrow().as_ref().map(|stats| stats.height);
            let outcome = check(addr, network, height).await;
            result.send_replace(Some(Checked {
                at: SystemTime::now(),
                result: outcome,
            }));
            due = false;
        }
        tokio::select! {
            changed = status.changed() => if changed.is_err() { return },
            asked = now.recv() => match asked {
                Some(()) => due = true,
                None => return,
            },
            _ = tokio::time::sleep(CHECK_EVERY) => due = true,
        }
    }
}
