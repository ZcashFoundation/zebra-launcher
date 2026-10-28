//! The activity row (specification 3.8 to 3.10): Recent problems and This run, whose views are
//! in [`crate::components::log_insights`], and the Peers card. The sentences and the event
//! merge are pure functions, tested natively.

use std::time::{Duration, SystemTime};

use leptos::prelude::*;

use crate::{
    app::Tab,
    components::log_insights::{RecentProblems, ThisRun},
    format::rough_duration,
    insights::RunEvent,
    stats_history::Sample,
};

use super::{Dash, Icon};

/// How many problems the Recent problems card lists.
pub const PROBLEMS_SHOWN: usize = 5;

/// How many events This run lists.
pub const EVENTS_SHOWN: usize = 8;

/// From this long without a peer, the caption counts the time.
pub const NO_PEERS_COUNTED_FROM: Duration = Duration::from_secs(60);

/// The newest `limit` of the log's events and the session's status transitions, newest
/// first; equal times keep the log's order.
pub fn merge_events(
    mut from_log: Vec<RunEvent>,
    session: &[RunEvent],
    limit: usize,
) -> Vec<RunEvent> {
    from_log.extend(session.iter().cloned());
    from_log.sort_by_key(|event| std::cmp::Reverse(event.at));
    from_log.truncate(limit);
    from_log
}

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

/// The Peers card's caption: `12 peers · 11 ready · 1 on an obsolete version`, `No peers for
/// 3 min`, or the last count once zebrad stopped.
pub fn peers_caption(
    peers: Option<u32>,
    ready: Option<u64>,
    obsolete: Option<u64>,
    no_peers_for: Option<Duration>,
    running: bool,
) -> (Icon, String) {
    let Some(peers) = peers else {
        return (Icon::Clock, "Waiting for zebrad's RPC…".to_owned());
    };
    if !running {
        return (
            Icon::Clock,
            format!("{} when zebrad stopped", peer_count(peers)),
        );
    }
    if peers == 0 {
        let text = match no_peers_for {
            Some(for_) if for_ >= NO_PEERS_COUNTED_FROM => {
                format!("No peers for {}", rough_duration(for_))
            }
            _ => "No peers connected".to_owned(),
        };
        return (Icon::Warning, text);
    }
    let mut parts = vec![peer_count(peers)];
    if let Some(ready) = ready {
        parts.push(format!("{ready} ready"));
    }
    match obsolete {
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
    let events = Signal::derive(move || {
        let from_log = state
            .insights
            .with(|insights| insights.events(EVENTS_SHOWN));
        state
            .session_events
            .with(|session| merge_events(from_log, session, EVENTS_SHOWN))
    });

    let caption = Memo::new(move |_| {
        let now = state.now.get();
        let peers = dash.latest.get().map(|sample| sample.peers);
        let facts = dash.metrics.get();
        let without = state
            .history
            .with(|history| no_peers_for(history.window(None, now), now));
        peers_caption(
            peers,
            facts.and_then(|facts| facts.ready_peers),
            facts.and_then(|facts| facts.obsolete_peers),
            without,
            dash.running(),
        )
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
        let tone = match icon {
            Icon::Check => "insight-tone-good",
            Icon::Clock => "insight-tone-neutral",
            Icon::Warning => "insight-tone-warning",
            Icon::Cross => "insight-tone-critical",
        };
        let muted = if dash.running() { "" } else { " muted" };
        format!("dash-peers-caption {tone}{muted}")
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

    use crate::insights::{EventKind, Tone};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn event(secs: u64, text: &str) -> RunEvent {
        RunEvent {
            at: at(secs),
            kind: EventKind::Milestone,
            text: text.to_owned(),
            tone: Tone::Neutral,
        }
    }

    fn sample(secs: u64, peers: u32) -> Sample {
        Sample {
            at: at(secs),
            height: 100,
            estimated_height: 200,
            peers,
            mempool_transactions: 0,
            verification_progress: 0.5,
        }
    }

    #[test]
    fn events_merge_newest_first() {
        let from_log = vec![event(30, "c"), event(10, "a")];
        let session = [event(20, "b"), event(40, "d"), event(30, "c2")];
        let merged = merge_events(from_log.clone(), &session, 8);
        let texts: Vec<_> = merged.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["d", "c", "c2", "b", "a"]);
        let few = merge_events(from_log, &session, 2);
        let texts: Vec<_> = few.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["d", "c"]);
        assert!(merge_events(Vec::new(), &[], 8).is_empty());
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
        assert_eq!(
            peers_caption(None, None, None, None, true),
            (Icon::Clock, "Waiting for zebrad's RPC…".into())
        );
        assert_eq!(
            peers_caption(Some(12), Some(11), Some(1), None, true),
            (
                Icon::Check,
                "12 peers · 11 ready · 1 on an obsolete version".into()
            )
        );
        assert_eq!(
            peers_caption(Some(12), None, Some(2), None, true),
            (Icon::Check, "12 peers · 2 on obsolete versions".into())
        );
        assert_eq!(
            peers_caption(Some(1), Some(1), Some(0), None, true),
            (Icon::Check, "1 peer · 1 ready".into())
        );
        assert_eq!(
            peers_caption(Some(0), Some(0), None, Some(Duration::from_secs(180)), true),
            (Icon::Warning, "No peers for 3 min".into())
        );
        assert_eq!(
            peers_caption(Some(0), None, None, Some(Duration::from_secs(20)), true),
            (Icon::Warning, "No peers connected".into())
        );
        assert_eq!(
            peers_caption(Some(9), Some(8), None, None, false),
            (Icon::Clock, "9 peers when zebrad stopped".into())
        );
        assert_eq!(
            peers_footer(Some(3)),
            Some("handshake failures this run: 3".into())
        );
        assert_eq!(peers_footer(None), None);
    }
}
