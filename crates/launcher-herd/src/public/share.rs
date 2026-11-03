//! What wallets enter to use the server.

use launcher_types::public::{PublicHostname, ShareInfo};

/// The strings per wallet for `name` on the router's `port`. The port is always written:
/// Zodl requires it, and Zingo may assume 9067 without one.
pub fn share(name: &PublicHostname, port: u16) -> ShareInfo {
    ShareInfo {
        zodl: format!("{name}:{port}"),
        zingo: format!("https://{name}:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_wallet_gets_the_port() {
        let name = PublicHostname::parse("zec-alice.duckdns.org").unwrap();
        let info = share(&name, 443);
        assert_eq!(info.zodl, "zec-alice.duckdns.org:443");
        assert_eq!(info.zingo, "https://zec-alice.duckdns.org:443");
    }
}
