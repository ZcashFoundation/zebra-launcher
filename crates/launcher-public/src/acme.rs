//! Certificates from Let's Encrypt, through `instant-acme` with ring: the `AcmeActor`.
//!
//! TLS-ALPN-01 on the forwarded port: the actor builds the challenge certificate (RFC 8737,
//! with `rcgen`) and hands it to the front door, which answers ALPN `acme-tls/1` for the name.
//! Staging first, for every new name (its validation is the setup's first evidence from
//! outside, and no wallet trusts it), then production with a fresh P-256 key. The chain is
//! verified with the platform verifier, the trust a phone uses, before the front door gets it.
//! Renewal at two thirds of the lifetime; never an order at startup while a certificate is
//! valid. ARI (`renewal_info`, `replaces`) comes later.
//!
//! The order budget in `attempts.json` keeps Zeal far under Let's Encrypt's limits: 3
//! production orders per name per 7 days, 2 failures an hour, a backoff of 1, 4, 12 and 24
//! hours between failures, and a stop until the Retry-After when Let's Encrypt says
//! `rateLimited`.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use instant_acme::{
    Account, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus,
    RetryPolicy,
};
use launcher_gateway::{CertifiedKey, Challenge};
use launcher_types::public::{AcmeProblem, CertIssuer, CertStatus};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime, pem::PemObject};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::store::{self, PublicPaths};

/// Let's Encrypt's directories.
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
pub const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";

/// The certificate profile Zeal asks for where the directory offers it.
const PROFILE: &str = "classic";

/// Where the actor gets certificates and how it checks them.
#[derive(Clone, Debug)]
pub struct AcmeConfig {
    pub paths: PublicPaths,
    pub name: String,
    pub staging: String,
    pub production: String,
    /// A root for the ACME server's own HTTPS, for test servers; `None` uses the platform's.
    pub server_root: Option<PathBuf>,
    /// Roots trusted besides the platform's when an issued chain is verified, for test
    /// servers.
    pub extra_roots: Vec<CertificateDer<'static>>,
    /// How long an order may take to validate and to be issued.
    pub order_timeout: Duration,
}

impl AcmeConfig {
    /// Let's Encrypt for `name`.
    pub fn lets_encrypt(paths: PublicPaths, name: &str) -> Self {
        AcmeConfig {
            paths,
            name: name.to_string(),
            staging: LETS_ENCRYPT_STAGING.to_string(),
            production: LETS_ENCRYPT_PRODUCTION.to_string(),
            server_root: None,
            extra_roots: Vec::new(),
            order_timeout: Duration::from_secs(120),
        }
    }
}

/// An issued certificate.
#[derive(Clone, Debug)]
pub struct Issued {
    pub chain_pem: String,
    pub key_pem: String,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
    /// A TLS-ALPN-01 validation happened for this order; a reused authorization has none.
    pub validated: bool,
}

/// The problem type's last part (`urn:ietf:params:acme:error:connection`), mapped to its fix.
fn map_problem(problem: &instant_acme::Problem, retry_at: Option<SystemTime>) -> AcmeProblem {
    let kind = problem
        .r#type
        .as_deref()
        .and_then(|kind| kind.rsplit(':').next())
        .unwrap_or_default();
    match kind {
        "connection" | "tls" => AcmeProblem::Connection,
        "dns" => AcmeProblem::Dns,
        "unauthorized" | "incorrectResponse" => AcmeProblem::Unauthorized,
        "caa" => AcmeProblem::Caa,
        "rateLimited" => AcmeProblem::RateLimited { retry_at },
        _ => AcmeProblem::Other {
            detail: problem.detail.clone().unwrap_or_else(|| kind.to_string()),
        },
    }
}

fn map_error(err: instant_acme::Error) -> AcmeProblem {
    match err {
        instant_acme::Error::Api(problem) => map_problem(&problem, None),
        instant_acme::Error::Hyper(_) | instant_acme::Error::Http(_) => AcmeProblem::Unreachable,
        instant_acme::Error::Timeout(_) => AcmeProblem::Other {
            detail: "Let's Encrypt did not finish the order in time".into(),
        },
        other => AcmeProblem::Other {
            detail: other.to_string(),
        },
    }
}

