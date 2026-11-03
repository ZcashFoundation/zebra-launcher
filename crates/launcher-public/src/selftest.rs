//! The self-tests: what a wallet sees through the front door. The test on this computer
//! connects to the front door on loopback with the real name, verifies the chain as a phone
//! would and runs the wallet check; the hairpin test connects to the router's public address,
//! which only works once the service is open and the router loops connections back.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use launcher_types::{
    Network,
    public::{LightdInfo, LocalTestProblem},
};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

use crate::grpc;

/// How long a connection may take.
const CONNECT: Duration = Duration::from_secs(10);

/// A TLS client that trusts what a phone trusts, plus `extra_roots` (test CAs), asking for h2.
fn client(extra_roots: &[CertificateDer<'static>]) -> Result<TlsConnector, String> {
    let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
        extra_roots.iter().cloned(),
        launcher_gateway::provider(),
    )
    .map_err(|err| err.to_string())?;
    let mut config = rustls::ClientConfig::builder_with_provider(launcher_gateway::provider())
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![launcher_gateway::H2_ALPN.to_vec()];
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn connect(
    addr: SocketAddr,
    name: &str,
    extra_roots: &[CertificateDer<'static>],
) -> Result<TlsStream<TcpStream>, LocalTestProblem> {
    let connector =
        client(extra_roots).map_err(|detail| LocalTestProblem::Certificate { detail })?;
    let server =
        ServerName::try_from(name.to_string()).map_err(|err| LocalTestProblem::Certificate {
            detail: err.to_string(),
        })?;
    let tcp = tokio::time::timeout(CONNECT, TcpStream::connect(addr))
        .await
        .map_err(|_| LocalTestProblem::Connect {
            detail: "no answer".into(),
        })?
        .map_err(|err| LocalTestProblem::Connect {
            detail: err.to_string(),
        })?;
    match tokio::time::timeout(CONNECT, connector.connect(server, tcp)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(err))
            if err
                .get_ref()
                .is_some_and(|inner| inner.is::<rustls::Error>()) =>
        {
            Err(LocalTestProblem::Certificate {
                detail: err.to_string(),
            })
        }
        Ok(Err(err)) => Err(LocalTestProblem::Connect {
            detail: err.to_string(),
        }),
        Err(_) => Err(LocalTestProblem::Connect {
            detail: "the handshake did not finish".into(),
        }),
    }
}

/// The test on this computer: the front door on loopback at `port` with SNI `name`, the chain
/// verified, then `GetLightdInfo` and `GetLatestBlock` through it, and Zodl's check of what
/// wallets will be told.
pub async fn local_test(
    port: u16,
    name: &str,
    extra_roots: &[CertificateDer<'static>],
    network: Network,
    node_height: Option<u64>,
) -> Result<LightdInfo, LocalTestProblem> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let authority = format!("{name}:{port}");
    let stream = connect(addr, name, extra_roots).await?;
    let info = grpc::get_lightd_info(stream, &authority, true)
        .await
        .map_err(|err| LocalTestProblem::Wallet {
            problem: launcher_types::public::WalletProblem::NoAnswer {
                detail: err.to_string(),
            },
        })?;
    let stream = connect(addr, name, extra_roots).await?;
    let latest = grpc::get_latest_block(stream, &authority, true)
        .await
        .map_err(|err| LocalTestProblem::Wallet {
            problem: launcher_types::public::WalletProblem::NoAnswer {
                detail: err.to_string(),
            },
        })?;
    launcher_herd::public::wallet::check(&info, network, node_height.or(Some(latest.height)))
        .map_err(|problem| LocalTestProblem::Wallet { problem })?;
    Ok(info)
}

/// The hairpin test: whether a TLS handshake with the right name completes through the
/// router's public address `wan` on the external `port`. A failure proves nothing: many routers
/// do not loop connections back.
pub async fn hairpin(
    wan: IpAddr,
    port: u16,
    name: &str,
    extra_roots: &[CertificateDer<'static>],
) -> bool {
    connect(SocketAddr::from((wan, port)), name, extra_roots)
        .await
        .is_ok()
}
