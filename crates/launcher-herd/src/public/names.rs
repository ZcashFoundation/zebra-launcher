//! Names: what the user types for each provider, DuckDNS's subdomains, and where a token is
//! kept.

use launcher_types::public::{DnsProvider, HostnameError, PublicHostname};

/// DuckDNS's zone: every name is `<subdomain>.duckdns.org`.
pub const DUCKDNS_ZONE: &str = "duckdns.org";

/// The page where a DuckDNS name and token are made, compiled into Zeal: the UI never sends a
/// URL to open.
pub const DUCKDNS_SIGNUP: &str = "https://www.duckdns.org/";

/// The keychain's service name for Zeal's entries.
pub const KEYCHAIN_SERVICE: &str = "org.zfnd.zeal";

/// The name the user typed for `provider`: a DuckDNS subdomain alone (`zec-alice`) gets
/// `.duckdns.org`, and a DuckDNS name must be one label under it.
pub fn parse_name(input: &str, provider: DnsProvider) -> Result<PublicHostname, HostnameError> {
    let input = input.trim().trim_end_matches('.');
    match provider {
        DnsProvider::DuckDns => {
            let lower = input.to_ascii_lowercase();
            let full = if lower.contains('.') {
                lower
            } else {
                format!("{lower}.{DUCKDNS_ZONE}")
            };
            let name = PublicHostname::parse(&full)?;
            duckdns_subdomain(&name)
                .map(|_| name.clone())
                .ok_or(HostnameError::BadLabel)
        }
        DnsProvider::Manual => PublicHostname::parse(input),
    }
}

/// The subdomain DuckDNS's update API takes for `name` (`zec-alice` for
/// `zec-alice.duckdns.org`), if it is a DuckDNS name.
pub fn duckdns_subdomain(name: &PublicHostname) -> Option<&str> {
    let sub = name
        .as_str()
        .strip_suffix(DUCKDNS_ZONE)?
        .strip_suffix('.')?;
    (!sub.is_empty() && !sub.contains('.')).then_some(sub)
}

/// The keychain account for `provider`'s token for `name`: `dns:<provider>:<zone>`.
pub fn keychain_account(provider: DnsProvider, name: &PublicHostname) -> String {
    let provider = match provider {
        DnsProvider::DuckDns => "duckdns",
        DnsProvider::Manual => "manual",
    };
    format!("dns:{provider}:{name}")
}

/// Whether `token` looks like a DuckDNS token: a UUID, 36 characters of hex digits and
/// hyphens. Anything else is refused before it reaches the keychain or the network.
pub fn plausible_duckdns_token(token: &str) -> bool {
    let token = token.trim();
    token.len() == 36
        && token.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckdns_names_are_one_label_under_the_zone() {
        for input in [
            "zec-alice",
            "zec-alice.duckdns.org",
            " ZEC-alice.DuckDNS.org. ",
        ] {
            let name = parse_name(input, DnsProvider::DuckDns).unwrap();
            assert_eq!(name.as_str(), "zec-alice.duckdns.org");
            assert_eq!(duckdns_subdomain(&name), Some("zec-alice"));
        }
        assert_eq!(
            parse_name("a.b.duckdns.org", DnsProvider::DuckDns),
            Err(HostnameError::BadLabel)
        );
        assert_eq!(
            parse_name("zec.example.com", DnsProvider::DuckDns),
            Err(HostnameError::BadLabel)
        );
        assert_eq!(
            parse_name("", DnsProvider::DuckDns),
            Err(HostnameError::BadLabel)
        );
        let manual = parse_name("zec.example.com", DnsProvider::Manual).unwrap();
        assert_eq!(duckdns_subdomain(&manual), None);
        assert_eq!(
            parse_name("zec", DnsProvider::Manual),
            Err(HostnameError::SingleLabel)
        );
    }

    #[test]
    fn keychain_accounts_name_the_provider_and_zone() {
        let name = PublicHostname::parse("zec-alice.duckdns.org").unwrap();
        assert_eq!(
            keychain_account(DnsProvider::DuckDns, &name),
            "dns:duckdns:zec-alice.duckdns.org"
        );
    }

    #[test]
    fn duckdns_tokens_are_uuids() {
        assert!(plausible_duckdns_token(
            "a7c4d0ad-114e-40ef-ba1d-d217904a50f2"
        ));
        assert!(plausible_duckdns_token(
            " A7C4D0AD-114E-40EF-BA1D-D217904A50F2 "
        ));
        for bad in [
            "",
            "a7c4d0ad114e40efba1dd217904a50f2",
            "a7c4d0ad-114e-40ef-ba1d-d217904a50fg",
            "a7c4d0ad-114e-40ef-ba1d-d217904a50f2&ip=1",
        ] {
            assert!(!plausible_duckdns_token(bad), "{bad:?}");
        }
    }
}
