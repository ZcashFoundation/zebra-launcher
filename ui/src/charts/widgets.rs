//! Dashboard widgets: the range control, stat tiles with sparklines and the sync meter.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::{
    charts::{Range, spark_paths},
    components::icons::warning_icon,
};

/// A segmented control for the dashboard's time range: a radio group, operable with the arrow
/// keys.
#[component]
pub fn RangeControl(value: RwSignal<Range>) -> impl IntoView {
    view! {
        <div class="dash-range">
            <span class="dash-range-label" id="dash-range-label">"Range"</span>
            <div class="dash-segmented" role="radiogroup" aria-labelledby="dash-range-label">
                {Range::ALL
                    .into_iter()
                    .map(|option| {
                        let selected = move || value.get() == option;
                        view! {
                            <button
                                type="button"
                                role="radio"
                                aria-checked=move || selected().to_string()
                                tabindex=move || if selected() { "0" } else { "-1" }
                                on:click=move |_| value.set(option)
                                on:keydown=move |ev: web_sys::KeyboardEvent| {
                                    let by = match ev.key().as_str() {
                                        "ArrowRight" | "ArrowDown" => 1,
                                        "ArrowLeft" | "ArrowUp" => -1,
                                        _ => return,
                                    };
                                    ev.prevent_default();
                                    value.set(option.step(by));
                                    focus_sibling(&ev, by);
                                }
                            >
                                {option.label()}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>
        </div>
    }
}

/// Moves focus to the next (`by` > 0) or previous radio, wrapping around the group.
fn focus_sibling(ev: &web_sys::KeyboardEvent, by: i32) {
    let Some(current) = ev
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    let parent = current.parent_element();
    let next = if by > 0 {
        current
            .next_element_sibling()
            .or_else(|| parent.and_then(|p| p.first_element_child()))
    } else {
        current
            .previous_element_sibling()
            .or_else(|| parent.and_then(|p| p.last_element_child()))
    };
    if let Some(next) = next.and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) {
        let _ = next.focus();
    }
}

/// How a tile's value reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Normal,
    /// Not a number yet: `estimating…`, `—`.
    Muted,
    /// Needs attention; shown with a warning icon.
    Warning,
}

/// A KPI tile: label, value, an optional line under it and an optional sparkline.
#[component]
pub fn StatTile(
    label: &'static str,
    #[prop(into)] value: Signal<String>,
    /// The full number behind a compact value, as the value's tooltip.
    #[prop(into, optional)]
    full_value: Option<Signal<String>>,
    #[prop(into, optional)] detail: Option<Signal<String>>,
    #[prop(into, optional)] spark: Option<Signal<Vec<f64>>>,
    #[prop(into, optional)] tone: Option<Signal<Tone>>,
) -> impl IntoView {
    let tone = move || tone.map_or(Tone::Normal, |tone| tone.get());
    let value_class = move || match tone() {
        Tone::Normal => "tile-value",
        Tone::Muted => "tile-value tile-muted",
        Tone::Warning => "tile-value tile-warning",
    };
    view! {
        <div class="tile">
            <span class="tile-label">{label}</span>
            <span class=value_class title=move || full_value.map(|full| full.get())>
                {move || (tone() == Tone::Warning).then(warning_icon)}
                <span>{move || value.get()}</span>
            </span>
            {detail.map(|detail| view! { <span class="tile-detail">{move || detail.get()}</span> })}
            {spark.map(|spark| view! { <Sparkline values=spark /> })}
        </div>
    }
}

/// A 120 by 32 sparkline: the line in the muted series colour, its last segment and end dot
/// in the first series colour. Empty with fewer than two values.
#[component]
pub fn Sparkline(values: Signal<Vec<f64>>) -> impl IntoView {
    view! {
        <svg class="chart-spark" viewBox="0 0 120 32" aria-hidden="true">
            {move || {
                values
                    .with(|values| spark_paths(values))
                    .map(|(line, last, (x, y))| {
                        view! {
                            <path class="chart-spark-line" d=line />
                            <path class="chart-spark-end" d=last />
                            <circle class="chart-spark-dot" cx=format!("{x:.1}") cy=format!("{y:.1}") r="2.5" />
                        }
                    })
            }}
        </svg>
    }
}

/// A progress bar from 0 to 1.
#[component]
pub fn Meter(#[prop(into)] fraction: Signal<f64>, label: &'static str) -> impl IntoView {
    let percent = move || fraction.get().clamp(0.0, 1.0) * 100.0;
    view! {
        <div
            class="meter"
            role="progressbar"
            aria-label=label
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow=move || format!("{:.1}", percent())
        >
            <div class="meter-fill" style:width=move || format!("{:.2}%", percent())></div>
        </div>
    }
}
