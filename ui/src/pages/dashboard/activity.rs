//! The activity row (specification 3.8 to 3.10): Recent problems and This run, whose views are
//! in [`crate::components::log_insights`], and the Peers card with its caption, its table of
//! connected peers and its footer. The sentences and the table's rows are pure functions,
//! tested natively.

use std::{
    cmp::Reverse,
    time::{Duration, SystemTime},
};

use launcher_types::NodeDetails;
use leptos::prelude::*;

use crate::{
    app::Tab,
    components::log_insights::{RecentProblems, ThisRun},
    format::{relative, rough_duration},
    stats_history::{Sample, is_connected},
};

use super::{Dash, Icon};

/// How many peers the table lists until "Show all".
pub const PEER_ROWS_SHOWN: usize = 8;

/// An address longer than this is cut from the middle (`[2001:db8:1::77]:8233` fits).
pub const ADDR_CHARS: usize = 22;

/// How many problems the Recent problems card lists.
pub const PROBLEMS_SHOWN: usize = 5;

/// How many events This run lists.
pub const EVENTS_SHOWN: usize = 8;

/// From this long without a peer, the caption counts the time.
pub const NO_PEERS_COUNTED_FROM: Duration = Duration::from_secs(60);

/// No inbound peer in any list of this long, at the tip, means the P2P port is not reachable
/// from outside: the caption says so, as information, not as a warning.
pub const NO_INBOUND_OVER: Duration = Duration::from_secs(15 * 60);

/// How long the samples have shown no peer: since the last sample with one, or since the first
/// sample when none had one. `None` while the newest sample has peers, or without samples.
pub fn no_peers_for(samples: &[Sample], now: SystemTime) -> Option<Duration> {
    let newest = samples.last()?;
    if newest.peers > 0 {
        return None;
    }
    let since = samples
        .iter()
        .rev()
        .find(|sample| sample.peers > 0)
        .map_or(samples[0].at, |sample| sample.at);
    Some(now.duration_since(since).unwrap_or(Duration::ZERO))
}

fn peer_count(peers: u32) -> String {
    match peers {
        1 => "1 peer".to_owned(),
        n => format!("{n} peers"),
    }
}

/// What the Peers card's caption is built from.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PeersFacts {
    /// The newest count; `None` before the first RPC answer.
    pub peers: Option<u32>,
    /// `(inbound, outbound)` from the peer list of the same poll.
    pub split: Option<(u32, u32)>,
    pub ready: Option<u64>,
    pub obsolete: Option<u64>,
    /// How long the samples have shown no peer ([`no_peers_for`]).
    pub no_peers_for: Option<Duration>,
    /// At the tip with no inbound peer in any list of the last [`NO_INBOUND_OVER`].
    pub no_inbound: bool,
    pub running: bool,
}

/// The Peers card's caption: `12 peers · 8 outbound · 4 inbound · 11 ready · 1 on an obsolete
/// version`, `No peers for 3 min`, `No inbound connections (…)` as information, or the last
/// count once zebrad stopped.
pub fn peers_caption(facts: &PeersFacts) -> (Icon, String) {
    let Some(peers) = facts.peers else {
        return (Icon::Clock, "Waiting for zebrad's RPC…".to_owned());
    };
    if !facts.running {
        return (
            Icon::Clock,
            format!("{} when zebrad stopped", peer_count(peers)),
        );
    }
    if peers == 0 {
        let text = match facts.no_peers_for {
            Some(for_) if for_ >= NO_PEERS_COUNTED_FROM => {
                format!("No peers for {}", rough_duration(for_))
            }
            _ => "No peers connected".to_owned(),
        };
        return (Icon::Warning, text);
    }
    if facts.no_inbound {
        return (
            Icon::Info,
            "No inbound connections (the P2P port is not reachable from outside; normal behind a router)".to_owned(),
        );
    }
    let mut parts = vec![peer_count(peers)];
    if let Some((inbound, outbound)) = facts.split {
        parts.push(format!("{outbound} outbound"));
        parts.push(format!("{inbound} inbound"));
    }
    if let Some(ready) = facts.ready {
        parts.push(format!("{ready} ready"));
    }
    match facts.obsolete {
        Some(1) => parts.push("1 on an obsolete version".to_owned()),
        Some(n) if n > 1 => parts.push(format!("{n} on obsolete versions")),
        _ => {}
    }
    (Icon::Check, parts.join(" · "))
}

