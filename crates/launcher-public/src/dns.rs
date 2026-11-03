//! What a name resolves to, through the system resolver.

use std::net::IpAddr;

/// The addresses `name` resolves to, IPv4 first, without duplicates. Empty when it does not
/// resolve.
pub async fn resolve(name: &str) -> Vec<IpAddr> {
    let Ok(found) = tokio::net::lookup_host((name, 443)).await else {
        return Vec::new();
    };
    let mut addrs: Vec<IpAddr> = Vec::new();
    for addr in found {
        if !addrs.contains(&addr.ip()) {
            addrs.push(addr.ip());
        }
    }
    addrs.sort_by_key(|addr| addr.is_ipv6());
    addrs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn localhost_resolves_and_nonsense_does_not() {
        assert!(resolve("localhost").await.iter().any(IpAddr::is_loopback));
        assert!(resolve("zeal-test.invalid").await.is_empty());
    }
}
