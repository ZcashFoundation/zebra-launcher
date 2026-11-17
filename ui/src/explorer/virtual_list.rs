//! A virtual list: rows of a fixed height in a scroller of a fixed height, of which only the
//! ones in view and a few either side are in the page. Each row slot is made once and keyed
//! by its index modulo the slots, so scrolling moves slots and updates their text rather than
//! building rows. The scroll position is read once a frame, and the page hears of the rows in
//! view once scrolling settles, so a fling asks the node for nothing.
//!
//! An ARIA grid with `aria-rowcount` and each row's `aria-rowindex`. The slots are in the page
//! in slot order, not row order, so the rowgroup's `aria-owns` lists them in row order, and
//! the grid is one Tab stop with an active row (`aria-activedescendant`) rather than a Tab
//! stop per row, whose order would follow the slots: the arrow keys, Page Up and Down, Home and
//! End move the active row and scroll it into view, and Enter opens it.

use std::time::Duration;

use leptos::{html, prelude::*};

/// A row's height, in pixels.
pub const ROW: f64 = 32.0;
/// Rows kept either side of the ones in view.
pub const OVERSCAN: u32 = 6;
/// How long scrolling must rest before the page hears of the rows in view.
const SETTLE: Duration = Duration::from_millis(120);

/// The rows a list has in the page, and the ones of them in view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// `first..end`: the rows in view and the overscan either side.
    pub rows: (u32, u32),
    /// The rows in view, one half in view included.
    pub seen: (u32, u32),
}

impl Window {
    /// The window of a viewport `height` pixels tall, scrolled `top` pixels, over `total` rows
    /// of `row` pixels, with `overscan` either side.
    pub fn of(top: f64, height: f64, row: f64, total: u32, overscan: u32) -> Self {
        Window {
            rows: visible_window(top, height, row, total, overscan),
            seen: visible_window(top, height, row, total, 0),
        }
    }
}

/// The rows a viewport `height` pixels tall, scrolled `top` pixels, shows of `total` rows of
/// `row` pixels, with `overscan` either side: `first..end`.
pub fn visible_window(top: f64, height: f64, row: f64, total: u32, overscan: u32) -> (u32, u32) {
    if total == 0 || row <= 0.0 {
        return (0, 0);
    }
    let first_seen = (top.max(0.0) / row).floor() as u32;
    let seen = (height.max(0.0) / row).ceil() as u32 + 1;
    let first = first_seen
        .saturating_sub(overscan)
        .min(total.saturating_sub(1));
    let end = first_seen
        .saturating_add(seen)
        .saturating_add(overscan)
        .min(total);
    (first, end.max(first))
}

/// The slots a viewport `height` pixels tall needs: the rows it shows at once, one more for a
/// row half in view, and the overscan either side; never more than the rows.
pub fn slot_count(height: f64, row: f64, total: u32, overscan: u32) -> u32 {
    let seen = (height.max(0.0) / row).ceil() as u32 + 1;
    (seen + 2 * overscan).min(total)
}

/// The row slot `slot` shows when the window starts at `first`: the one row of
/// `first..first + slots` whose index is `slot` modulo `slots`.
pub fn slot_row(slot: u32, first: u32, slots: u32) -> u32 {
    if slots == 0 {
        return 0;
    }
    first + (slot + slots - first % slots) % slots
}

/// The slots of a window starting at row `first`, in the order of the rows they show.
pub fn slots_in_row_order(first: u32, slots: u32) -> Vec<u32> {
    (first..first + slots).map(|row| row % slots).collect()
}

/// The row a key moves the active row to, of `total` rows with `page` in view at once: from
/// the active row, or with none from `first_seen`, the first row in view. `None` for another
/// key.
pub fn key_target(
    key: &str,
    active: Option<u32>,
    first_seen: u32,
    total: u32,
    page: u32,
) -> Option<u32> {
    let last = total.checked_sub(1)?;
    let from = active.unwrap_or(first_seen).min(last);
    let moved = u32::from(active.is_some());
    Some(match key {
        "ArrowDown" => (from + moved).min(last),
        "ArrowUp" => from.saturating_sub(moved),
        "PageDown" => (from + page.max(1)).min(last),
        "PageUp" => from.saturating_sub(page.max(1)),
        "Home" => 0,
        "End" => last,
        _ => return None,
    })
}

/// Where to scroll a viewport `height` pixels tall, scrolled `top`, so row `row` of `row_px`
/// pixels is all in view; `None` when it is already.
pub fn scroll_for(row: u32, top: f64, height: f64, row_px: f64) -> Option<f64> {
    let start = f64::from(row) * row_px;
    if start < top {
        Some(start)
    } else if start + row_px > top + height {
        Some(start + row_px - height)
    } else {
        None
    }
}

