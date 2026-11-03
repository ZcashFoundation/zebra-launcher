//! The front door's TLS: one config per handshake, chosen from the ClientHello. A pending
//! Let's Encrypt challenge gets ALPN `acme-tls/1` and its challenge certificate; the service
//! gets ALPN `h2` and the served certificate. TLS 1.2 and 1.3, no client certificates.

use std::sync::Arc;

use rustls::{
    ServerConfig,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};

/// The ALPN protocol of a TLS-ALPN-01 challenge (RFC 8737).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// HTTP/2 over TLS, as gRPC clients ask for it.
pub const H2_ALPN: &[u8] = b"h2";

/// ring, the one crypto provider in Zeal, given explicitly so no process default is needed.
pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A certificate chain and its key, as the front door serves them.
pub fn certified_key(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<CertifiedKey>, rustls::Error> {
    CertifiedKey::from_der(chain, key, &provider()).map(Arc::new)
}

/// [`certified_key`] from PEM text: the chain's certificates, then the private key.
pub fn certified_key_from_pem(
    chain_pem: &str,
    key_pem: &str,
) -> Result<Arc<CertifiedKey>, rustls::Error> {
    let chain = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| rustls::Error::General(format!("certificate PEM: {err}")))?;
    if chain.is_empty() {
        return Err(rustls::Error::General("no certificate in the PEM".into()));
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|err| rustls::Error::General(format!("key PEM: {err}")))?;
    certified_key(chain, key)
}

/// Always the same certificate.
#[derive(Debug)]
struct Fixed(Arc<CertifiedKey>);

impl ResolvesServerCert for Fixed {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

fn config(key: Arc<CertifiedKey>, alpn: &[u8]) -> Result<Arc<ServerConfig>, rustls::Error> {
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(key)));
    // A client that offers ALPN without this protocol is refused with
    // `no_application_protocol`.
    config.alpn_protocols = vec![alpn.to_vec()];
    Ok(Arc::new(config))
}

/// The service's config: ALPN `h2` and the served certificate.
pub(crate) fn service_config(key: Arc<CertifiedKey>) -> Result<Arc<ServerConfig>, rustls::Error> {
    config(key, H2_ALPN)
}

/// A challenge's config: ALPN `acme-tls/1` and the challenge certificate.
pub(crate) fn challenge_config(key: Arc<CertifiedKey>) -> Result<Arc<ServerConfig>, rustls::Error> {
    config(key, ACME_TLS_ALPN)
}

/// The SNI a ClientHello asks for, lowercased and without a trailing dot.
pub(crate) fn requested_name(hello: &ClientHello<'_>) -> Option<String> {
    hello
        .server_name()
        .map(|name| name.trim_end_matches('.').to_ascii_lowercase())
}

/// Whether a ClientHello offers the TLS-ALPN-01 protocol.
pub(crate) fn offers_acme(hello: &ClientHello<'_>) -> bool {
    hello
        .alpn()
        .is_some_and(|mut protocols| protocols.any(|protocol| protocol == ACME_TLS_ALPN))
}
