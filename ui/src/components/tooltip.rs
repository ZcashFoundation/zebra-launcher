//! A "?" icon that explains a setting on hover, focus or tap.
//!
//! The box opens above the icon and flips below it when there is no room above; it is shifted
//! sideways to stay inside the scrolling page. Hover and keyboard focus show it, a click or tap
//! pins it (so it stays while the pointer moves away), `Escape` hides it. The box is measured
//! with `getBoundingClientRect` once it is displayed, so the placement happens one frame after
//! the icon is hovered, while the box is still invisible.

use leptos::{html, prelude::*};

/// Space between the icon and the box, matching `.help-tip` in `styles/config-form.css`.
const GAP: f64 = 6.0;

/// The box keeps this far from the edges of the scrolling page.
const EDGE: f64 = 8.0;

/// A rectangle in viewport pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
}

impl Rect {
    fn of(element: &web_sys::Element) -> Self {
        let rect = element.get_bounding_client_rect();
        Rect {
            left: rect.left(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
        }
    }

    /// The viewport.
    fn viewport() -> Self {
        let window = window();
        let size = |value: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
            value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0)
        };
        Rect {
            left: 0.0,
            top: 0.0,
            right: size(window.inner_width()),
            bottom: size(window.inner_height()),
        }
    }

    /// The part of `self` inside `other`.
    fn clipped_to(self, other: Rect) -> Rect {
        Rect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        }
    }

    fn width(self) -> f64 {
        self.right - self.left
    }

    fn height(self) -> f64 {
        self.bottom - self.top
    }
}

/// Where the box sits relative to the icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Above,
    Below,
}

/// The side and the horizontal shift (pixels, positive to the right) of a box `width` by
/// `height` that is centred on `icon` by default: above the icon when the box fits there with
/// [`EDGE`] to spare, else below, or above anyway when there is more room above than below; and
/// shifted so it keeps [`EDGE`] from the left and right of `bounds`.
fn place(icon: Rect, width: f64, height: f64, bounds: Rect) -> (Side, f64) {
    let room_above = icon.top - bounds.top;
    let room_below = bounds.bottom - icon.bottom;
    let needed = height + GAP + EDGE;
    let side = if room_above >= needed || room_above >= room_below {
        Side::Above
    } else {
        Side::Below
    };
    let centred_left = (icon.left + icon.right - width) / 2.0;
    let min_left = bounds.left + EDGE;
    let max_left = (bounds.right - EDGE - width).max(min_left);
    let left = centred_left.clamp(min_left, max_left);
    (side, left - centred_left)
}