/// The TLS-ALPN-01 certificate for `name`: self-signed, with the critical `acmeIdentifier`
/// extension holding the SHA-256 digest of the key authorization (RFC 8737).
pub fn challenge_cert(name: &str, digest: &[u8]) -> Result<Arc<CertifiedKey>, AcmeProblem> {
    let other = |detail: String| AcmeProblem::Other { detail };
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()])
        .map_err(|err| other(err.to_string()))?;
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let key = rcgen::KeyPair::generate().map_err(|err| other(err.to_string()))?;
    let cert = params
        .self_signed(&key)
        .map_err(|err| other(err.to_string()))?;
    // Loaded without rustls's key check, which parses the certificate and refuses the
    // critical extension it does not know. The certificate is only ever presented.
    let signing = launcher_gateway::provider()
        .key_provider
        .load_private_key(PrivateKeyDer::Pkcs8(key.serialize_der().into()))
        .map_err(|err| other(err.to_string()))?;
    Ok(Arc::new(CertifiedKey::new(
        vec![cert.der().clone()],
        signing,
    )))
}

/// The validity of the first certificate in `chain_pem`.
pub fn validity(chain_pem: &str) -> Result<(SystemTime, SystemTime), AcmeProblem> {
    let bad = || AcmeProblem::Verify;
    let first = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .next()
        .ok_or_else(bad)?
        .map_err(|_| bad())?;
    let (_, cert) = x509_parser::parse_x509_certificate(&first).map_err(|_| bad())?;
    let at = |time: x509_parser::time::ASN1Time| {
        UNIX_EPOCH + Duration::from_secs(time.timestamp().max(0) as u64)
    };
    let validity = cert.validity();
    Ok((at(validity.not_before), at(validity.not_after)))
}

/// Verifies `chain_pem` for `name` with the platform's roots and `extra_roots`.
pub fn verify_chain(
    chain_pem: &str,
    name: &str,
    extra_roots: &[CertificateDer<'static>],
) -> Result<(), AcmeProblem> {
    check_chain(chain_pem, name, extra_roots).map_err(|detail| {
        tracing::warn!(target: "zeal::public", %detail, "the issued chain does not verify");
        AcmeProblem::Verify
    })
}

/// [`verify_chain`], with the verifier's own words on failure.
pub fn check_chain(
    chain_pem: &str,
    name: &str,
    extra_roots: &[CertificateDer<'static>],
) -> Result<(), String> {
    use rustls::client::danger::ServerCertVerifier;
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|err| format!("PEM: {err}"))?;
    let (end, intermediates) = chain.split_first().ok_or("no certificate")?;
    let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
        extra_roots.iter().cloned(),
        launcher_gateway::provider(),
    )
    .map_err(|err| err.to_string())?;
    let server = ServerName::try_from(name.to_string()).map_err(|err| err.to_string())?;
    verifier
        .verify_server_cert(end, intermediates, &server, &[], UnixTime::now())
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// When to renew a certificate valid from `not_before` to `not_after`: at two thirds of its
/// lifetime.
pub fn renew_at(not_before: SystemTime, not_after: SystemTime) -> SystemTime {
    let lifetime = not_after.duration_since(not_before).unwrap_or_default();
    not_before + lifetime * 2 / 3
}

/// `attempts.json`: the orders and failures that count against the budget.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Attempts {
    /// Production orders, when they were placed.
    pub production: Vec<SystemTime>,
    /// Failed orders, staging or production, newest last.
    pub failures: Vec<SystemTime>,
    /// Failures since the last success.
    pub consecutive_failures: u32,
    /// Let's Encrypt's Retry-After from a `rateLimited` answer.
    pub rate_limited_until: Option<SystemTime>,
}

/// Production orders per name in [`WEEK`].
const PRODUCTION_PER_WEEK: usize = 3;
const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);
/// Failures per name in [`HOUR`].
const FAILURES_PER_HOUR: usize = 2;
const HOUR: Duration = Duration::from_secs(3600);
/// The wait after the first, second, third and later consecutive failures.
const BACKOFF_HOURS: [u64; 4] = [1, 4, 12, 24];

