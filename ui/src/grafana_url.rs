//! Builds the URL the Grafana tab embeds: the chosen dashboard, time range, refresh interval
//! and theme on top of the Grafana URL from Settings. Pure Rust, tested natively.
//!
//! The base URL comes in three forms, all accepted by `launcher_types::is_loopback_http_url`:
//! an origin (`http://127.0.0.1:3000`), an origin with a path prefix
//! (`http://localhost:3000/grafana`), or a full dashboard URL copied from Grafana
//! (`http://127.0.0.1:3000/d/zebra-overview/zebra-overview?orgId=1&kiosk`). The dashboard
//! segment `/d/<uid>` is replaced or appended, the query keys this module sets itself are
//! replaced, everything else (a path prefix, `orgId`, `var-*`) is kept, and a fragment is dropped.

/// A dashboard this repository ships in `docs/observability/grafana/dashboards/`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DashboardRef {
    /// Grafana's `uid`: the segment after `/d/` in the dashboard's URL.
    pub uid: &'static str,
    /// The name the picker shows.
    pub title: &'static str,
}

/// The 14 shipped dashboards, in the order the picker lists them.
pub const DASHBOARDS: &[DashboardRef] = &[
    DashboardRef {
        uid: "zebra-overview",
        title: "Zebra overview",
    },
    DashboardRef {
        uid: "Sl3h19Gnk",
        title: "Syncer",
    },
    DashboardRef {
        uid: "o4LmN_OMk",
        title: "Checkpoint verification",
    },
    DashboardRef {
        uid: "rO_Cl5tGz",
        title: "Block verification",
    },
    DashboardRef {
        uid: "UXVRR1v7z",
        title: "Transaction verification",
    },
    DashboardRef {
        uid: "S29TgUH7k",
        title: "Peers",
    },
    DashboardRef {
        uid: "320aS_dMk",
        title: "Network health",
    },
    DashboardRef {
        uid: "YQ3yxiVnk",
        title: "Network messages",
    },
    DashboardRef {
        uid: "wVXGE6v7z",
        title: "Mempool",
    },
    DashboardRef {
        uid: "IhbO11wGk",
        title: "Errors",
    },
    DashboardRef {
        uid: "zebra-value-pools",
        title: "Value pools",
    },
    DashboardRef {
        uid: "zebra-rpc-metrics",
        title: "RPC metrics",
    },
    DashboardRef {
        uid: "zebra-rpc-tracing",
        title: "RPC tracing",
    },
    DashboardRef {
        uid: "zebra-rocksdb",
        title: "RocksDB",
    },
];

/// The dashboard shown until the user picks another: Zebra's overview, which is also Grafana's
/// home dashboard in the shipped stack.
pub const DEFAULT_UID: &str = "zebra-overview";

/// How far back the embedded dashboard looks. The default matches the overview dashboard's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    M15,
    #[default]
    H1,
    H6,
    H24,
    D7,
}

impl Range {
    /// Every range, in picker order.
    pub const ALL: [Range; 5] = [Range::M15, Range::H1, Range::H6, Range::H24, Range::D7];

    /// The `from=` value Grafana expects (`to` is always `now`).
    pub fn start_param(self) -> &'static str {
        match self {
            Range::M15 => "now-15m",
            Range::H1 => "now-1h",
            Range::H6 => "now-6h",
            Range::H24 => "now-24h",
            Range::D7 => "now-7d",
        }
    }

    /// A short stable key, for `<option>` values and the remembered choice.
    pub fn key(self) -> &'static str {
        match self {
            Range::M15 => "15m",
            Range::H1 => "1h",
            Range::H6 => "6h",
            Range::H24 => "24h",
            Range::D7 => "7d",
        }
    }

    /// The range with this [`Range::key`].
    pub fn parse(key: &str) -> Option<Range> {
        Range::ALL.into_iter().find(|range| range.key() == key)
    }

    /// What the picker shows.
    pub fn label(self) -> &'static str {
        match self {
            Range::M15 => "Last 15 minutes",
            Range::H1 => "Last hour",
            Range::H6 => "Last 6 hours",
            Range::H24 => "Last 24 hours",
            Range::D7 => "Last 7 days",
        }
    }
}

