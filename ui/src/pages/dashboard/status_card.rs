//! The status card: the health verdict with its actions on the left (specification 3.2) and
//! the sync progress hero on the right (3.3). The verdict itself comes from
//! [`crate::health::health_verdict`]; this card maps its level to an icon, a word and a colour
//! class, and its actions to the backend calls. The pure parts are tested natively.

use std::time::{Duration, SystemTime};

use launcher_types::OpenTarget;
use leptos::{prelude::*, task::spawn_local};

use crate::{
    api,
    app::Tab,
    charts::{hhmm, phase_note, sync_summary, tz_offset_min_at, widgets::Meter},
    clipboard::copy_to_clipboard,
    components::{reset_config::ResetConfigButton, transient::Transient, updates::CheckForUpdate},
    format::{relative, rough_duration, thousands},
    health::{Action, CHECKPOINT_NEAR_BLOCKS, Level, humantime_secs, summary_text},
    insights::SyncStage,
    stats_history::HERO_RATE_WINDOW,
};

use super::{Dash, Icon, with_health_input};

/// How long "Copied" stays beside the Copy summary button.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// The class that colours the card's edge and the chip's icon: `dash-level-` plus the
/// level's tone (`good`, `neutral`, `warning`, `serious`, `critical`).
pub fn level_class(level: Level) -> String {
    format!("dash-level-{}", level.tone())
}

pub fn level_icon(level: Level) -> Icon {
    match level {
        Level::Healthy => Icon::Check,
        Level::Neutral => Icon::Clock,
        Level::Attention | Level::Urgent => Icon::Warning,
        Level::Problem => Icon::Cross,
    }
}

/// What the chip says: the level's word (`Healthy`, `Attention`, `Problem`), or the headline
/// itself for a neutral verdict (`Syncing`, `Stopped`), which has no word of its own.
pub fn chip_word(level: Level, headline: &str) -> String {
    level
        .word()
        .map_or_else(|| headline.to_owned(), str::to_owned)
}

/// What follows the chip: the headline, unless the chip already is the headline.
pub fn chip_rest(level: Level, headline: &str) -> Option<String> {
    level.word().map(|_| headline.to_owned())
}

/// The verb in "Could not …" when an action fails.
pub fn action_verb(action: &Action) -> &'static str {
    match action {
        Action::Start | Action::StartNow => "start zebrad",
        Action::StartKillingStale => "stop the earlier zebrad and start",
        Action::Restart => "restart zebrad",
        Action::Install => "install the update",
        Action::CancelUpdate => "cancel the update",
        Action::OpenLogsFolder => "open the logs folder",
        Action::OpenReleases => "open the releases page",
        Action::ShowInLogs(_)
        | Action::OpenConfigure
        | Action::ResetConfig
        | Action::CheckForUpdates => "do that",
    }
}

/// `last block 5 s ago`: from the sample history, or, while the history saw no rise, from
/// zebrad's own words in its progress log (`5s`, `1m 2s`, stamped when it said so) aged to
/// now and written in the page's units.
pub fn last_block_text(
    change_at: Option<SystemTime>,
    zebrad: Option<(&str, SystemTime)>,
    now: SystemTime,
) -> Option<String> {
    if let Some(at) = change_at {
        return Some(format!("last block {}", relative(at, now)));
    }
    let (words, said_at) = zebrad?;
    let said = Duration::from_secs(humantime_secs(words)?);
    let since = said + now.duration_since(said_at).unwrap_or(Duration::ZERO);
    Some(format!("last block {} ago", rough_duration(since)))
}

/// The stage words zebrad's progress log adds to the hero line, when the report belongs to
/// this run. Syncing and at the tip are what the line already says; a slow or stalled stage is
/// the verdict's job, so the line carries no warning.
pub fn stage_prefix(
    stage: SyncStage,
    snapshot_at: SystemTime,
    started_at: Option<SystemTime>,
) -> Option<&'static str> {
    if !started_at.is_some_and(|started| snapshot_at >= started) {
        return None;
    }
    match stage {
        SyncStage::NearTip | SyncStage::WaitingForGenesis => Some(stage.stage_text()),
        SyncStage::Syncing
        | SyncStage::AtTip
        | SyncStage::Slow
        | SyncStage::Stalled
        | SyncStage::GenesisStalled => None,
    }
}

/// The hero's line: the stage words, the summary's own line and the last block's age, with
/// middle dots between them. A waiting line (no sample yet) stands alone.
pub fn hero_line(
    line: &str,
    waiting: bool,
    stage: Option<&str>,
    last_block: Option<&str>,
) -> String {
    if waiting {
        return line.to_owned();
    }
    let mut parts = Vec::with_capacity(3);
    parts.extend(stage);
    parts.push(line);
    parts.extend(last_block);
    parts.join(" · ")
}

