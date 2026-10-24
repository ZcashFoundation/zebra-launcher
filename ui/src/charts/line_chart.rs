//! A chart card: one or two lines over time, with a hover crosshair, keyboard navigation and
//! a table view. The geometry comes from [`crate::charts::geometry`].

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::{
    charts::{
        ChartData, Geometry, MAX_POINTS, PAD_B, PAD_L, PAD_R, PAD_T, VIEW_H, VIEW_W,
        downsample_indices, geometry, hhmmss, nearest_index, summary, tz_offset_min,
    },
    format::thousands,
};

/// A coordinate in `viewBox` units, as an attribute value.
fn px(value: f64) -> String {
    format!("{value:.1}")
}

/// A sample value in the tooltip and the table.
fn whole(value: f64) -> String {
    thousands(value.round().max(0.0) as u64)
}

/// A chart card. `data` is the range's samples; `empty` is what the frame says while there are
/// fewer than two of them.
#[component]
pub fn LineChart(
    title: &'static str,
    subtitle: &'static str,
    #[prop(into)] data: Signal<ChartData>,
    #[prop(into)] empty: Signal<String>,
    /// Start the y axis at zero (counts) instead of around the data (heights).
    #[prop(optional)]
    from_zero: bool,
    /// Wash the area under the first line.
    #[prop(optional)]
    area: bool,
) -> impl IntoView {
    let tz = tz_offset_min();
    let table = RwSignal::new(false);
    let hover = RwSignal::new(None::<usize>);
    let layout = Memo::new(move |_| data.with(|d| geometry(d, from_zero, tz)));
    let label = Memo::new(move |_| data.with(|d| summary(title, d, tz)));
    let legend = move || data.with(|d| d.series.len() >= 2);

    view! {
        <div class="card chart-card">
            <div class="chart-head">
                <div class="chart-titles">
                    <h3>{title}</h3>
                    <p class="chart-subtitle">{subtitle}</p>
                </div>
                <button
                    class="chart-toggle"
                    aria-pressed=move || table.get().to_string()
                    on:click=move |_| table.update(|shown| *shown = !*shown)
                >
                    "Table"
                </button>
            </div>
            <Show when=legend>
                <ul class="chart-legend" aria-label="Series">
                    {move || {
                        data.with(|d| {
                            d.series
                                .iter()
                                .map(|s| {
                                    view! {
                                        <li>
                                            <span class=format!("chart-key {}", s.class) aria-hidden="true"></span>
                                            {s.name}
                                        </li>
                                    }
                                })
                                .collect_view()
                        })
                    }}
                </ul>
            </Show>
            <Show
                when=move || layout.with(Option::is_some)
                fallback=move || {
                    view! {
                        <div class="chart-empty muted" role="status">{move || empty.get()}</div>
                    }
                }
            >
                <Show
                    when=move || table.get()
                    fallback=move || view! { <Plot layout data label hover area /> }
                >
                    <DataTable title data tz />
                </Show>
            </Show>
        </div>
    }
}