impl Attempts {
    /// When the next order for `issuer` may be placed: `None` for now. `asked` is the user's
    /// click, which skips the backoff but never the hourly budget or a rate limit.
    pub fn next_allowed(
        &self,
        issuer: CertIssuer,
        now: SystemTime,
        asked: bool,
    ) -> Option<SystemTime> {
        let mut at: Option<SystemTime> = None;
        let mut later = |time: SystemTime| {
            if time > now {
                at = Some(at.map_or(time, |at: SystemTime| at.max(time)));
            }
        };
        if let Some(until) = self.rate_limited_until {
            later(until);
        }
        let recent = |times: &[SystemTime], window: Duration| -> Vec<SystemTime> {
            times
                .iter()
                .copied()
                .filter(|time| now.duration_since(*time).is_ok_and(|age| age < window))
                .collect()
        };
        let failures = recent(&self.failures, HOUR);
        if failures.len() >= FAILURES_PER_HOUR
            && let Some(oldest) = failures.iter().min()
        {
            later(*oldest + HOUR);
        }
        if issuer == CertIssuer::Production {
            let orders = recent(&self.production, WEEK);
            if orders.len() >= PRODUCTION_PER_WEEK
                && let Some(oldest) = orders.iter().min()
            {
                later(*oldest + WEEK);
            }
        }
        if !asked
            && self.consecutive_failures > 0
            && let Some(last) = self.failures.last()
        {
            let step = (self.consecutive_failures as usize - 1).min(BACKOFF_HOURS.len() - 1);
            later(*last + Duration::from_secs(BACKOFF_HOURS[step] * 3600));
        }
        at
    }

    /// Records an order's outcome at `now`.
    pub fn record(
        &mut self,
        issuer: CertIssuer,
        result: &Result<(), AcmeProblem>,
        now: SystemTime,
    ) {
        if issuer == CertIssuer::Production {
            self.production.push(now);
        }
        match result {
            Ok(()) => self.consecutive_failures = 0,
            Err(problem) => {
                self.failures.push(now);
                self.consecutive_failures += 1;
                if let AcmeProblem::RateLimited { retry_at } = problem {
                    self.rate_limited_until = Some(retry_at.unwrap_or(now + HOUR));
                }
            }
        }
        let keep = |times: &mut Vec<SystemTime>| {
            times.retain(|time| now.duration_since(*time).is_ok_and(|age| age < WEEK));
        };
        keep(&mut self.production);
        keep(&mut self.failures);
    }
}

/// The account for `directory`, loaded from `path` or created there.
async fn account(
    config: &AcmeConfig,
    directory: &str,
    path: PathBuf,
) -> Result<Account, AcmeProblem> {
    let builder = || match &config.server_root {
        Some(root) => Account::builder_with_root(root),
        None => Account::builder(),
    };
    let saved = tokio::fs::read(&path).await.ok();
    if let Some(credentials) = saved.and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
        return builder()
            .map_err(map_error)?
            .from_credentials(credentials)
            .await
            .map_err(map_error);
    }
    let new = NewAccount {
        // No contact address: Let's Encrypt ended expiry emails.
        contact: &[],
        terms_of_service_agreed: true,
        only_return_existing: false,
    };
    let (account, credentials) = builder()
        .map_err(map_error)?
        .create(&new, directory.to_string(), None)
        .await
        .map_err(map_error)?;
    let json = serde_json::to_vec(&credentials).map_err(|err| AcmeProblem::Other {
        detail: err.to_string(),
    })?;
    tokio::task::spawn_blocking(move || store::write_private(&path, &json))
        .await
        .map_err(|err| AcmeProblem::Other {
            detail: err.to_string(),
        })?
        .map_err(|err| AcmeProblem::Other {
            detail: err.to_string(),
        })?;
    Ok(account)
}

