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
pub mod grpc;
pub mod netcheck;
pub mod secrets;
pub mod store;
