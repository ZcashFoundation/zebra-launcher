//! Zeal's public services without Tauri (docs/PUBLIC-SERVICES.md): the service actor and its
//! children. PS1 serves light wallets from zebrad's own light wallet endpoint, on Linux,
//! through the front door (`launcher-gateway`).
//!
//! The pure rules (the setup's plan, pauses, the wallet check) are in `launcher-herd`; this
//! crate gathers the facts they read and carries out what they decide. It keeps the network
//! dependencies (ACME, the keychain, HTTPS) in one place, reviewed and cargo-denied as a unit.

pub mod acme;
pub mod awake;
pub mod ddns;
pub mod dns;
pub mod firewall;
pub mod gateway;
pub mod grpc;
pub mod light;
pub mod netcheck;
pub mod secrets;
pub mod selftest;
pub mod service;
pub mod store;

pub use service::{EndpointHost, Options, PublicServices, Wiring, prepare_launch};

/// Makes ring the process's default rustls provider if none is yet, so clients built without
/// an explicit provider (instant-acme's, the updater's) find one. The first install wins.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