/// One order for `config.name` from `issuer`, answering its TLS-ALPN-01 challenge through
/// `challenge`, which the front door reads.
pub async fn order(
    config: &AcmeConfig,
    issuer: CertIssuer,
    challenge: &watch::Sender<Option<Arc<Challenge>>>,
) -> Result<Issued, AcmeProblem> {
    let (directory, environment) = match issuer {
        CertIssuer::Staging => (&config.staging, "staging"),
        CertIssuer::Production => (&config.production, "production"),
    };
    let account = account(config, directory, config.paths.account(environment)).await?;
    let identifiers = [Identifier::Dns(config.name.clone())];
    let mut new_order = NewOrder::new(&identifiers);
    if account.profiles().any(|profile| profile.name == PROFILE) {
        new_order = new_order.profile(PROFILE);
    }
    let mut order = account.new_order(&new_order).await.map_err(map_error)?;
    let retry = RetryPolicy::new().timeout(config.order_timeout);

    let mut validated = false;
    {
        let mut authorizations = order.authorizations();
        while let Some(authorization) = authorizations.next().await {
            let mut authorization = authorization.map_err(map_error)?;
            match authorization.status {
                // Reused: Let's Encrypt validated the name recently.
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                _ => return Err(AcmeProblem::Unauthorized),
            }
            let mut handle = authorization
                .challenge(ChallengeType::TlsAlpn01)
                .ok_or_else(|| AcmeProblem::Other {
                    detail: "no TLS-ALPN-01 challenge offered".into(),
                })?;
            let digest = handle.key_authorization().digest();
            let key = challenge_cert(&config.name, digest.as_ref())?;
            challenge.send_replace(Some(Arc::new(Challenge {
                name: config.name.clone(),
                key,
            })));
            handle.set_ready().await.map_err(map_error)?;
            validated = true;
        }
    }
    let status = order.poll_ready(&retry).await;
    challenge.send_replace(None);
    let status = status.map_err(map_error)?;
    if status != OrderStatus::Ready {
        // The challenge's own error says why.
        let mut problem = AcmeProblem::Unauthorized;
        let mut authorizations = order.authorizations();
        while let Some(Ok(mut authorization)) = authorizations.next().await {
            if let Ok(state) = authorization.refresh().await
                && let Some(error) = state.challenges.iter().find_map(|c| c.error.as_ref())
            {
                problem = map_problem(error, None);
            }
        }
        return Err(problem);
    }
    let key_pem = order.finalize().await.map_err(map_error)?;
    let chain_pem = order.poll_certificate(&retry).await.map_err(map_error)?;
    let (not_before, not_after) = validity(&chain_pem)?;
    Ok(Issued {
        chain_pem,
        key_pem,
        not_before,
        not_after,
        validated,
    })
}

/// A production certificate kept on disk for `name`, if it is valid now and its key is
/// owner-only.
pub fn load_stored(paths: &PublicPaths, name: &str, now: SystemTime) -> Option<Issued> {
    let key_path = paths.key(name);
    if store::readable_by_others(&key_path) {
        tracing::warn!(target: "zeal::public", "the stored key is readable by others; ignoring it");
        return None;
    }
    let chain_pem = std::fs::read_to_string(paths.cert(name)).ok()?;
    let key_pem = std::fs::read_to_string(&key_path).ok()?;
    let (not_before, not_after) = validity(&chain_pem).ok()?;
    (not_after > now).then_some(Issued {
        chain_pem,
        key_pem,
        not_before,
        not_after,
        validated: false,
    })
}

/// What the actor may be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcmeCmd {
    /// The setup reached the certificate step, or the user asked to try again.
    Order { asked: bool },
}