/// The SVG plot with its crosshair and tooltip.
#[component]
fn Plot(
    layout: Memo<Option<Geometry>>,
    data: Signal<ChartData>,
    label: Memo<String>,
    hover: RwSignal<Option<usize>>,
    area: bool,
) -> impl IntoView {
    let tz = tz_offset_min();
    let count = move || layout.with(|l| l.as_ref().map_or(0, |g| g.xs.len()));
    let highlighted = move || hover.get().filter(|index| *index < count());
    let classes = move || data.with(|d| d.series.iter().map(|s| s.class).collect::<Vec<_>>());

    // The sample nearest to the pointer, from its offset in the SVG's own box.
    let locate = move |ev: &web_sys::PointerEvent| -> Option<usize> {
        let svg = ev.current_target()?.dyn_into::<web_sys::Element>().ok()?;
        let rect = svg.get_bounding_client_rect();
        if rect.width() <= 0.0 {
            return None;
        }
        let x = (f64::from(ev.client_x()) - rect.left()) * VIEW_W / rect.width();
        layout.with(|l| l.as_ref().map(|g| nearest_index(&g.xs, x)))
    };
    let on_key = move |ev: web_sys::KeyboardEvent| {
        let count = count();
        if count == 0 {
            return;
        }
        let current = hover.get_untracked().unwrap_or(count - 1).min(count - 1);
        let next = match ev.key().as_str() {
            "ArrowLeft" => current.saturating_sub(1),
            "ArrowRight" => (current + 1).min(count - 1),
            "Home" => 0,
            "End" => count - 1,
            "Escape" => {
                hover.set(None);
                return;
            }
            _ => return,
        };
        ev.prevent_default();
        hover.set(Some(next));
    };
    let on_focus = move |_| {
        let count = count();
        if hover.get_untracked().is_none() && count > 0 {
            hover.set(Some(count - 1));
        }
    };

    let axes_and_lines = move || {
        layout.with(|l| {
            let g = l.as_ref()?;
            let classes = classes();
            let first_class = classes.first().copied().unwrap_or("chart-series-1");
            let n = g.xs.len();
            Some(view! {
                <g class="chart-axes">
                    {g
                        .y_ticks
                        .iter()
                        .map(|(y, text)| {
                            view! {
                                <line
                                    class="chart-grid-line"
                                    x1=px(PAD_L)
                                    x2=px(VIEW_W - PAD_R)
                                    y1=px(*y)
                                    y2=px(*y)
                                />
                                <text class="chart-axis chart-axis-y" x=px(PAD_L - 8.0) y=px(*y + 4.0)>
                                    {text.clone()}
                                </text>
                            }
                        })
                        .collect_view()}
                    {g
                        .x_ticks
                        .iter()
                        .map(|(x, text)| {
                            view! {
                                <text class="chart-axis chart-axis-x" x=px(*x) y=px(VIEW_H - 9.0)>
                                    {text.clone()}
                                </text>
                            }
                        })
                        .collect_view()}
                </g>
                {area
                    .then(|| {
                        view! { <path class=format!("chart-area {first_class}") d=g.area.clone() /> }
                    })}
                {g
                    .lines
                    .iter()
                    .zip(classes.iter())
                    .map(|(d, class)| {
                        view! { <path class=format!("chart-line {class}") d=d.clone() /> }
                    })
                    .collect_view()}
                {g
                    .ys
                    .iter()
                    .zip(classes.iter())
                    .map(|(ys, class)| {
                        view! {
                            <circle class=format!("chart-dot {class}") cx=px(g.xs[n - 1]) cy=px(ys[n - 1]) r="4.5" />
                        }
                    })
                    .collect_view()}
                <text class="chart-direct-label" x=px(g.xs[n - 1] + 10.0) y=px(g.ys[0][n - 1] + 4.0)>
                    {g.end_label.clone()}
                </text>
            })
        })
    };
    let crosshair = move || {
        let index = highlighted()?;
        layout.with(|l| {
            let g = l.as_ref()?;
            let x = g.xs[index];
            Some(view! {
                <g class="chart-hover">
                    <line class="chart-crosshair" x1=px(x) x2=px(x) y1=px(PAD_T) y2=px(VIEW_H - PAD_B) />
                    {g
                        .ys
                        .iter()
                        .zip(classes())
                        .map(|(ys, class)| {
                            view! { <circle class=format!("chart-dot {class}") cx=px(x) cy=px(ys[index]) r="4.5" /> }
                        })
                        .collect_view()}
                </g>
            })
        })
    };
    let tooltip = move || {
        let index = highlighted()?;
        let x = layout.with(|l| l.as_ref().map(|g| g.xs[index]))?;
        let (time, rows) = data.with(|d| {
            let rows: Vec<_> = d
                .series
                .iter()
                .map(|s| {
                    (
                        s.class,
                        s.name,
                        s.values
                            .get(index)
                            .map_or_else(|| "—".to_owned(), |v| whole(*v)),
                    )
                })
                .collect();
            (d.times.get(index).copied(), rows)
        });
        let time = time?;
        let class = if x > VIEW_W * 0.6 {
            "chart-tooltip chart-tooltip-left"
        } else {
            "chart-tooltip"
        };
        Some(view! {
            <div class=class style:left=format!("{:.2}%", x / VIEW_W * 100.0)>
                <div class="chart-tooltip-time">{hhmmss(time, tz)}</div>
                {rows
                    .into_iter()
                    .map(|(class, name, value)| {
                        view! {
                            <div class="chart-tooltip-row">
                                <span class=format!("chart-key {class}") aria-hidden="true"></span>
                                <strong>{value}</strong>
                                <span class="chart-tooltip-name">{name}</span>
                            </div>
                        }
                    })
                    .collect_view()}
            </div>
        })
    };

    view! {
        <div class="chart-plot">
            <svg
                class="chart-svg"
                viewBox="0 0 640 220"
                width="100%"
                height="auto"
                tabindex="0"
                role="img"
                aria-label=move || label.get()
                on:pointermove=move |ev| hover.set(locate(&ev))
                on:pointerleave=move |_| hover.set(None)
                on:focus=on_focus
                on:blur=move |_| hover.set(None)
                on:keydown=on_key
            >
                <line
                    class="chart-axis-line"
                    x1=px(PAD_L)
                    x2=px(VIEW_W - PAD_R)
                    y1=px(VIEW_H - PAD_B)
                    y2=px(VIEW_H - PAD_B)
                />
                {axes_and_lines}
                {crosshair}
            </svg>
            {tooltip}
        </div>
    }
}

/// The chart's values as a table: at most [`MAX_POINTS`] rows, picked evenly.
#[component]
fn DataTable(title: &'static str, data: Signal<ChartData>, tz: i32) -> impl IntoView {
    view! {
        <div class="chart-table-wrap">
            <table class="chart-table" aria-label=format!("{title} samples")>
                <thead>
                    <tr>
                        <th scope="col">"Time"</th>
                        {move || {
                            data.with(|d| {
                                d.series
                                    .iter()
                                    .map(|s| view! { <th scope="col">{s.name}</th> })
                                    .collect_view()
                            })
                        }}
                    </tr>
                </thead>
                <tbody>
                    {move || {
                        data.with(|d| {
                            downsample_indices(d.len(), MAX_POINTS)
                                .into_iter()
                                .map(|index| {
                                    view! {
                                        <tr>
                                            <td>{hhmmss(d.times[index], tz)}</td>
                                            {d
                                                .series
                                                .iter()
                                                .map(|s| view! { <td>{whole(s.values[index])}</td> })
                                                .collect_view()}
                                        </tr>
                                    }
                                })
                                .collect_view()
                        })
                    }}
                </tbody>
            </table>
        </div>
    }
}