/// How often Grafana refreshes the embedded dashboard. The default matches the overview
/// dashboard's own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Refresh {
    Off,
    #[default]
    S10,
    S30,
    M1,
}

impl Refresh {
    /// Every interval, in picker order.
    pub const ALL: [Refresh; 4] = [Refresh::Off, Refresh::S10, Refresh::S30, Refresh::M1];

    /// The `refresh=` value, `None` when the key is left out (no automatic refresh).
    pub fn param(self) -> Option<&'static str> {
        match self {
            Refresh::Off => None,
            Refresh::S10 => Some("10s"),
            Refresh::S30 => Some("30s"),
            Refresh::M1 => Some("1m"),
        }
    }

    /// A short stable key, for `<option>` values and the remembered choice.
    pub fn key(self) -> &'static str {
        match self {
            Refresh::Off => "off",
            Refresh::S10 => "10s",
            Refresh::S30 => "30s",
            Refresh::M1 => "1m",
        }
    }

    /// The interval with this [`Refresh::key`].
    pub fn parse(key: &str) -> Option<Refresh> {
        Refresh::ALL
            .into_iter()
            .find(|refresh| refresh.key() == key)
    }

    /// What the picker shows.
    pub fn label(self) -> &'static str {
        match self {
            Refresh::Off => "No auto refresh",
            Refresh::S10 => "Refresh every 10 s",
            Refresh::S30 => "Refresh every 30 s",
            Refresh::M1 => "Refresh every minute",
        }
    }
}

/// The query keys the tab sets itself; the user's own values for them are dropped first.
const CONTROLLED_KEYS: [&str; 5] = ["kiosk", "from", "to", "refresh", "theme"];

/// A base URL split into its origin (scheme and authority), path and query. The fragment is
/// dropped and surrounding whitespace ignored.
struct Parts<'a> {
    origin: &'a str,
    path: &'a str,
    query: &'a str,
}

fn split(base: &str) -> Parts<'_> {
    let base = base.trim();
    let base = base.split_once('#').map_or(base, |(before, _)| before);
    let (before_query, query) = base.split_once('?').unwrap_or((base, ""));
    let authority_start = before_query.find("://").map_or(0, |at| at + 3);
    let path_start = before_query[authority_start..]
        .find('/')
        .map_or(before_query.len(), |at| authority_start + at);
    Parts {
        origin: &before_query[..path_start],
        path: &before_query[path_start..],
        query,
    }
}

/// When `path` names a dashboard (`[<prefix>]/d/<uid>[/<slug>]`): the prefix before `/d/` and
/// the uid after it (possibly empty for a bare `/d/`).
fn dashboard_in(path: &str) -> Option<(String, &str)> {
    let segments: Vec<&str> = path.split('/').collect();
    let marker = (0..segments.len().saturating_sub(1)).find(|&at| segments[at] == "d")?;
    Some((segments[..marker].join("/"), segments[marker + 1]))
}

/// The URL the iframe loads: `base` with the dashboard `uid`, in kiosk mode, over `range`,
/// refreshing every `refresh`, in the dark or light theme. See the module docs for the rules.
pub fn embed_url(base: &str, uid: &str, range: Range, refresh: Refresh, dark: bool) -> String {
    let parts = split(base);
    let prefix = match dashboard_in(parts.path) {
        Some((prefix, _)) => prefix,
        None => parts.path.trim_end_matches('/').to_owned(),
    };
    let mut query: Vec<String> = parts
        .query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(key, _)| key);
            !CONTROLLED_KEYS.contains(&key)
        })
        .map(str::to_owned)
        .collect();
    query.push("kiosk".to_owned());
    query.push(format!("from={}", range.start_param()));
    query.push("to=now".to_owned());
    if let Some(refresh) = refresh.param() {
        query.push(format!("refresh={refresh}"));
    }
    query.push(format!("theme={}", if dark { "dark" } else { "light" }));
    format!("{}{prefix}/d/{uid}?{}", parts.origin, query.join("&"))
}

/// The dashboard uid in `base`, when its path has a `/d/<uid>` segment: the picker starts on
/// the dashboard the user's URL names.
pub fn uid_in(base: &str) -> Option<String> {
    dashboard_in(split(base).path)
        .map(|(_, uid)| uid)
        .filter(|uid| !uid.is_empty())
        .map(str::to_owned)
}