/// The hero's muted second line: `Checkpoints · verified to 2,500,000` while the checkpoint
/// verifier is within [`CHECKPOINT_NEAR_BLOCKS`] of the height, else `Full validation since
/// 14:02` when the log said so, else nothing.
pub fn verification_note(
    checkpoint_height: Option<u64>,
    height: u64,
    full_since: Option<&str>,
) -> Option<String> {
    match checkpoint_height {
        Some(checkpoint) if height.saturating_sub(checkpoint) <= CHECKPOINT_NEAR_BLOCKS => Some(
            format!("Checkpoints · verified to {}", thousands(checkpoint)),
        ),
        _ => full_since.map(|since| format!("Full validation since {since}")),
    }
}

/// Runs an action: the tab switches at once, the backend calls through `busy`, failing into
/// `failure`.
fn run_action(dash: Dash, action: Action, busy: RwSignal<bool>, failure: Transient<String>) {
    let state = dash.state;
    match &action {
        Action::ShowInLogs(filter) => {
            state.log_filter_request.set(Some(filter.clone()));
            state.tab.set(Tab::Logs);
            return;
        }
        Action::OpenConfigure => {
            state.tab.set(Tab::Configure);
            return;
        }
        _ => {}
    }
    busy.set(true);
    failure.clear();
    spawn_local(async move {
        let result = match &action {
            Action::Start | Action::StartNow => api::start_zebrad().await,
            Action::StartKillingStale => api::start_zebrad_killing_stale().await,
            Action::Restart => api::restart_zebrad().await,
            Action::Install => api::install_update().await,
            Action::CancelUpdate => api::cancel_update().await,
            Action::OpenLogsFolder => api::open_path(OpenTarget::LogDir).await,
            Action::OpenReleases => api::open_path(OpenTarget::ReleasesPage).await,
            Action::ShowInLogs(_)
            | Action::OpenConfigure
            | Action::ResetConfig
            | Action::CheckForUpdates => Ok(()),
        };
        if let Err(error) = result {
            failure.hold(format!("Could not {}: {error}", action_verb(&action)));
        }
        busy.set(false);
    });
}

/// One action as a control: the reset and the update check bring their own components (a
/// confirmation, a blocked state); the rest are plain buttons.
fn action_view(
    dash: Dash,
    action: Action,
    busy: RwSignal<bool>,
    failure: Transient<String>,
) -> AnyView {
    match action {
        Action::ResetConfig => view! { <ResetConfigButton /> }.into_any(),
        Action::CheckForUpdates => view! { <CheckForUpdate show_result=false /> }.into_any(),
        action => {
            let label = action.label();
            view! {
                <button
                    type="button"
                    disabled=move || busy.get()
                    on:click=move |_| run_action(dash, action.clone(), busy, failure)
                >
                    {label}
                </button>
            }
            .into_any()
        }
    }
}

/// `time` as `HH:MM` in the browser's zone at that instant.
fn local_hhmm(time: SystemTime) -> String {
    hhmm(time, tz_offset_min_at(time))
}

