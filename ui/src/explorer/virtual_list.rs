//! A virtual list: rows of a fixed height in a scroller of a fixed height, of which only the
//! ones in view and a few either side are in the page. Each row slot is made once and keyed
//! by its index modulo the slots, so scrolling moves slots and updates their text rather than
//! building rows. The scroll position is read once a frame, and the page hears of the rows in
//! view once scrolling settles, so a fling asks the node for nothing. An ARIA grid with
//! `aria-rowcount` and each row's `aria-rowindex`; the scroller takes focus, so the keys that
//! scroll work.

use std::time::Duration;

use leptos::{html, prelude::*};

/// A row's height, in pixels.
pub const ROW: f64 = 32.0;
/// Rows kept either side of the ones in view.
pub const OVERSCAN: u32 = 6;
/// How long scrolling must rest before the page hears of the rows in view.
const SETTLE: Duration = Duration::from_millis(120);

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

/// A virtual list of `total` rows, at most `rows_shown` of them in view at once, under a
/// header row. `row` builds one slot's cells, once, from the row index it shows (a signal:
/// the cells read it). `on_window` hears of the rows in view, overscan included, once
/// scrolling settles and when the list first shows.
#[component]
pub fn VirtualList<H, HV, R, V>(
    total: u32,
    rows_shown: u32,
    /// The grid's name for assistive technology.
    label: String,
    /// The header row's cells.
    header: H,
    row: R,
    on_window: Callback<(u32, u32)>,
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
    let window = Memo::new(move |_| visible_window(top.get(), height, ROW, total, OVERSCAN));
    let scroller = NodeRef::<html::Div>::new();
    let frame_pending = StoredValue::new(false);
    let settle = StoredValue::new(0u64);

    // Scroll events come faster than frames: the position is read once a frame.
    let on_scroll = move |_| {
        if frame_pending.get_value() {
            return;
        }
        frame_pending.set_value(true);
        request_animation_frame(move || {
            frame_pending.try_set_value(false);
            if let Some(scroller) = scroller.get_untracked() {
                top.try_set(f64::from(scroller.scroll_top()));
            }
        });
    };
    // The page hears of the rows in view once they rest.
    Effect::new(move |_| {
        let (first, end) = window.get();
        let mine = settle.get_value() + 1;
        settle.set_value(mine);
        set_timeout(
            move || {
                if settle.try_get_value() == Some(mine) {
                    on_window.run((first, end));
                }
            },
            SETTLE,
        );
    });
    let slot_views = (0..slots)
        .map(|slot| {
            // Near the end the slots keep to the last rows, so none shows a row past them.
            let index = Memo::new(move |_| {
                slot_row(slot, window.get().0.min(total - slots), slots)
            });
            let cells = row(index);
            view! {
                <div
                    class="ex-vrow"
                    role="row"
                    aria-rowindex=move || (index.get() + 2).to_string()
                    style:transform=move || format!("translateY({}px)", f64::from(index.get()) * ROW)
                >
                    {cells}
                </div>
            }
        })
        .collect_view();
    view! {
        <div
            class="ex-vlist"
            role="grid"
            aria-label=label
            aria-rowcount=(total + 1).to_string()
            tabindex="0"
            node_ref=scroller
            style:height=format!("{}px", height + ROW)
            on:scroll=on_scroll
        >
            <div class="ex-vrow ex-vhead" role="row" aria-rowindex="1">{header()}</div>
            <div class="ex-vbody" role="rowgroup" style:height=format!("{}px", f64::from(total) * ROW)>
                {slot_views}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Scrolling one row moves one slot; the others keep their rows.
        let before: Vec<u32> = (0..slots).map(|slot| slot_row(slot, 94, slots)).collect();
        let after: Vec<u32> = (0..slots).map(|slot| slot_row(slot, 95, slots)).collect();
        let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        assert_eq!(moved, 1);
    }
}