/// A help icon with `text` in a tooltip. `id` must be unique on the page: it links the icon
/// to its tooltip for assistive technology.
#[component]
pub fn Help(id: String, text: &'static str) -> impl IntoView {
    let tooltip_id = format!("{id}-tip");
    let icon = NodeRef::<html::Button>::new();
    let tip = NodeRef::<html::Span>::new();
    let hovered = RwSignal::new(false);
    let focused = RwSignal::new(false);
    let pinned = RwSignal::new(false);
    let open = Memo::new(move |_| hovered.get() || focused.get() || pinned.get());
    // `None` until the displayed box was measured; the box stays invisible until then.
    let placement = RwSignal::new(None::<(Side, f64)>);

    Effect::new(move |_| {
        if !open.get() {
            placement.set(None);
            return;
        }
        request_animation_frame(move || {
            let (Some(icon), Some(tip)) = (icon.get_untracked(), tip.get_untracked()) else {
                return;
            };
            // The app's scrolling page clips what overflows it; outside one, the viewport does.
            let bounds = icon
                .closest(".page")
                .ok()
                .flatten()
                .map_or_else(Rect::viewport, |page| Rect::of(&page))
                .clipped_to(Rect::viewport());
            let box_rect = Rect::of(&tip);
            placement.set(Some(place(
                Rect::of(&icon),
                box_rect.width(),
                box_rect.height(),
                bounds,
            )));
        });
    });

    let hide = move || {
        hovered.set(false);
        focused.set(false);
        pinned.set(false);
    };
    let on_key = move |event: web_sys::KeyboardEvent| {
        if event.key() == "Escape" && open.get_untracked() {
            event.prevent_default();
            hide();
        }
    };

    view! {
        <span
            class="help"
            on:pointerenter=move |_| hovered.set(true)
            on:pointerleave=move |_| hovered.set(false)
        >
            <button
                type="button"
                class="help-icon"
                class:help-pinned=move || pinned.get()
                node_ref=icon
                aria-label="What this setting does"
                aria-describedby=tooltip_id.clone()
                on:focus=move |_| focused.set(true)
                on:blur=move |_| {
                    focused.set(false);
                    pinned.set(false);
                }
                on:click=move |_| pinned.update(|pinned| *pinned = !*pinned)
                on:keydown=on_key
            >
                "?"
            </button>
            <span
                role="tooltip"
                id=tooltip_id
                class="help-tip"
                class:help-open=move || open.get()
                class:help-placed=move || placement.get().is_some()
                class:help-below=move || {
                    placement.get().is_some_and(|(side, _)| side == Side::Below)
                }
                style=(
                    "--help-shift",
                    move || format!("{}px", placement.get().map_or(0.0, |(_, shift)| shift)),
                )
                node_ref=tip
            >
                {text}
            </span>
        </span>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: Rect = Rect {
        left: 0.0,
        top: 100.0,
        right: 1000.0,
        bottom: 700.0,
    };

    fn icon(left: f64, top: f64) -> Rect {
        Rect {
            left,
            top,
            right: left + 16.0,
            bottom: top + 16.0,
        }
    }

    #[test]
    fn opens_above_when_there_is_room() {
        assert_eq!(
            place(icon(500.0, 400.0), 200.0, 60.0, BOUNDS),
            (Side::Above, 0.0)
        );
    }

    #[test]
    fn flips_below_near_the_top() {
        // 40 px above the icon inside the page: not enough for a 60 px box.
        assert_eq!(
            place(icon(500.0, 140.0), 200.0, 60.0, BOUNDS),
            (Side::Below, 0.0)
        );
        // Just enough: 60 + GAP + EDGE = 74.
        assert_eq!(
            place(icon(500.0, 174.0), 200.0, 60.0, BOUNDS).0,
            Side::Above
        );
    }

    #[test]
    fn stays_above_when_there_is_no_room_either_way() {
        let tall = Rect {
            bottom: 300.0,
            ..BOUNDS
        };
        // 100 px above, 84 below: above wins.
        assert_eq!(place(icon(500.0, 200.0), 200.0, 150.0, tall).0, Side::Above);
        // 50 px above, 134 below.
        assert_eq!(place(icon(500.0, 150.0), 200.0, 150.0, tall).0, Side::Below);
    }

    #[test]
    fn shifts_away_from_the_edges() {
        // Centred on the icon the box would start at 18 - 100 = -82; it moves to EDGE.
        let (_, shift) = place(icon(10.0, 400.0), 200.0, 60.0, BOUNDS);
        assert_eq!(shift, 90.0);
        // Centred on the icon the box would end at 998 + 100; it moves back to 1000 - EDGE.
        let (_, shift) = place(icon(990.0, 400.0), 200.0, 60.0, BOUNDS);
        assert_eq!(shift, -106.0);
    }

    #[test]
    fn a_box_wider_than_the_page_sticks_to_the_left_edge() {
        let (_, shift) = place(icon(500.0, 400.0), 1200.0, 60.0, BOUNDS);
        assert_eq!(shift, EDGE - (508.0 - 600.0));
    }

    #[test]
    fn clipping_keeps_the_intersection() {
        let page = Rect {
            left: -20.0,
            top: 150.0,
            right: 1400.0,
            bottom: 900.0,
        };
        assert_eq!(
            page.clipped_to(BOUNDS),
            Rect {
                left: 0.0,
                top: 150.0,
                right: 1000.0,
                bottom: 700.0
            }
        );
        assert_eq!(BOUNDS.width(), 1000.0);
        assert_eq!(BOUNDS.height(), 600.0);
    }
}