/// The running actor. Dropping it stops the actor.
#[derive(Debug)]
pub struct AcmeHandle {
    pub status: watch::Receiver<CertStatus>,
    /// The served certificate, for the front door.
    pub cert: watch::Receiver<Option<Arc<CertifiedKey>>>,
    /// The latest TLS-ALPN-01 validation of a new order: evidence from outside.
    pub validated: watch::Receiver<Option<SystemTime>>,
    pub commands: mpsc::Sender<AcmeCmd>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AcmeHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl AcmeHandle {
    /// Starts the actor for `config.name`. `challenge` goes to the front door. `can_validate`
    /// says whether the front door is up, so renewals wait for it: Zeal never opens the port
    /// to renew. `staging_validated_at` is the last staging validation kept from earlier runs.
    pub fn spawn(
        config: AcmeConfig,
        challenge: watch::Sender<Option<Arc<Challenge>>>,
        can_validate: watch::Receiver<bool>,
        staging_validated_at: Option<SystemTime>,
    ) -> Self {
        let now = SystemTime::now();
        let stored = load_stored(&config.paths, &config.name, now);
        let cert = stored.as_ref().and_then(|issued| {
            launcher_gateway::certified_key_from_pem(&issued.chain_pem, &issued.key_pem).ok()
        });
        let status = CertStatus {
            not_after: stored.as_ref().map(|issued| issued.not_after),
            not_before: stored.as_ref().map(|issued| issued.not_before),
            renew_at: stored
                .as_ref()
                .map(|issued| renew_at(issued.not_before, issued.not_after)),
            staging_validated_at,
            ordering: None,
            next_try: None,
            last_problem: None,
        };
        let (status_tx, status_rx) = watch::channel(status);
        let (cert_tx, cert_rx) = watch::channel(cert);
        let (validated_tx, validated) = watch::channel(None);
        let (commands, commands_rx) = mpsc::channel(4);
        let task = tokio::spawn(run(
            config,
            challenge,
            can_validate,
            status_tx,
            cert_tx,
            validated_tx,
            commands_rx,
        ));
        AcmeHandle {
            status: status_rx,
            cert: cert_rx,
            validated,
            commands,
            task,
        }
    }
}

async fn run(
    config: AcmeConfig,
    challenge: watch::Sender<Option<Arc<Challenge>>>,
    mut can_validate: watch::Receiver<bool>,
    status: watch::Sender<CertStatus>,
    cert: watch::Sender<Option<Arc<CertifiedKey>>>,
    validated: watch::Sender<Option<SystemTime>>,
    mut commands: mpsc::Receiver<AcmeCmd>,
) {
    let attempts_path = config.paths.attempts(&config.name);
    let mut attempts: Attempts = store::load(&attempts_path);
    let mut wanted = false;
    let mut asked = false;
    loop {
        let now = SystemTime::now();
        let current = status.borrow().clone();
        let valid = current.not_after.is_some_and(|not_after| not_after > now);
        let renewal_due = current.renew_at.is_some_and(|at| at <= now);
        let issuer = if current.staging_validated_at.is_none() && !valid {
            CertIssuer::Staging
        } else {
            CertIssuer::Production
        };
        let needed = (wanted && !valid)
            || (valid && renewal_due)
            || (wanted && issuer == CertIssuer::Staging);
        let blocked = attempts.next_allowed(issuer, now, asked);
        if needed && *can_validate.borrow() && blocked.is_none() {
            status.send_modify(|s| {
                s.ordering = Some(issuer);
                s.next_try = None;
            });
            let result = order(&config, issuer, &challenge).await;
            let now = SystemTime::now();
            let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
            attempts.record(issuer, &outcome, now);
            let _ = store::save(&attempts_path, &attempts, false);
            asked = false;
            match result {
                Ok(issued) => {
                    if issued.validated {
                        validated.send_replace(Some(now));
                    }
                    match issuer {
                        CertIssuer::Staging => {
                            tracing::info!(target: "zeal::public", "Let's Encrypt staging validated the name");
                            status.send_modify(|s| {
                                s.ordering = None;
                                s.staging_validated_at = Some(now);
                                s.last_problem = None;
                            });
                        }
                        CertIssuer::Production => {
                            let installed = install(&config, &issued);
                            match installed {
                                Ok(key) => {
                                    tracing::info!(target: "zeal::public", "certificate issued");
                                    cert.send_replace(Some(key));
                                    status.send_modify(|s| {
                                        s.ordering = None;
                                        s.not_before = Some(issued.not_before);
                                        s.not_after = Some(issued.not_after);
                                        s.renew_at =
                                            Some(renew_at(issued.not_before, issued.not_after));
                                        s.last_problem = None;
                                    });
                                    wanted = false;
                                }
                                Err(problem) => status.send_modify(|s| {
                                    s.ordering = None;
                                    s.last_problem = Some(problem);
                                }),
                            }
                        }
                    }
                }
                Err(problem) => {
                    tracing::warn!(target: "zeal::public", ?problem, ?issuer, "certificate order failed");
                    let next = attempts.next_allowed(issuer, now, false);
                    status.send_modify(|s| {
                        s.ordering = None;
                        s.last_problem = Some(problem);
                        s.next_try = next;
                    });
                }
            }
            continue;
        }
        if needed && blocked.is_some() {
            status.send_modify(|s| s.next_try = blocked);
        }
        // Wake for a command, the front door coming up, the next allowed try or renewal.
        let wake = blocked
            .or(current.renew_at.filter(|_| valid))
            .and_then(|at| at.duration_since(now).ok())
            .unwrap_or(Duration::from_secs(3600))
            .min(Duration::from_secs(3600));
        tokio::select! {
            command = commands.recv() => match command {
                Some(AcmeCmd::Order { asked: by_user }) => {
                    wanted = true;
                    asked |= by_user;
                }
                None => return,
            },
            changed = can_validate.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = tokio::time::sleep(wake) => {}
        }
    }
}

/// Verifies and stores a production certificate, then loads it for the front door.
fn install(config: &AcmeConfig, issued: &Issued) -> Result<Arc<CertifiedKey>, AcmeProblem> {
    verify_chain(&issued.chain_pem, &config.name, &config.extra_roots)?;
    let key = launcher_gateway::certified_key_from_pem(&issued.chain_pem, &issued.key_pem)
        .map_err(|_| AcmeProblem::Verify)?;
    let io = |err: std::io::Error| AcmeProblem::Other {
        detail: err.to_string(),
    };
    store::write_private(&config.paths.key(&config.name), issued.key_pem.as_bytes()).map_err(io)?;
    store::write_public(
        &config.paths.cert(&config.name),
        issued.chain_pem.as_bytes(),
    )
    .map_err(io)?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hours: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000 * 3600 + hours * 3600)
    }

    #[test]
    fn renewals_fall_at_two_thirds() {
        let not_before = at(0);
        let not_after = at(90 * 24);
        assert_eq!(renew_at(not_before, not_after), at(60 * 24));
    }

    #[test]
    fn the_budget_spaces_failures_and_caps_production_orders() {
        let mut attempts = Attempts::default();
        assert_eq!(
            attempts.next_allowed(CertIssuer::Staging, at(0), false),
            None
        );
        attempts.record(CertIssuer::Staging, &Err(AcmeProblem::Connection), at(0));
        // An hour's backoff, which a click skips.
        assert_eq!(
            attempts.next_allowed(CertIssuer::Staging, at(0), false),
            Some(at(1))
        );
        assert_eq!(
            attempts.next_allowed(CertIssuer::Staging, at(0), true),
            None
        );
        attempts.record(CertIssuer::Staging, &Err(AcmeProblem::Connection), at(0));
        // Two failures in the hour: not even a click.
        assert_eq!(
            attempts.next_allowed(CertIssuer::Staging, at(0), true),
            Some(at(1))
        );
        // The second failure in a row waits 4 hours.
        assert_eq!(
            attempts.next_allowed(CertIssuer::Staging, at(2), false),
            Some(at(4))
        );
        attempts.record(CertIssuer::Staging, &Ok(()), at(5));
        assert_eq!(
            attempts.next_allowed(CertIssuer::Production, at(5), false),
            None
        );
        for hour in [5, 6, 7] {
            attempts.record(CertIssuer::Production, &Ok(()), at(hour));
        }
        assert_eq!(
            attempts.next_allowed(CertIssuer::Production, at(8), true),
            Some(at(5 + 7 * 24))
        );
        assert_eq!(
            attempts.next_allowed(CertIssuer::Staging, at(8), true),
            None
        );
    }

    #[test]
    fn a_rate_limit_waits_for_the_retry_after() {
        let mut attempts = Attempts::default();
        attempts.record(
            CertIssuer::Production,
            &Err(AcmeProblem::RateLimited {
                retry_at: Some(at(30)),
            }),
            at(0),
        );
        assert_eq!(
            attempts.next_allowed(CertIssuer::Production, at(10), true),
            Some(at(30))
        );
    }

    #[test]
    fn a_challenge_certificate_names_the_host() {
        let key = challenge_cert("zec.example.com", &[7u8; 32]).unwrap();
        let der = key.end_entity_cert().unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(der).unwrap();
        let san = cert.subject_alternative_name().unwrap().unwrap();
        assert!(format!("{:?}", san.value).contains("zec.example.com"));
        // The acmeIdentifier extension, 1.3.6.1.5.5.7.1.31, critical.
        let acme = cert
            .extensions()
            .iter()
            .find(|ext| ext.oid.to_id_string() == "1.3.6.1.5.5.7.1.31")
            .expect("acmeIdentifier");
        assert!(acme.critical);
        assert!(acme.value.ends_with(&[7u8; 32]));
    }
}
