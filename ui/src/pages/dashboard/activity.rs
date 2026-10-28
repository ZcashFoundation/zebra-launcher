//! The activity row (specification 3.8 to 3.10): Recent problems and This run, whose views are
//! in [`crate::components::log_insights`], and the Peers card. The sentences are pure
//! functions, tested natively.

use std::time::{Duration, SystemTime};

use leptos::prelude::*;

use crate::{
    app::Tab,
    components::log_insights::{RecentProblems, ThisRun},
    format::rough_duration,
    stats_history::Sample,
};

use super::{Dash, Icon};

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

/// The Peers card's footer, from zebrad's counters for this run.
pub fn peers_footer(handshake_failures: Option<u64>) -> Option<String> {
    handshake_failures.map(|failures| format!("handshake failures this run: {failures}"))
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
        peers_footer(
            dash.metrics
                .get()
                .and_then(|facts| facts.handshake_failures),
        )
    });
    let caption_class = move || {
        let (icon, _) = caption.get();
        let muted = if dash.running() { "" } else { " muted" };
        format!("dash-peers-caption {}{muted}", icon.tone_class())
    };

    view! {
        <section class="dash-activity" aria-label="Activity">
            <RecentProblems limit=PROBLEMS_SHOWN on_pick=show_in_logs on_show_all=open_logs />
            <ThisRun events on_show_all=open_logs />
            <div class="card insight-card dash-peers">
                <div class="insight-head">
                    <h3>"Peers"</h3>
                </div>
                <p class=caption_class>
                    {move || caption.get().0.view()}
                    <span>{move || caption.get().1}</span>
                </p>
                <p class="muted insight-foot">{move || footer.get().unwrap_or_default()}</p>
            </div>
        </section>
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

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
            peers_footer(Some(3)),
            Some("handshake failures this run: 3".into())
        );
        assert_eq!(peers_footer(None), None);
    }
}