/// The dashboard `base` names when it is not one of the shipped ones, so the picker can list
/// it too instead of hiding the user's own dashboard.
pub fn custom_uid(base: &str) -> Option<String> {
    uid_in(base).filter(|uid| !DASHBOARDS.iter().any(|dashboard| dashboard.uid == uid))
}

/// The host and port of `base`, such as `127.0.0.1:3000`, for the toolbar.
pub fn host_in(base: &str) -> String {
    let origin = split(base).origin;
    origin
        .find("://")
        .map_or(origin, |at| &origin[at + 3..])
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAIL: &str = "kiosk&from=now-1h&to=now&refresh=10s&theme=dark";

    fn default_url(base: &str, uid: &str) -> String {
        embed_url(base, uid, Range::H1, Refresh::S10, true)
    }

    #[test]
    fn the_fourteen_shipped_dashboards_in_order() {
        assert_eq!(DASHBOARDS.len(), 14);
        let uids: Vec<&str> = DASHBOARDS.iter().map(|d| d.uid).collect();
        assert_eq!(
            uids,
            [
                "zebra-overview",
                "Sl3h19Gnk",
                "o4LmN_OMk",
                "rO_Cl5tGz",
                "UXVRR1v7z",
                "S29TgUH7k",
                "320aS_dMk",
                "YQ3yxiVnk",
                "wVXGE6v7z",
                "IhbO11wGk",
                "zebra-value-pools",
                "zebra-rpc-metrics",
                "zebra-rpc-tracing",
                "zebra-rocksdb",
            ]
        );
        assert!(DASHBOARDS.iter().any(|d| d.uid == DEFAULT_UID));
        for dashboard in DASHBOARDS {
            assert!(
                !dashboard.title.is_empty(),
                "{} has no title",
                dashboard.uid
            );
        }
    }

    #[test]
    fn an_origin_gets_the_dashboard_appended() {
        assert_eq!(
            default_url("http://127.0.0.1:3000", "zebra-overview"),
            format!("http://127.0.0.1:3000/d/zebra-overview?{TAIL}")
        );
    }

    #[test]
    fn trailing_slashes_and_whitespace_are_tolerated() {
        let expected = format!("http://127.0.0.1:3000/d/zebra-overview?{TAIL}");
        assert_eq!(
            default_url("http://127.0.0.1:3000/", "zebra-overview"),
            expected
        );
        assert_eq!(
            default_url(" http://127.0.0.1:3000// ", "zebra-overview"),
            expected
        );
    }

    #[test]
    fn a_path_prefix_is_kept() {
        let expected = format!("http://localhost:3000/grafana/d/Sl3h19Gnk?{TAIL}");
        assert_eq!(
            default_url("http://localhost:3000/grafana", "Sl3h19Gnk"),
            expected
        );
        assert_eq!(
            default_url("http://localhost:3000/grafana/", "Sl3h19Gnk"),
            expected
        );
    }

    #[test]
    fn a_full_dashboard_url_gets_its_uid_replaced_and_slug_dropped() {
        assert_eq!(
            default_url(
                "http://127.0.0.1:3000/d/zebra-overview/zebra-overview?orgId=1&kiosk",
                "Sl3h19Gnk"
            ),
            format!("http://127.0.0.1:3000/d/Sl3h19Gnk?orgId=1&{TAIL}")
        );
    }

    #[test]
    fn a_prefixed_full_url_keeps_the_prefix_and_other_query_keys() {
        assert_eq!(
            embed_url(
                "http://localhost:3000/grafana/d/abc/some-slug?var-net=main&from=now-2d&to=now-1d&refresh=5s&theme=light&orgId=2#panel-3",
                "wVXGE6v7z",
                Range::D7,
                Refresh::M1,
                false
            ),
            "http://localhost:3000/grafana/d/wVXGE6v7z?var-net=main&orgId=2&kiosk&from=now-7d&to=now&refresh=1m&theme=light"
        );
    }

    #[test]
    fn controlled_keys_are_dropped_with_or_without_values() {
        assert_eq!(
            default_url(
                "http://127.0.0.1:3000/?kiosk&theme&from=x&to&refresh=1d&",
                "IhbO11wGk"
            ),
            format!("http://127.0.0.1:3000/d/IhbO11wGk?{TAIL}")
        );
    }

    #[test]
    fn refresh_off_omits_the_refresh_key() {
        assert_eq!(
            embed_url(
                "http://127.0.0.1:3000",
                "zebra-overview",
                Range::M15,
                Refresh::Off,
                true
            ),
            "http://127.0.0.1:3000/d/zebra-overview?kiosk&from=now-15m&to=now&theme=dark"
        );
    }

    #[test]
    fn every_range_and_refresh_lands_in_the_url() {
        for range in Range::ALL {
            let url = embed_url("http://127.0.0.1:3000", "x", range, Refresh::Off, true);
            assert!(
                url.contains(&format!("from={}&to=now", range.start_param())),
                "{url}"
            );
        }
        for refresh in Refresh::ALL {
            let url = embed_url("http://127.0.0.1:3000", "x", Range::H1, refresh, true);
            match refresh.param() {
                Some(param) => assert!(url.contains(&format!("&refresh={param}&")), "{url}"),
                None => assert!(!url.contains("refresh"), "{url}"),
            }
        }
    }

    #[test]
    fn the_theme_follows_the_flag() {
        let dark = embed_url("http://127.0.0.1:3000", "x", Range::H1, Refresh::Off, true);
        let light = embed_url("http://127.0.0.1:3000", "x", Range::H1, Refresh::Off, false);
        assert!(dark.ends_with("&theme=dark"));
        assert!(light.ends_with("&theme=light"));
    }

    #[test]
    fn a_fragment_is_dropped() {
        let url = default_url("http://127.0.0.1:3000/#/something", "x");
        assert!(!url.contains('#'), "{url}");
        assert_eq!(url, format!("http://127.0.0.1:3000/d/x?{TAIL}"));
    }

    #[test]
    fn a_bare_dashboard_path_is_replaced() {
        assert_eq!(
            default_url("http://127.0.0.1:3000/d/", "x"),
            format!("http://127.0.0.1:3000/d/x?{TAIL}")
        );
    }

    #[test]
    fn uid_in_finds_the_dashboard_segment_only() {
        assert_eq!(uid_in("http://127.0.0.1:3000"), None);
        assert_eq!(uid_in("http://127.0.0.1:3000/"), None);
        assert_eq!(uid_in("http://127.0.0.1:3000/d/"), None);
        assert_eq!(uid_in("http://127.0.0.1:3000/dashboards"), None);
        assert_eq!(uid_in("http://127.0.0.1:3000/d/abc"), Some("abc".into()));
        assert_eq!(
            uid_in("http://127.0.0.1:3000/d/abc/slug?orgId=1#x"),
            Some("abc".into())
        );
        assert_eq!(
            uid_in("http://localhost:3000/grafana/d/zebra-rocksdb/rocksdb"),
            Some("zebra-rocksdb".into())
        );
    }

    #[test]
    fn custom_uid_is_only_for_dashboards_not_shipped() {
        assert_eq!(custom_uid("http://127.0.0.1:3000"), None);
        assert_eq!(custom_uid("http://127.0.0.1:3000/d/zebra-overview"), None);
        assert_eq!(
            custom_uid("http://127.0.0.1:3000/d/my-own/x"),
            Some("my-own".into())
        );
    }

    #[test]
    fn host_in_strips_scheme_path_and_query() {
        assert_eq!(host_in("http://127.0.0.1:3000"), "127.0.0.1:3000");
        assert_eq!(
            host_in("http://127.0.0.1:3000/d/x?orgId=1"),
            "127.0.0.1:3000"
        );
        assert_eq!(host_in("http://localhost/grafana/"), "localhost");
        assert_eq!(host_in("  http://LOCALHOST:3000  "), "LOCALHOST:3000");
    }

    #[test]
    fn range_and_refresh_keys_round_trip() {
        for range in Range::ALL {
            assert_eq!(Range::parse(range.key()), Some(range));
            assert!(!range.label().is_empty());
        }
        for refresh in Refresh::ALL {
            assert_eq!(Refresh::parse(refresh.key()), Some(refresh));
            assert!(!refresh.label().is_empty());
        }
        assert_eq!(Range::parse("bogus"), None);
        assert_eq!(Refresh::parse(""), None);
        assert_eq!(Range::default(), Range::H1);
        assert_eq!(Refresh::default(), Refresh::S10);
    }
}
