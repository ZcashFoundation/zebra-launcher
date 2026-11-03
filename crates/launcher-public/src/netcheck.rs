//! The read-only network checks: this computer's LAN address and gateway from the default
//! route, a VPN on it, and the router's public address from NAT-PMP's external-address
//! request (RFC 6886, opcode 0), which creates no mapping. Nothing here changes the router.
//!
//! PS1 reads the default route from `/proc/net/route` (Linux). UPnP IGD's
//! `GetExternalIPAddress` and the router's model come later; without NAT-PMP the router's
//! address is unknown and the outside checks decide.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::{Duration, SystemTime},
};

use launcher_types::public::{NetFacts, WanKind};
use tokio::net::UdpSocket;

/// NAT-PMP's port on the gateway.
const NAT_PMP_PORT: u16 = 5351;

/// The NAT-PMP request is retried with these waits, a short version of RFC 6886's schedule.
const NAT_PMP_WAITS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(1000),
];

/// Interface names that carry a VPN, a heuristic: WireGuard, OpenVPN and other tunnels, PPP,
/// Tailscale, ZeroTier, and the names of a few VPN apps.
const VPN_PREFIXES: [&str; 9] = [
    "tun",
    "tap",
    "wg",
    "ppp",
    "tailscale",
    "zt",
    "utun",
    "nordlynx",
    "proton",
];

/// The default route: its interface and gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultRoute {
    pub interface: String,
    pub gateway: Ipv4Addr,
}

/// The IPv4 default route in `/proc/net/route` text: the first line with destination and
/// mask 0 and a gateway. The kernel writes addresses as little-endian hex.
pub fn parse_proc_route(text: &str) -> Option<DefaultRoute> {
    let hex = |field: &str| u32::from_str_radix(field, 16).ok();
    text.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (interface, destination, gateway, mask) = (
            fields.first()?,
            fields.get(1)?,
            fields.get(2)?,
            fields.get(7)?,
        );
        if hex(destination)? != 0 || hex(mask)? != 0 {
            return None;
        }
        let gateway = Ipv4Addr::from(hex(gateway)?.swap_bytes());
        (!gateway.is_unspecified()).then(|| DefaultRoute {
            interface: interface.to_string(),
            gateway,
        })
    })
}

/// Whether `interface` looks like a VPN's.
pub fn is_vpn_interface(interface: &str) -> bool {
    let lower = interface.to_ascii_lowercase();
    VPN_PREFIXES.iter().any(|prefix| lower.starts_with(prefix))
}

/// How the router reaches the internet, from the address it reports.
pub fn classify_wan(addr: Option<IpAddr>) -> WanKind {
    match addr {
        Some(IpAddr::V4(v4)) => {
            let [a, b, ..] = v4.octets();
            if a == 100 && (64..128).contains(&b) {
                WanKind::Cgnat
            } else if v4.is_private() || v4.is_link_local() || v4.is_loopback() {
                WanKind::BehindAnotherRouter
            } else if v4.is_unspecified() {
                WanKind::Unknown
            } else {
                WanKind::Public
            }
        }
        Some(IpAddr::V6(_)) | None => WanKind::Unknown,
    }
}

/// The address this computer uses towards `gateway`. A UDP `connect` only picks the route:
/// no packet is sent.
pub async fn lan_address(gateway: Ipv4Addr) -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.ok()?;
    socket.connect((gateway, 9)).await.ok()?;
    let local = socket.local_addr().ok()?.ip();
    (!local.is_unspecified()).then_some(local)
}

/// The router's public address from a NAT-PMP external-address request to `gateway`, or
/// `None` without an answer.
pub async fn nat_pmp_external(gateway: SocketAddr) -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.ok()?;
    socket.connect(gateway).await.ok()?;
    let mut answer = [0u8; 16];
    for wait in NAT_PMP_WAITS {
        socket.send(&[0, 0]).await.ok()?;
        if let Ok(Ok(n)) = tokio::time::timeout(wait, socket.recv(&mut answer)).await {
            return parse_nat_pmp(&answer[..n]);
        }
    }
    None
}

