//! The name step's actor: one `DdnsActor` per name keeps a DuckDNS name's address current with
//! the user's token, or only checks a name kept by hand. It compares every 5 minutes and when
//! the router's address changes, updates only on a change, backs off after errors and stops
//! asking after a refusal. It resolves the name through the system resolver every 30 s while
//! the name does not point here, and every 5 minutes after.
//!
//! No request URL is formatted into an error, a log line or the status: the URL holds the
//! token. Failures are reported by kind ([`DdnsProblem`]).

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr},
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use launcher_herd::public::names::{duckdns_subdomain, keychain_account};
use launcher_types::public::{DdnsProblem, DnsProvider, DnsStatus, PublicHostname};
use tokio::sync::{mpsc, watch};

use crate::secrets::{SecretError, SecretStore};

/// DuckDNS's update API.
pub const DUCKDNS_API: &str = "https://www.duckdns.org";

/// How often the address is compared and the name resolved once it points here.
const SETTLED_EVERY: Duration = Duration::from_secs(5 * 60);

/// How often the name is resolved while it does not point here.
const WAITING_EVERY: Duration = Duration::from_secs(30);

/// Backoff after failed updates, and the longest.
const FIRST_BACKOFF: Duration = Duration::from_secs(60);
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// The answer to a GET: the HTTP status and the body, or `Err(())` without one. Errors carry
/// no URL.
pub type Fetched = Pin<Box<dyn Future<Output = Result<(u16, String), ()>> + Send>>;

/// An HTTPS GET.
pub trait Fetch: Send + Sync + 'static {
    fn get(&self, url: String) -> Fetched;
}

/// [`Fetch`] over HTTPS with rustls, ring and the platform's roots.
#[derive(Clone, Debug)]
pub struct HttpsFetch {
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Empty<Bytes>,
    >,
}

impl HttpsFetch {
    pub fn new() -> std::io::Result<Self> {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_provider_and_platform_verifier(launcher_gateway::provider())?
            .https_only()
            .enable_http1()
            .build();
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);
        Ok(HttpsFetch { client })
    }
}

impl Fetch for HttpsFetch {
    fn get(&self, url: String) -> Fetched {
        let client = self.client.clone();
        Box::pin(async move {
            let uri: http::Uri = url.parse().map_err(|_| ())?;
            let request = tokio::time::timeout(Duration::from_secs(20), client.get(uri));
            let response = request.await.map_err(|_| ())?.map_err(|_| ())?;
            let status = response.status().as_u16();
            let body = response.into_body().collect().await.map_err(|_| ())?;
            let body = String::from_utf8_lossy(&body.to_bytes()).into_owned();
            Ok((status, body))
        })
    }
}

/// What DuckDNS answered to an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Updated {
    /// The address DuckDNS saw or set.
    pub address: Option<IpAddr>,
    pub changed: bool,
}

/// One DuckDNS update of `subdomain` to `address`, or to the address DuckDNS sees with `None`.
/// `verbose=true` makes it echo the address.
pub async fn duckdns_update(
    fetch: &dyn Fetch,
    api: &str,
    subdomain: &str,
    token: &str,
    address: Option<Ipv4Addr>,
) -> Result<Updated, DdnsProblem> {
    let ip = address.map(|ip| ip.to_string()).unwrap_or_default();
    let url = format!("{api}/update?domains={subdomain}&token={token}&ip={ip}&verbose=true");
    let (status, body) = fetch
        .get(url)
        .await
        .map_err(|()| DdnsProblem::Unreachable)?;
    if status == 429 {
        return Err(DdnsProblem::TooMany);
    }
    if status != 200 {
        return Err(DdnsProblem::BadAnswer);
    }
    parse_duckdns(&body)
}

