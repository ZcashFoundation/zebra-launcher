//! The inline icons: a check, a clock, a warning triangle and a cross, each a 16-unit SVG drawn
//! in `currentColor` and sized by `.icon` in styles.css, so a colour or size fix is one edit.
//! They are decorative (`aria-hidden`): the text beside an icon says what it means.
//!
//! Used by the dashboard (the sync hero's check, the warning on a stale tile) and the sync
//! stage card's tone. The Settings page's `WarningIcon` is the second phase's.

use leptos::prelude::*;

/// The outline of the round icons.
const CIRCLE: &str = "M8 1a7 7 0 1 1 0 14A7 7 0 0 1 8 1z";

/// An icon: its outline (`frame`) and the mark inside it, classed `icon icon-{name}`.
fn icon(name: &'static str, frame: &'static str, mark: &'static str) -> impl IntoView {
    view! {
        <svg class=format!("icon icon-{name}") viewBox="0 0 16 16" aria-hidden="true">
            <path class="icon-frame" d=frame />
            <path class="icon-mark" d=mark />
        </svg>
    }
}

/// A check mark in a circle: done, at the tip, good.
pub fn check_icon() -> impl IntoView {
    icon("check", CIRCLE, "M4.5 8.5l2.5 2.5 4.5-5")
}

/// A clock face: in progress, waiting.
pub fn clock_icon() -> impl IntoView {
    icon("clock", CIRCLE, "M8 4.5V8l2.5 1.5")
}

/// A warning triangle: needs a look.
pub fn warning_icon() -> impl IntoView {
    icon("warning", "M8 1.5L15 14H1z", "M8 5v4M8 11.2v.3")
}

/// A cross in a circle: failed, critical.
pub fn cross_icon() -> impl IntoView {
    icon("cross", CIRCLE, "M5.5 5.5l5 5M10.5 5.5l-5 5")
}
