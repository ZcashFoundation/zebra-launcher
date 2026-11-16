//! The read-only network checks: this computer's LAN address and gateway from the default
//! route, a VPN on the route the kernel prefers, and the router's public address from
//! NAT-PMP's external-address request (RFC 6886, opcode 0), which creates no mapping. Nothing
//! here changes the router.
//!
//! PS1 reads the routes from `/proc/net/route` (Linux). UPnP IGD's
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

/// A route of the main table, as `/proc/net/route` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub interface: String,
    pub destination: Ipv4Addr,
    pub mask: Ipv4Addr,
    /// `None` for a route without a gateway, such as a tunnel's or the LAN's own.
    pub gateway: Option<Ipv4Addr>,
    pub metric: u32,
}

impl Route {
    fn is_default(&self) -> bool {
        self.destination.is_unspecified() && self.mask.is_unspecified()
    }
}

/// The IPv4 routes in `/proc/net/route` text, which lists the main table. The kernel writes
/// addresses as little-endian hex and the metric in decimal. Routes without an interface
/// (unreachable, prohibit) are left out.
pub fn parse_routes(text: &str) -> Vec<Route> {
    let addr = |field: &str| {
        u32::from_str_radix(field, 16)
            .ok()
            .map(|hex| Ipv4Addr::from(hex.swap_bytes()))
    };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let interface = *fields.first()?;
            if interface == "*" {
                return None;
            }
            let gateway = addr(fields.get(2)?)?;
            Some(Route {
                interface: interface.to_owned(),
                destination: addr(fields.get(1)?)?,
                mask: addr(fields.get(7)?)?,
                gateway: (!gateway.is_unspecified()).then_some(gateway),
                metric: fields.get(6)?.parse().ok()?,
            })
        })
        .collect()
}

/// The default route through the LAN's router: its interface and gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultRoute {
    pub interface: String,
    pub gateway: Ipv4Addr,
}

/// The default route with a gateway and the lowest metric.
pub fn default_route(routes: &[Route]) -> Option<DefaultRoute> {
    routes
        .iter()
        .filter(|route| route.is_default())
        .filter_map(|route| Some((route, route.gateway?)))
        .min_by_key(|(route, _)| route.metric)
        .map(|(route, gateway)| DefaultRoute {
            interface: route.interface.clone(),
            gateway,
        })
}

/// The default route the kernel prefers, with a gateway or without: the lowest metric, the
/// first listed of equals.
fn preferred_default(routes: &[Route]) -> Option<&Route> {
    routes
        .iter()
        .filter(|route| route.is_default())
        .min_by_key(|route| route.metric)
}

/// The VPN on the default route the kernel prefers, if any: its interface when the name looks
/// like a VPN's, or when it is not the router's, as with a tunnel's default without a gateway
/// at a lower metric than the router's. Without a router, only a VPN's name counts.
pub fn vpn_on_route(routes: &[Route], router: Option<&DefaultRoute>) -> Option<String> {
    let preferred = preferred_default(routes)?;
    let elsewhere = router.is_some_and(|router| router.interface != preferred.interface);
    (elsewhere || is_vpn_interface(&preferred.interface)).then(|| preferred.interface.clone())
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
    let routes = tokio::fs::read_to_string("/proc/net/route")
        .await
        .map(|text| parse_routes(&text))
        .unwrap_or_default();
    let router = default_route(&routes);
    let vpn = vpn_on_route(&routes, router.as_ref());
    let Some(router) = router else {
        return NetFacts {
            lan_addr: None,
            gateway: None,
            interface: None,
            vpn,
            wan: WanKind::Unknown,
            wan_addr: None,
            checked_at: now,
        };
    };
    let lan_addr = lan_address(router.gateway).await;
    let wan_addr = if vpn.is_some() {
        None
    } else {
        nat_pmp_external(SocketAddr::from((router.gateway, NAT_PMP_PORT)))
            .await
            .map(IpAddr::V4)
    };
    NetFacts {
        lan_addr,
        gateway: Some(IpAddr::V4(router.gateway)),
        interface: Some(router.interface),
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

    /// Parses `text` and gives the router and the VPN, as `check` does.
    fn router_and_vpn(text: &str) -> (Option<DefaultRoute>, Option<String>) {
        let routes = parse_routes(text);
        let router = default_route(&routes);
        let vpn = vpn_on_route(&routes, router.as_ref());
        (router, vpn)
    }

    fn router(interface: &str) -> Option<DefaultRoute> {
        Some(DefaultRoute {
            interface: interface.into(),
            gateway: Ipv4Addr::new(192, 168, 1, 1),
        })
    }

    #[test]
    fn the_default_route_names_its_gateway() {
        assert_eq!(router_and_vpn(ROUTE), (router("eth0"), None));
        assert_eq!(router_and_vpn("Iface\tDestination\n"), (None, None));
        assert_eq!(
            parse_routes(ROUTE)[0],
            Route {
                interface: "eth0".into(),
                destination: Ipv4Addr::new(192, 168, 1, 0),
                mask: Ipv4Addr::new(255, 255, 255, 0),
                gateway: None,
                metric: 0,
            }
        );
    }

    /// openconnect's vpnc-script on a NetworkManager laptop: `ip route replace default dev
    /// tun0` adds a default without a gateway at metric 0 beside the DHCP default at 600.
    const TUNNEL_FIRST: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
tun0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";

    /// Captured from a kernel: a VPN's `default dev tun0 metric 50` beside the router's
    /// default at metric 100.
    const TUNNEL_METRIC_50: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
tun0\t00000000\t00000000\t0001\t0\t0\t50\t00000000\t0\t0\t0
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
tun0\t0000080A\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";

    #[test]
    fn a_preferred_default_without_a_gateway_is_the_vpn() {
        assert_eq!(
            router_and_vpn(TUNNEL_FIRST),
            (router("wlan0"), Some("tun0".into()))
        );
        assert_eq!(
            router_and_vpn(TUNNEL_METRIC_50),
            (router("eth0"), Some("tun0".into()))
        );
        // The metric decides, not the order of the lines.
        let mut lines: Vec<&str> = TUNNEL_FIRST.lines().collect();
        lines.swap(1, 2);
        assert_eq!(
            router_and_vpn(&lines.join("\n")),
            (router("wlan0"), Some("tun0".into()))
        );
        // A tunnel's default behind the router's carries nothing.
        let behind = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
tun0\t00000000\t00000000\t0001\t0\t0\t900\t00000000\t0\t0\t0
";
        assert_eq!(router_and_vpn(behind), (router("eth0"), None));
    }

    #[test]
    fn a_vpn_that_replaced_the_default_route_is_seen_without_a_router() {
        let replaced = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
tun0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
wlan0\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0
";
        assert_eq!(router_and_vpn(replaced), (None, Some("tun0".into())));
        // Without a router, a default without a gateway is a VPN's only by its name.
        let bare = replaced.replace("tun0", "eth0");
        assert_eq!(router_and_vpn(&bare), (None, None));
        // An unreachable default names no interface.
        let unreachable = replaced.replace("tun0", "*");
        assert_eq!(router_and_vpn(&unreachable), (None, None));
    }

    #[test]
    fn a_vpn_default_with_a_gateway_ahead_of_the_routers_is_the_vpn() {
        let ahead = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
tun0\t00000000\t0100080A\t0003\t0\t0\t50\t00000000\t0\t0\t0
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
";
        let (router, vpn) = router_and_vpn(ahead);
        assert_eq!(
            router.map(|router| router.interface).as_deref(),
            Some("tun0")
        );
        assert_eq!(vpn.as_deref(), Some("tun0"));
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