/// A NAT-PMP external-address answer: version 0, opcode 128, result 0, then the address.
pub fn parse_nat_pmp(answer: &[u8]) -> Option<Ipv4Addr> {
    if answer.len() < 12 || answer[0] != 0 || answer[1] != 128 {
        return None;
    }
    let result = u16::from_be_bytes([answer[2], answer[3]]);
    (result == 0).then(|| Ipv4Addr::new(answer[8], answer[9], answer[10], answer[11]))
}

/// Every read-only fact, at `now`.
pub async fn check(now: SystemTime) -> NetFacts {
    let route = tokio::fs::read_to_string("/proc/net/route")
        .await
        .ok()
        .and_then(|text| parse_proc_route(&text));
    let Some(route) = route else {
        return NetFacts {
            lan_addr: None,
            gateway: None,
            interface: None,
            vpn: None,
            wan: WanKind::Unknown,
            wan_addr: None,
            checked_at: now,
        };
    };
    let lan_addr = lan_address(route.gateway).await;
    let vpn = is_vpn_interface(&route.interface).then(|| route.interface.clone());
    let wan_addr = if vpn.is_some() {
        None
    } else {
        nat_pmp_external(SocketAddr::from((route.gateway, NAT_PMP_PORT)))
            .await
            .map(IpAddr::V4)
    };
    NetFacts {
        lan_addr,
        gateway: Some(IpAddr::V4(route.gateway)),
        interface: Some(route.interface),
        vpn,
        wan: classify_wan(wan_addr),
        wan_addr,
        checked_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
";

    #[test]
    fn the_default_route_names_its_gateway() {
        assert_eq!(
            parse_proc_route(ROUTE),
            Some(DefaultRoute {
                interface: "eth0".into(),
                gateway: Ipv4Addr::new(192, 168, 1, 1),
            })
        );
        assert_eq!(parse_proc_route("Iface\tDestination\n"), None);
    }

    #[test]
    fn vpn_interfaces_are_recognised() {
        for vpn in ["tun0", "wg0", "tailscale0", "ppp0", "utun3", "nordlynx"] {
            assert!(is_vpn_interface(vpn), "{vpn}");
        }
        for lan in ["eth0", "enp3s0", "wlan0", "wlp2s0", "eno1"] {
            assert!(!is_vpn_interface(lan), "{lan}");
        }
    }

    #[test]
    fn wan_addresses_are_classified() {
        let v4 = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        assert_eq!(classify_wan(v4("203.0.113.10")), WanKind::Public);
        assert_eq!(classify_wan(v4("100.72.4.9")), WanKind::Cgnat);
        assert_eq!(classify_wan(v4("100.128.0.1")), WanKind::Public);
        assert_eq!(
            classify_wan(v4("192.168.0.2")),
            WanKind::BehindAnotherRouter
        );
        assert_eq!(classify_wan(v4("10.1.2.3")), WanKind::BehindAnotherRouter);
        assert_eq!(classify_wan(None), WanKind::Unknown);
    }

    #[test]
    fn nat_pmp_answers_parse() {
        let ok = [0, 128, 0, 0, 0, 0, 1, 0, 203, 0, 113, 10];
        assert_eq!(parse_nat_pmp(&ok), Some(Ipv4Addr::new(203, 0, 113, 10)));
        let refused = [0, 128, 0, 3, 0, 0, 1, 0, 0, 0, 0, 0];
        assert_eq!(parse_nat_pmp(&refused), None);
        assert_eq!(parse_nat_pmp(&[0, 129]), None);
    }

    #[tokio::test]
    async fn nat_pmp_asks_without_mapping_anything() {
        // A fake router that answers the external-address request only.
        let router = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = router.local_addr().unwrap();
        tokio::spawn(async move {
            let mut request = [0u8; 16];
            if let Ok((n, from)) = router.recv_from(&mut request).await {
                assert_eq!(&request[..n], &[0, 0], "opcode 0 only");
                let answer = [0, 128, 0, 0, 0, 0, 1, 0, 198, 51, 100, 7];
                let _ = router.send_to(&answer, from).await;
            }
        });
        assert_eq!(
            nat_pmp_external(addr).await,
            Some(Ipv4Addr::new(198, 51, 100, 7))
        );
    }
}