#[component]
pub(super) fn StatusCard(dash: Dash) -> impl IntoView {
    let state = dash.state;
    let verdict = dash.verdict;
    // The parts that rarely change are memos, so a reason that counts seconds does not rebuild
    // the chip or the buttons under the pointer.
    let level = Memo::new(move |_| verdict.with(|verdict| verdict.level));
    let headline = Memo::new(move |_| verdict.with(|verdict| verdict.headline.clone()));
    let actions = Memo::new(move |_| verdict.with(|verdict| verdict.actions.clone()));
    let busy = RwSignal::new(false);
    let failure = Transient::<String>::new();

    let copied = Transient::<()>::new();
    let copy = move |_| {
        let now = state.now.get_untracked();
        let phase_since = state.phase_since.get_untracked();
        let text = verdict.with_untracked(|verdict| {
            with_health_input(state, now, phase_since, |input| {
                summary_text(input, verdict)
            })
        });
        spawn_local(async move {
            match copy_to_clipboard(&text).await {
                Ok(()) => copied.show_for((), COPIED_FOR),
                // A failure is a sentence: it goes under the actions, not into the head line.
                Err(error) => failure.hold(format!("Could not copy the summary: {error}")),
            }
        });
    };

    // The sync hero: the fixed `HERO_RATE_WINDOW` for the pace (the verdict's rule 17 reads
    // the same figures), the last block's age from the sample history (zebrad's own words as
    // the fallback), stage words from this run's progress log and the checkpoint or
    // full-validation note from the metrics and the log.
    let rate = Memo::new(move |_| {
        let now = state.now.get();
        state
            .history
            .with(|history| history.block_rate(Some(HERO_RATE_WINDOW), now))
    });
    let eta = Memo::new(move |_| {
        let now = state.now.get();
        state
            .history
            .with(|history| history.eta_to_tip(Some(HERO_RATE_WINDOW), now))
    });
    let hero = Memo::new(move |_| {
        let note = state.status.with(|status| phase_note(&status.phase));
        sync_summary(dash.latest.get().as_ref(), rate.get(), eta.get(), note)
    });
    let last_block = Memo::new(move |_| {
        let now = state.now.get();
        let change_at = state
            .history
            .with(|history| history.last_height_change_at());
        let words = state.insights.with(|insights| {
            insights
                .sync()
                .and_then(|sync| Some((sync.since_last_block.clone()?, sync.at)))
        });
        last_block_text(
            change_at,
            words.as_ref().map(|(words, at)| (words.as_str(), *at)),
            now,
        )
    });
    let stage = Memo::new(move |_| {
        let started = state.status.with(|status| status.started_at);
        state.insights.with(|insights| {
            insights
                .sync()
                .and_then(|sync| stage_prefix(sync.stage, sync.at, started))
        })
    });
    let line = Memo::new(move |_| {
        hero.with(|hero| {
            hero_line(
                &hero.line,
                hero.waiting,
                stage.get(),
                last_block.get().as_deref(),
            )
        })
    });
    let note = Memo::new(move |_| {
        let height = dash.latest.get()?.height;
        let checkpoint = dash.metrics.get().and_then(|facts| facts.checkpoint_height);
        let full_since = state
            .insights
            .with(|insights| insights.checkpoints().final_reached.map(local_hhmm));
        verification_note(checkpoint, height, full_since.as_deref())
    });
    let muted_hero = move || hero.with(|hero| hero.waiting) || !dash.running();

    view! {
        <section
            class=move || format!("card dash-status {}", level_class(level.get()))
            aria-label="Status"
        >
            <div class="dash-verdict">
                <div class="dash-verdict-head">
                    // Only the level and the headline are live: the reason counts seconds.
                    <span class="dash-verdict-state" role="status">
                        <span class="dash-chip">
                            {move || level_icon(level.get()).view()}
                            <strong>{move || chip_word(level.get(), &headline.get())}</strong>
                        </span>
                        {move || {
                            chip_rest(level.get(), &headline.get())
                                .map(|rest| view! { <span class="dash-verdict-headline">" · " {rest}</span> })
                        }}
                    </span>
                    <span class="dash-copy">
                        <button class="link" type="button" on:click=copy>
                            "Copy summary"
                        </button>
                        <span class="dash-copy-note" role="status">
                            {move || copied.get().map(|()| "Copied")}
                        </span>
                    </span>
                </div>
                <p class="dash-verdict-reason">
                    {move || verdict.with(|verdict| verdict.reason.clone())}
                </p>
                <div class="buttons dash-verdict-actions">
                    {move || {
                        actions
                            .get()
                            .into_iter()
                            .map(|action| action_view(dash, action, busy, failure))
                            .collect_view()
                    }}
                </div>
                {move || {
                    failure
                        .get()
                        .map(|text| view! { <p class="error dash-verdict-failure" role="alert">{text}</p> })
                }}
            </div>
            <div class="dash-hero">
                <div class="dash-hero-head">
                    <h3 id="dash-hero-label">"Sync progress"</h3>
                    <span class=move || {
                        if muted_hero() { "dash-hero-figure muted" } else { "dash-hero-figure" }
                    }>
                        {move || hero.with(|hero| hero.figure.clone())}
                    </span>
                </div>
                <Meter
                    fraction=Signal::derive(move || hero.with(|hero| hero.fraction))
                    label="Sync progress"
                />
                <p class=move || {
                    if muted_hero() { "dash-hero-line muted" } else { "dash-hero-line" }
                }>
                    {move || hero.with(|hero| hero.at_tip).then(|| Icon::Check.view())}
                    <span>{move || line.get()}</span>
                </p>
                <p class="dash-hero-sub muted">{move || note.get().unwrap_or_default()}</p>
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

    #[test]
    fn chip_words_and_classes() {
        assert_eq!(chip_word(Level::Healthy, "At the chain tip"), "Healthy");
        assert_eq!(
            chip_rest(Level::Healthy, "At the chain tip"),
            Some("At the chain tip".into())
        );
        assert_eq!(chip_word(Level::Neutral, "Syncing"), "Syncing");
        assert_eq!(chip_rest(Level::Neutral, "Syncing"), None);
        assert_eq!(chip_word(Level::Attention, "Few peers"), "Attention");
        assert_eq!(
            chip_word(Level::Urgent, "End of support in 2 days"),
            "Attention"
        );
        assert_eq!(chip_word(Level::Problem, "Sync stalled"), "Problem");
        assert_eq!(level_class(Level::Healthy), "dash-level-good");
        assert_eq!(level_class(Level::Neutral), "dash-level-neutral");
        assert_eq!(level_class(Level::Attention), "dash-level-warning");
        assert_eq!(level_class(Level::Urgent), "dash-level-serious");
        assert_eq!(level_class(Level::Problem), "dash-level-critical");
        assert_eq!(level_icon(Level::Healthy), Icon::Check);
        assert_eq!(level_icon(Level::Neutral), Icon::Clock);
        assert_eq!(level_icon(Level::Urgent), Icon::Warning);
        assert_eq!(level_icon(Level::Problem), Icon::Cross);
    }

    #[test]
    fn action_verbs() {
        assert_eq!(action_verb(&Action::Start), "start zebrad");
        assert_eq!(action_verb(&Action::StartNow), "start zebrad");
        assert_eq!(
            action_verb(&Action::StartKillingStale),
            "stop the earlier zebrad and start"
        );
        assert_eq!(action_verb(&Action::Install), "install the update");
        assert_eq!(action_verb(&Action::OpenReleases), "open the releases page");
    }

    #[test]
    fn last_block_words() {
        let now = at(100);
        assert_eq!(
            last_block_text(Some(at(95)), Some(("1m", at(90))), now),
            Some("last block 5 s ago".into())
        );
        // zebrad said "1m 20s" ten seconds ago: a minute and a half, in the page's units.
        assert_eq!(
            last_block_text(None, Some(("1m 20s", at(90))), now),
            Some("last block 1 min ago".into())
        );
        assert_eq!(
            last_block_text(None, Some(("5s", at(100))), now),
            Some("last block 5 s ago".into())
        );
        assert_eq!(last_block_text(None, Some(("soon", at(100))), now), None);
        assert_eq!(last_block_text(None, None, now), None);
    }

    #[test]
    fn stage_words_only_from_this_run() {
        let started = Some(at(100));
        assert_eq!(
            stage_prefix(SyncStage::NearTip, at(200), started),
            Some("Close to the chain tip")
        );
        assert_eq!(
            stage_prefix(SyncStage::WaitingForGenesis, at(100), started),
            Some("Waiting for the genesis block")
        );
        // An older report, or no run start: nothing.
        assert_eq!(stage_prefix(SyncStage::NearTip, at(99), started), None);
        assert_eq!(stage_prefix(SyncStage::NearTip, at(200), None), None);
        // The line says these already, or the verdict does.
        for stage in [
            SyncStage::Syncing,
            SyncStage::AtTip,
            SyncStage::Slow,
            SyncStage::Stalled,
            SyncStage::GenesisStalled,
        ] {
            assert_eq!(stage_prefix(stage, at(200), started), None, "{stage:?}");
        }
    }

    #[test]
    fn hero_lines() {
        assert_eq!(
            hero_line("Waiting for zebrad's RPC…", true, Some("x"), Some("y")),
            "Waiting for zebrad's RPC…"
        );
        assert_eq!(
            hero_line(
                "2,952,680 of 3,120,000 blocks · 3,747 blocks/min · about 44 min left",
                false,
                None,
                Some("last block 5 s ago")
            ),
            "2,952,680 of 3,120,000 blocks · 3,747 blocks/min · about 44 min left · last block 5 s ago"
        );
        assert_eq!(
            hero_line(
                "At the chain tip · height 3,120,012",
                false,
                None,
                Some("last block 48 s ago")
            ),
            "At the chain tip · height 3,120,012 · last block 48 s ago"
        );
        assert_eq!(
            hero_line(
                "2,952,680 of 3,120,000 blocks · estimating…",
                false,
                Some("Close to the chain tip"),
                None
            ),
            "Close to the chain tip · 2,952,680 of 3,120,000 blocks · estimating…"
        );
    }

    #[test]
    fn verification_notes() {
        assert_eq!(
            verification_note(Some(2_500_000), 2_501_000, Some("14:02")),
            Some("Checkpoints · verified to 2,500,000".into())
        );
        assert_eq!(
            verification_note(Some(2_500_000), 2_500_000 + CHECKPOINT_NEAR_BLOCKS, None),
            Some("Checkpoints · verified to 2,500,000".into())
        );
        assert_eq!(
            verification_note(Some(2_500_000), 2_952_680, Some("14:02")),
            Some("Full validation since 14:02".into())
        );
        assert_eq!(verification_note(Some(2_500_000), 2_952_680, None), None);
        assert_eq!(
            verification_note(None, 2_952_680, Some("14:02")),
            Some("Full validation since 14:02".into())
        );
        assert_eq!(verification_note(None, 10, None), None);
        // A checkpoint height ahead of the verified height is still the verifier at work.
        assert_eq!(
            verification_note(Some(2_500_400), 2_500_000, None),
            Some("Checkpoints · verified to 2,500,400".into())
        );
    }
}