/// A virtual list of `total` rows, at most `rows_shown` of them in view at once, under a
/// header row. `row` builds one slot's cells, once, from the row index it shows (a signal:
/// the cells read it); its controls should take no Tab stop of their own. `on_window` hears of
/// the rows in the page and those in view once scrolling settles, and when the list first
/// shows. `on_active` hears of the row the keys made active, with its element, and of `None`
/// when focus leaves the list; `on_open` of Enter on the active row. `id` names the slots for
/// ARIA, `{id}-{slot}`.
#[component]
pub fn VirtualList<H, HV, R, V>(
    total: u32,
    rows_shown: u32,
    id: &'static str,
    /// The grid's name for assistive technology.
    label: String,
    /// The header row's cells.
    header: H,
    row: R,
    on_window: Callback<Window>,
    on_active: Callback<Option<(u32, web_sys::Element)>>,
    on_open: Callback<u32>,
) -> impl IntoView
where
    H: Fn() -> HV + Send + Sync + 'static,
    HV: IntoView + 'static,
    R: Fn(Memo<u32>) -> V + 'static,
    V: IntoView + 'static,
{
    let height = f64::from(total.min(rows_shown).max(1)) * ROW;
    let slots = slot_count(height, ROW, total, OVERSCAN);
    let top = RwSignal::new(0.0f64);
    let window = Memo::new(move |_| Window::of(top.get(), height, ROW, total, OVERSCAN));
    let scroller = NodeRef::<html::Div>::new();
    let body = NodeRef::<html::Div>::new();
    let frame_pending = StoredValue::new(false);
    let settle = StoredValue::new(0u64);
    let active = RwSignal::new(None::<u32>);
    let focused = RwSignal::new(false);
    let slot_id = move |row: u32| format!("{id}-{}", row % slots.max(1));
    // The slots' first row: near the end they keep to the last rows, so none shows a row past
    // them.
    let first = Memo::new(move |_| window.get().rows.0.min(total - slots));

    // Scroll events come faster than frames: the position is read once a frame. A slot that
    // had focus (a row's control, clicked) may show another row now: focus goes to the grid.
    let on_scroll = move |_| {
        if frame_pending.get_value() {
            return;
        }
        frame_pending.set_value(true);
        request_animation_frame(move || {
            frame_pending.try_set_value(false);
            if let Some(scroller) = scroller.get_untracked() {
                top.try_set(f64::from(scroller.scroll_top()));
                let in_rows = document()
                    .active_element()
                    .zip(body.get_untracked())
                    .is_some_and(|(focus, body)| body.contains(Some(&focus)));
                if in_rows {
                    let _ = scroller.focus();
                }
            }
        });
    };
    // An active row scrolled out of the page by other means is active no more: its slot shows
    // another row.
    Effect::new(move |_| {
        let (start, end) = window.get().rows;
        if active
            .get_untracked()
            .is_some_and(|row| row < start || row >= end)
        {
            active.set(None);
        }
    });
    let on_key = move |event: web_sys::KeyboardEvent| {
        let key = event.key();
        if key == "Enter" {
            if let Some(row) = active.get_untracked() {
                event.prevent_default();
                on_open.run(row);
            }
            return;
        }
        let first_seen = window.get_untracked().seen.0;
        let Some(row) = key_target(&key, active.get_untracked(), first_seen, total, rows_shown)
        else {
            return;
        };
        event.prevent_default();
        if let Some(scroller) = scroller.get_untracked()
            && let Some(to) = scroll_for(row, top.get_untracked(), height, ROW)
        {
            scroller.set_scroll_top(to as i32);
            top.set(to);
        }
        active.set(Some(row));
        // Once the slots show the row, the page hears of it with its element.
        request_animation_frame(move || {
            if active.try_get_untracked() == Some(Some(row))
                && let Some(element) = document().get_element_by_id(&slot_id(row))
            {
                on_active.run(Some((row, element)));
            }
        });
    };
    // The page hears of the rows in view once they rest.
    Effect::new(move |_| {
        let rows = window.get();
        let mine = settle.get_value() + 1;
        settle.set_value(mine);
        set_timeout(
            move || {
                if settle.try_get_value() == Some(mine) {
                    on_window.run(rows);
                }
            },
            SETTLE,
        );
    });
    let slot_views = (0..slots)
        .map(|slot| {
            let index = Memo::new(move |_| slot_row(slot, first.get(), slots));
            let cells = row(index);
            let is_active = move || focused.get() && active.get() == Some(index.get());
            view! {
                <div
                    class="ex-vrow"
                    class:ex-vrow-active=is_active
                    role="row"
                    id=format!("{id}-{slot}")
                    aria-rowindex=move || (index.get() + 2).to_string()
                    style:transform=move || format!("translateY({}px)", f64::from(index.get()) * ROW)
                >
                    {cells}
                </div>
            }
        })
        .collect_view();
    let owns = move || {
        slots_in_row_order(first.get(), slots)
            .into_iter()
            .map(|slot| format!("{id}-{slot}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    view! {
        <div
            class="ex-vlist"
            role="grid"
            aria-label=label
            aria-rowcount=(total + 1).to_string()
            aria-activedescendant=move || active.get().map(slot_id)
            tabindex="0"
            node_ref=scroller
            style:height=format!("{}px", height + ROW)
            on:scroll=on_scroll
            on:keydown=on_key
            on:focus=move |_| focused.set(true)
            on:blur=move |_| {
                focused.set(false);
                on_active.run(None);
            }
        >
            <div class="ex-vrow ex-vhead" role="row" aria-rowindex="1">{header()}</div>
            <div
                class="ex-vbody"
                role="rowgroup"
                aria-owns=owns
                node_ref=body
                style:height=format!("{}px", f64::from(total) * ROW)
            >
                {slot_views}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_move_the_active_row_and_bring_it_into_view() {
        // With no row active, the first key starts from the first row in view.
        assert_eq!(key_target("ArrowDown", None, 100, 5_000, 14), Some(100));
        assert_eq!(
            key_target("ArrowDown", Some(100), 100, 5_000, 14),
            Some(101)
        );
        assert_eq!(key_target("ArrowUp", Some(0), 0, 5_000, 14), Some(0));
        assert_eq!(
            key_target("PageDown", Some(4_990), 0, 5_000, 14),
            Some(4_999)
        );
        assert_eq!(key_target("PageUp", Some(10), 0, 5_000, 14), Some(0));
        assert_eq!(key_target("Home", Some(10), 0, 5_000, 14), Some(0));
        assert_eq!(key_target("End", None, 0, 5_000, 14), Some(4_999));
        assert_eq!(key_target("Tab", Some(10), 0, 5_000, 14), None);
        assert_eq!(key_target("End", None, 0, 0, 14), None, "no rows");
        // A row below the view scrolls up into it; one above, down; one in view stays.
        let height = 14.0 * ROW;
        assert_eq!(scroll_for(20, 0.0, height, ROW), Some(21.0 * ROW - height));
        assert_eq!(scroll_for(3, 10.0 * ROW, height, ROW), Some(3.0 * ROW));
        assert_eq!(scroll_for(12, 0.0, height, ROW), None);
    }

    #[test]
    fn the_window_covers_the_view_and_the_overscan() {
        // 15 rows in view at the top of 5,000: rows 0 to 15 (one half in view) and 6 more.
        assert_eq!(visible_window(0.0, 480.0, ROW, 5_000, 6), (0, 22));
        // Scrolled to row 100: six before it, then the view and six after.
        assert_eq!(visible_window(100.0 * ROW, 480.0, ROW, 5_000, 6), (94, 122));
        // Half a row in: the first row is still in view.
        assert_eq!(
            visible_window(100.0 * ROW + 16.0, 480.0, ROW, 5_000, 6).0,
            94
        );
        // At the end, the window stops at the last row.
        assert_eq!(
            visible_window(4_990.0 * ROW, 480.0, ROW, 5_000, 6),
            (4_984, 5_000)
        );
        // A short list is all in view.
        assert_eq!(visible_window(0.0, 96.0, ROW, 3, 6), (0, 3));
        assert_eq!(visible_window(0.0, 480.0, ROW, 0, 6), (0, 0));
        // The rows in view, apart from the overscan.
        assert_eq!(
            Window::of(110.0 * ROW, 448.0, ROW, 5_000, 6),
            Window {
                rows: (104, 131),
                seen: (110, 125)
            }
        );
    }

    #[test]
    fn each_row_of_the_window_has_one_slot() {
        let slots = slot_count(480.0, ROW, 5_000, 6);
        assert_eq!(slots, 28);
        assert_eq!(slot_count(96.0, ROW, 3, 6), 3, "never more than the rows");
        for first in [0, 1, 27, 28, 94, 4_972] {
            let mut rows: Vec<u32> = (0..slots)
                .map(|slot| slot_row(slot, first, slots))
                .collect();
            rows.sort_unstable();
            assert_eq!(rows, (first..first + slots).collect::<Vec<_>>(), "{first}");
        }
        // Assistive technology reads the slots in the order of their rows.
        assert_eq!(slots_in_row_order(94, 27)[..3], [13, 14, 15]);
        let order = slots_in_row_order(94, 27);
        for (place, slot) in order.iter().enumerate() {
            assert_eq!(slot_row(*slot, 94, 27), 94 + place as u32);
        }
        // Scrolling one row moves one slot; the others keep their rows.
        let before: Vec<u32> = (0..slots).map(|slot| slot_row(slot, 94, slots)).collect();
        let after: Vec<u32> = (0..slots).map(|slot| slot_row(slot, 95, slots)).collect();
        let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        assert_eq!(moved, 1);
    }
}