/// The Peers card's footer, from zebrad's counters for this run: `handshake failures this
/// run: 3 · obsolete peers refused: 1`; nothing without either counter.
pub fn peers_footer(
    handshake_failures: Option<u64>,
    obsolete_refused: Option<u64>,
) -> Option<String> {
    let mut parts = Vec::with_capacity(2);
    if let Some(failures) = handshake_failures {
        parts.push(format!("handshake failures this run: {failures}"));
    }
    if let Some(refused) = obsolete_refused {
        parts.push(format!("obsolete peers refused: {refused}"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// `text` cut to `max` characters from the middle: `[2001:db8:…77]:8233`.
pub fn truncate_middle(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max || max < 3 {
        return text.to_owned();
    }
    let head = max / 2;
    let tail = max - 1 - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(count - tail).collect();
    format!("{start}…{end}")
}

/// A round-trip time: `34 ms`, `1.2 s`, or a dash.
pub fn ping_text(secs: Option<f64>) -> String {
    match secs {
        Some(secs) if secs.is_finite() && secs >= 0.0 => {
            if secs < 1.0 {
                format!("{} ms", (secs * 1000.0).round() as u64)
            } else {
                format!("{secs:.1} s")
            }
        }
        _ => "—".to_owned(),
    }
}

/// One row of the peer table. The row is its own key in the list: a change in any cell
/// replaces it, so the cells never lag behind the order.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PeerRow {
    pub addr: String,
    /// The address cut to [`ADDR_CHARS`]; the full one is the cell's title.
    pub short_addr: String,
    pub inbound: bool,
    /// The user agent without its slashes: `Zebra 7.0.0`; the raw one is the cell's title.
    pub agent: String,
    pub raw_agent: String,
    pub version: String,
    pub ping: String,
    pub last_seen: Option<SystemTime>,
}

/// `/Zebra:7.0.0/` as `Zebra 7.0.0`.
pub fn agent_text(user_agent: &str) -> String {
    user_agent.trim_matches('/').replace(':', " ")
}

impl PeerRow {
    /// The agent cell's title: the user agent as zebrad reports it, with the protocol
    /// version, which the narrow table leaves out.
    pub fn agent_title(&self) -> String {
        format!("{} · protocol version {}", self.raw_agent, self.version)
    }

    pub fn direction(&self) -> &'static str {
        if self.inbound { "In" } else { "Out" }
    }
}

/// The connected peers of the list, the most recently heard from first (those never heard
/// from last), then by address.
pub fn peer_rows(details: &NodeDetails) -> Vec<PeerRow> {
    let mut rows: Vec<PeerRow> = details
        .peers
        .iter()
        .filter(|peer| is_connected(peer))
        .map(|peer| PeerRow {
            addr: peer.addr.clone(),
            short_addr: truncate_middle(&peer.addr, ADDR_CHARS),
            inbound: peer.inbound,
            agent: peer
                .user_agent
                .as_deref()
                .map_or_else(|| "—".to_owned(), agent_text),
            raw_agent: peer.user_agent.clone().unwrap_or_else(|| "—".to_owned()),
            version: peer
                .protocol_version
                .map_or_else(|| "—".to_owned(), |version| version.to_string()),
            ping: ping_text(peer.ping_secs),
            last_seen: peer.last_seen,
        })
        .collect();
    rows.sort_by(|a, b| {
        Reverse(a.last_seen)
            .cmp(&Reverse(b.last_seen))
            .then_with(|| a.addr.cmp(&b.addr))
    });
    rows
}

/// The "Show all 12" link's text when the list is longer than [`PEER_ROWS_SHOWN`].
pub fn show_all_text(total: usize) -> Option<String> {
    (total > PEER_ROWS_SHOWN).then(|| format!("Show all {total}"))
}

/// The three activity cards.
#[component]
pub(super) fn ActivityRow(dash: Dash) -> impl IntoView {
    let state = dash.state;
    // A problem picked here is shown filtered on the Logs page.
    let show_in_logs = Callback::new(move |filter: String| {
        state.log_filter_request.set(Some(filter));
        state.tab.set(Tab::Logs);
    });
    let open_logs = Callback::new(move |()| state.tab.set(Tab::Logs));
    // The log's events and the session's, merged by `Insights::events`.
    let events = Signal::derive(move || {
        state
            .insights
            .with(|insights| insights.events(EVENTS_SHOWN))
    });

    let caption = Memo::new(move |_| {
        let now = state.now.get();
        let latest = dash.latest.get();
        let metrics = dash.metrics.get();
        let (no_peers_for, no_inbound) = state.history.with(|history| {
            (
                no_peers_for(history.window(None, now), now),
                latest.is_some_and(|sample| sample.at_tip())
                    && history.no_inbound_over(Some(NO_INBOUND_OVER), now),
            )
        });
        peers_caption(&PeersFacts {
            peers: latest.map(|sample| sample.peers),
            split: latest.and_then(|sample| sample.peer_split()),
            ready: metrics.and_then(|facts| facts.ready_peers),
            obsolete: metrics.and_then(|facts| facts.obsolete_peers),
            no_peers_for,
            no_inbound,
            running: dash.running(),
        })
    });
    let footer = Memo::new(move |_| {
        let facts = dash.metrics.get();
        peers_footer(
            facts.and_then(|facts| facts.handshake_failures),
            facts.and_then(|facts| facts.obsolete_refused),
        )
    });
    let caption_class = move || {
        let (icon, _) = caption.get();
        let muted = if dash.running() { "" } else { " muted" };
        format!("dash-peers-caption {}{muted}", icon.tone_class())
    };

    // The table: the connected peers of the newest list, eight by last seen until "Show all";
    // under 1100 px the card is full width and collapsed to its caption behind "Show peers".
    let rows = Memo::new(move |_| {
        state
            .details
            .with(|details| details.as_ref().map(peer_rows).unwrap_or_default())
    });
    let all = RwSignal::new(false);
    let open = RwSignal::new(false);
    let shown = move || {
        let rows = rows.get();
        let count = if all.get() {
            rows.len()
        } else {
            rows.len().min(PEER_ROWS_SHOWN)
        };
        rows.into_iter().take(count).collect::<Vec<_>>()
    };
    let has_rows = move || rows.with(|rows| !rows.is_empty());
    let peer_row = move |row: PeerRow| {
        let last_seen = row.last_seen;
        view! {
            <tr>
                <td class="dash-peer-addr" title=row.addr.clone()>{row.short_addr.clone()}</td>
                <td>{row.direction()}</td>
                <td class="dash-peer-agent" title=row.agent_title()>{row.agent.clone()}</td>
                <td class="dash-peer-version">{row.version.clone()}</td>
                <td class="num">{row.ping.clone()}</td>
                <td>
                    {move || {
                        last_seen.map_or_else(|| "—".to_owned(), |at| relative(at, state.now.get()))
                    }}
                </td>
            </tr>
        }
    };

    view! {
        <section class="dash-activity" aria-label="Activity">
            <RecentProblems limit=PROBLEMS_SHOWN on_pick=show_in_logs on_show_all=open_logs />
            <ThisRun events on_show_all=open_logs />
            <div class="card insight-card dash-peers" class:dash-peers-open=move || open.get()>
                <div class="insight-head">
                    <h3>"Peers"</h3>
                    <button
                        class="link dash-peers-toggle"
                        type="button"
                        aria-expanded=move || open.get().to_string()
                        on:click=move |_| open.update(|open| *open = !*open)
                    >
                        {move || if open.get() { "Hide peers" } else { "Show peers" }}
                    </button>
                </div>
                <p class=caption_class>
                    {move || caption.get().0.view()}
                    <span>{move || caption.get().1}</span>
                </p>
                <Show when=has_rows>
                    <div class="dash-peers-table insight-scroll">
                        <table class="chart-table dash-peer-list" aria-label="Connected peers">
                            <thead>
                                <tr>
                                    <th scope="col">"Address"</th>
                                    <th scope="col">"Direction"</th>
                                    <th scope="col">"Agent"</th>
                                    <th scope="col" class="dash-peer-version">"Version"</th>
                                    <th scope="col" class="num">"Ping"</th>
                                    <th scope="col">"Last seen"</th>
                                </tr>
                            </thead>
                            <tbody>
                                <For each=shown key=|row| row.clone() children=peer_row />
                            </tbody>
                        </table>
                    </div>
                    {move || {
                        rows.with(|rows| show_all_text(rows.len())).map(|text| {
                            view! {
                                <p class="insight-footer dash-peers-all">
                                    <button class="link" type="button" on:click=move |_| all.update(|all| *all = !*all)>
                                        {move || if all.get() { "Show fewer".to_owned() } else { text.clone() }}
                                    </button>
                                </p>
                            }
                        })
                    }}
                </Show>
                <p class="muted insight-foot">{move || footer.get().unwrap_or_default()}</p>
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use launcher_types::PeerInfo;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sample(secs: u64, peers: u32) -> Sample {
        Sample {
            at: at(secs),
            height: 100,
            estimated_height: 200,
            peers,
            mempool_transactions: 0,
            verification_progress: 0.5,
            peers_inbound: None,
            peers_outbound: None,
        }
    }

    #[test]
    fn time_without_peers() {
        let now = at(300);
        assert_eq!(no_peers_for(&[], now), None);
        assert_eq!(no_peers_for(&[sample(0, 3)], now), None);
        assert_eq!(
            no_peers_for(&[sample(0, 3), sample(100, 0), sample(200, 0)], now),
            Some(Duration::from_secs(300))
        );
        assert_eq!(
            no_peers_for(&[sample(0, 0), sample(100, 0)], now),
            Some(Duration::from_secs(300)),
            "never had a peer: since the first sample"
        );
        assert_eq!(no_peers_for(&[sample(0, 0), sample(100, 2)], now), None);
    }

    #[test]
    fn captions() {
        let running = PeersFacts {
            running: true,
            ..PeersFacts::default()
        };
        assert_eq!(
            peers_caption(&running),
            (Icon::Clock, "Waiting for zebrad's RPC…".into())
        );
        assert_eq!(
            peers_caption(&PeersFacts {
                peers: Some(12),
                split: Some((4, 8)),
                ready: Some(11),
                obsolete: Some(1),
                ..running
            }),
            (
                Icon::Check,
                "12 peers · 8 outbound · 4 inbound · 11 ready · 1 on an obsolete version".into()
            )
        );
        assert_eq!(
            peers_caption(&PeersFacts {
                peers: Some(12),
                obsolete: Some(2),
                ..running
            }),
            (Icon::Check, "12 peers · 2 on obsolete versions".into())
        );
        assert_eq!(
            peers_caption(&PeersFacts {
                peers: Some(1),
                ready: Some(1),
                obsolete: Some(0),
                ..running
            }),
            (Icon::Check, "1 peer · 1 ready".into())
        );
        assert_eq!(
            peers_caption(&PeersFacts {
                peers: Some(0),
                ready: Some(0),
                no_peers_for: Some(Duration::from_secs(180)),
                ..running
            }),
            (Icon::Warning, "No peers for 3 min".into())
        );
        assert_eq!(
            peers_caption(&PeersFacts {
                peers: Some(0),
                no_peers_for: Some(Duration::from_secs(20)),
                ..running
            }),
            (Icon::Warning, "No peers connected".into())
        );
        // No inbound peer for a while at the tip: an explanation, before the counts.
        let (icon, text) = peers_caption(&PeersFacts {
            peers: Some(8),
            split: Some((0, 8)),
            no_inbound: true,
            ..running
        });
        assert_eq!(icon, Icon::Info);
        assert!(text.starts_with("No inbound connections"), "{text}");
        assert_eq!(
            peers_caption(&PeersFacts {
                peers: Some(9),
                ready: Some(8),
                running: false,
                ..PeersFacts::default()
            }),
            (Icon::Clock, "9 peers when zebrad stopped".into())
        );
        assert_eq!(
            peers_footer(Some(3), Some(1)),
            Some("handshake failures this run: 3 · obsolete peers refused: 1".into())
        );
        assert_eq!(
            peers_footer(Some(3), None),
            Some("handshake failures this run: 3".into())
        );
        assert_eq!(
            peers_footer(None, Some(0)),
            Some("obsolete peers refused: 0".into())
        );
        assert_eq!(peers_footer(None, None), None);
    }

    #[test]
    fn peer_table_rows() {
        assert_eq!(
            truncate_middle("203.0.113.17:8233", ADDR_CHARS),
            "203.0.113.17:8233"
        );
        assert_eq!(
            truncate_middle("[2001:db8:1::77]:8233", ADDR_CHARS),
            "[2001:db8:1::77]:8233"
        );
        assert_eq!(
            truncate_middle("[2001:db8:85a3:8d3:1319:8a2e:370:7348]:8233", ADDR_CHARS),
            "[2001:db8:8…7348]:8233"
        );
        assert_eq!(truncate_middle("abcdef", 5), "ab…ef");
        assert_eq!(truncate_middle("abcdef", 2), "abcdef", "too short to cut");
        assert_eq!(ping_text(Some(0.0341)), "34 ms");
        assert_eq!(ping_text(Some(1.25)), "1.2 s");
        assert_eq!(ping_text(Some(-1.0)), "—");
        assert_eq!(ping_text(None), "—");
        assert_eq!(show_all_text(8), None);
        assert_eq!(show_all_text(12), Some("Show all 12".into()));

        let peer = |addr: &str, inbound: bool, state: Option<&str>, seen: Option<u64>| PeerInfo {
            addr: addr.into(),
            inbound,
            user_agent: Some("/Zebra:7.0.0/".into()),
            protocol_version: Some(170_120),
            state: state.map(str::to_owned),
            last_seen: seen.map(at),
            ping_secs: Some(0.05),
        };
        let details = NodeDetails {
            peers: vec![
                peer("192.0.2.1:8233", false, Some("connected"), Some(10)),
                peer("192.0.2.2:8233", true, None, Some(30)),
                peer("192.0.2.3:8233", false, Some("failed"), Some(40)),
                peer("192.0.2.4:8233", false, Some("Responded"), None),
                peer("192.0.2.0:8233", true, None, Some(30)),
            ],
            best_block_hash: None,
            headers: None,
            difficulty: None,
            size_on_disk: None,
            chain_supply_zec: None,
            value_pools: Vec::new(),
            active_upgrade: None,
            subversion: None,
            protocol_version: None,
            mempool_bytes: None,
            mempool_usage: None,
            updated_at: at(40),
        };
        let rows = peer_rows(&details);
        let order: Vec<_> = rows.iter().map(|row| row.addr.as_str()).collect();
        // The failed peer is not connected; equal times go by address; never seen is last.
        assert_eq!(
            order,
            vec![
                "192.0.2.0:8233",
                "192.0.2.2:8233",
                "192.0.2.1:8233",
                "192.0.2.4:8233"
            ]
        );
        assert_eq!(rows[0].direction(), "In");
        assert_eq!(rows[2].direction(), "Out");
        assert_eq!(rows[0].agent, "Zebra 7.0.0");
        assert_eq!(rows[0].raw_agent, "/Zebra:7.0.0/");
        assert_eq!(
            rows[0].agent_title(),
            "/Zebra:7.0.0/ · protocol version 170120"
        );
        assert_eq!(agent_text("/MagicBean:6.2.0/"), "MagicBean 6.2.0");
        assert_eq!(agent_text("—"), "—");
        assert_eq!(rows[0].version, "170120");
        assert_eq!(rows[0].ping, "50 ms");
        assert_eq!(rows[3].last_seen, None);
    }
}