/// DuckDNS's verbose answer: `OK`, the IPv4 address, the IPv6 address, then `UPDATED` or
/// `NOCHANGE`; or `KO`.
pub fn parse_duckdns(body: &str) -> Result<Updated, DdnsProblem> {
    let mut lines = body.lines().map(str::trim);
    match lines.next() {
        Some("OK") => {}
        Some("KO") => return Err(DdnsProblem::Refused),
        _ => return Err(DdnsProblem::BadAnswer),
    }
    let address = lines.next().and_then(|line| line.parse().ok());
    let _ipv6 = lines.next();
    let changed = lines.next() == Some("UPDATED");
    Ok(Updated { address, changed })
}

/// What the actor needs.
pub struct DdnsConfig {
    pub name: PublicHostname,
    pub provider: DnsProvider,
    pub api: String,
    pub fetch: Arc<dyn Fetch>,
    pub secrets: Arc<dyn SecretStore>,
    /// The address the name must point at: the router's public address, when known.
    pub wanted: watch::Receiver<Option<IpAddr>>,
}

/// What the actor may be asked.
#[derive(Debug)]
pub enum DdnsCmd {
    /// Update and resolve now: a new token, or "Run checks".
    Now,
}

/// The running actor: its status, and a way to poke it. Dropping it stops the actor.
#[derive(Debug)]
pub struct DdnsHandle {
    pub status: watch::Receiver<DnsStatus>,
    pub commands: mpsc::Sender<DdnsCmd>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DdnsHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl DdnsHandle {
    pub fn spawn(config: DdnsConfig) -> Self {
        let initial = DnsStatus {
            name: config.name.clone(),
            provider: config.provider,
            has_token: false,
            resolved: Vec::new(),
            provider_saw: None,
            updated_at: None,
            problem: None,
            checked_at: None,
        };
        let (status_tx, status) = watch::channel(initial);
        let (commands, commands_rx) = mpsc::channel(4);
        let task = tokio::spawn(run(config, status_tx, commands_rx));
        DdnsHandle {
            status,
            commands,
            task,
        }
    }
}

async fn run(
    mut config: DdnsConfig,
    status: watch::Sender<DnsStatus>,
    mut commands: mpsc::Receiver<DdnsCmd>,
) {
    let account = keychain_account(config.provider, &config.name);
    let subdomain = duckdns_subdomain(&config.name).map(str::to_string);
    let mut last_sent: Option<Option<IpAddr>> = None;
    let mut backoff = FIRST_BACKOFF;
    let mut retry_at: Option<tokio::time::Instant> = None;
    let mut refused = false;
    loop {
        let wanted = *config.wanted.borrow_and_update();
        // The address update, for DuckDNS.
        if let (DnsProvider::DuckDns, Some(subdomain)) = (config.provider, &subdomain) {
            let token = config.secrets.get(account.clone()).await;
            let has_token = token.is_ok();
            status.send_modify(|s| s.has_token = has_token);
            let due = retry_at.is_none_or(|at| tokio::time::Instant::now() >= at);
            match token {
                Ok(token) if !refused && due && last_sent != Some(wanted) => {
                    let v4 = match wanted {
                        Some(IpAddr::V4(v4)) => Some(v4),
                        _ => None,
                    };
                    let result = duckdns_update(
                        config.fetch.as_ref(),
                        &config.api,
                        subdomain,
                        token.trim(),
                        v4,
                    )
                    .await;
                    match result {
                        Ok(updated) => {
                            last_sent = Some(wanted);
                            backoff = FIRST_BACKOFF;
                            retry_at = None;
                            if updated.changed {
                                tracing::info!(target: "zeal::public", "DuckDNS took the new address");
                            }
                            status.send_modify(|s| {
                                s.provider_saw = updated.address;
                                s.updated_at = Some(SystemTime::now());
                                s.problem = None;
                            });
                        }
                        Err(problem) => {
                            tracing::warn!(target: "zeal::public", ?problem, "DuckDNS update failed");
                            refused = problem == DdnsProblem::Refused;
                            retry_at = Some(tokio::time::Instant::now() + backoff);
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                            status.send_modify(|s| s.problem = Some(problem));
                        }
                    }
                }
                Err(SecretError::Missing) => {
                    status.send_modify(|s| s.problem = None);
                }
                _ => {}
            }
        }
        // What the name resolves to now.
        let resolved = crate::dns::resolve(config.name.as_str()).await;
        let points_here = wanted.is_none_or(|wanted| resolved.contains(&wanted));
        status.send_modify(|s| {
            s.resolved = resolved;
            s.checked_at = Some(SystemTime::now());
        });
        let every = if points_here && !resolved_empty(&status) {
            SETTLED_EVERY
        } else {
            WAITING_EVERY
        };
        tokio::select! {
            command = commands.recv() => match command {
                Some(DdnsCmd::Now) => {
                    // A new token or an explicit check: ask again.
                    refused = false;
                    retry_at = None;
                    last_sent = None;
                }
                None => return,
            },
            changed = config.wanted.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = tokio::time::sleep(every) => {}
        }
    }
}

fn resolved_empty(status: &watch::Sender<DnsStatus>) -> bool {
    status.borrow().resolved.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckdns_answers_parse() {
        assert_eq!(
            parse_duckdns("OK\n203.0.113.10\n\nUPDATED"),
            Ok(Updated {
                address: Some("203.0.113.10".parse().unwrap()),
                changed: true
            })
        );
        assert_eq!(
            parse_duckdns("OK\n203.0.113.10\n\nNOCHANGE").map(|u| u.changed),
            Ok(false)
        );
        assert_eq!(parse_duckdns("KO"), Err(DdnsProblem::Refused));
        assert_eq!(parse_duckdns("<html>"), Err(DdnsProblem::BadAnswer));
    }

    /// A DuckDNS that refuses every token, keeping the URLs it was asked.
    struct Refusing(mpsc::UnboundedSender<String>);

    impl Fetch for Refusing {
        fn get(&self, url: String) -> Fetched {
            let _ = self.0.send(url);
            Box::pin(async { Ok((200, "KO".to_string())) })
        }
    }

    #[tokio::test]
    async fn a_refused_update_reports_its_kind_and_never_the_token() {
        let token = "a7c4d0ad-114e-40ef-ba1d-d217904a50f2";
        let dir = tempfile::tempdir().unwrap();
        let secrets = Arc::new(crate::secrets::FileStore::new(dir.path()));
        let name = PublicHostname::parse("zeal-test.duckdns.org").unwrap();
        secrets
            .set(keychain_account(DnsProvider::DuckDns, &name), token.into())
            .await
            .unwrap();
        let (urls_tx, mut urls) = mpsc::unbounded_channel();
        let (_wanted_tx, wanted) = watch::channel(Some("203.0.113.10".parse().unwrap()));
        let mut handle = DdnsHandle::spawn(DdnsConfig {
            name,
            provider: DnsProvider::DuckDns,
            api: "https://duckdns.invalid".into(),
            fetch: Arc::new(Refusing(urls_tx)),
            secrets,
            wanted,
        });
        let status = tokio::time::timeout(
            Duration::from_secs(10),
            handle.status.wait_for(|s| s.problem.is_some()),
        )
        .await
        .unwrap()
        .unwrap()
        .clone();
        assert_eq!(status.problem, Some(DdnsProblem::Refused));
        assert!(status.has_token);
        // The token went to DuckDNS, and nowhere else.
        let url = urls.recv().await.unwrap();
        assert!(url.contains(token) && url.contains("ip=203.0.113.10"));
        let shown = format!("{status:?} {}", serde_json::to_string(&status).unwrap());
        assert!(!shown.contains(token), "{shown}");
        let problem = format!("{:?}", status.problem);
        assert!(!problem.contains("duckdns.invalid"));
    }
}
