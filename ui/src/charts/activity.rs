//! The log activity column chart's geometry: the lines per minute over the last hour as
//! stacked columns on a nice scale, the segment paths and the tooltip's placement. The view is
//! [`crate::components::log_insights::LogActivityChart`]. Pure Rust, tested natively.

use crate::{
    charts::{nice_ticks, tick_labels},
    insights::{ACTIVITY_MINUTES, MinuteBucket},
};

/// The chart's drawing space; it is scaled to the card's width.
pub const VIEW_WIDTH: f32 = 640.0;
pub const VIEW_HEIGHT: f32 = 160.0;
pub const PLOT_LEFT: f32 = 36.0;
pub const PLOT_RIGHT: f32 = 634.0;
pub const PLOT_TOP: f32 = 8.0;
pub const PLOT_BOTTOM: f32 = 140.0;
/// The surface-coloured gap between stacked segments and between columns.
pub const GAP: f32 = 2.0;
pub const MAX_COLUMN_WIDTH: f32 = 24.0;
pub const CORNER: f32 = 4.0;
/// Which columns carry a time label.
pub const LABELLED_COLUMNS: [usize; 4] = [0, 20, 40, ACTIVITY_MINUTES - 1];

/// One kind of line in the activity chart, bottom of the stack first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Info,
    Other,
    Warn,
    Error,
}

impl Kind {
    pub const STACK: [Kind; 4] = [Kind::Info, Kind::Other, Kind::Warn, Kind::Error];

    /// The legend entries, read top to bottom as the stack is: most urgent first.
    pub const LEGEND: [Kind; 4] = [Kind::Error, Kind::Warn, Kind::Info, Kind::Other];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Info => "INFO",
            Kind::Other => "Other",
            Kind::Warn => "WARN",
            Kind::Error => "ERROR and stderr",
        }
    }

    pub fn class(self) -> &'static str {
        match self {
            Kind::Info => "insight-seg insight-seg-info",
            Kind::Other => "insight-seg insight-seg-other",
            Kind::Warn => "insight-seg insight-seg-warn",
            Kind::Error => "insight-seg insight-seg-error",
        }
    }

    pub fn key_class(self) -> &'static str {
        match self {
            Kind::Info => "chart-key-block insight-seg-info",
            Kind::Other => "chart-key-block insight-seg-other",
            Kind::Warn => "chart-key-block insight-seg-warn",
            Kind::Error => "chart-key-block insight-seg-error",
        }
    }

    pub fn count(self, bucket: &MinuteBucket) -> u32 {
        match self {
            Kind::Info => bucket.info,
            Kind::Other => bucket.other,
            Kind::Warn => bucket.warn,
            Kind::Error => bucket.error,
        }
    }
}

/// A drawn piece of a column.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub kind: Kind,
    pub y: f32,
    pub height: f32,
    /// The topmost segment gets rounded corners.
    pub top: bool,
}

/// A drawn column.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    /// The left edge of the column's slot, the hover target.
    pub slot_x: f32,
    pub slot_width: f32,
    pub x: f32,
    pub width: f32,
    pub segments: Vec<Segment>,
}

impl Column {
    pub fn center(&self) -> f32 {
        self.slot_x + self.slot_width / 2.0
    }
}

/// Where everything goes, for `buckets` oldest first.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// The gridlines above the axis, `(value, label)`, on the line charts' clean steps; the
    /// last is the top of the scale.
    pub ticks: Vec<(u32, String)>,
    /// The value at the top of the plot.
    pub top: u32,
    pub columns: Vec<Column>,
}

impl Geometry {
    pub fn y_of(&self, value: u32) -> f32 {
        let top = self.top.max(1) as f32;
        PLOT_BOTTOM - (value as f32 / top) * (PLOT_BOTTOM - PLOT_TOP)
    }
}

/// Lays out `buckets` (oldest first) as stacked columns on a scale with the same ticks the
/// line charts would draw for it.
pub fn geometry(buckets: &[MinuteBucket]) -> Geometry {
    let max = buckets.iter().map(MinuteBucket::total).max().unwrap_or(0);
    let values = nice_ticks(0.0, f64::from(max), 3);
    let ticks: Vec<(u32, String)> = values
        .iter()
        .zip(tick_labels(&values))
        .map(|(value, label)| (value.round() as u32, label))
        .filter(|(value, _)| *value > 0)
        .collect();
    let top = ticks.last().map_or(1, |(value, _)| *value);
    let mut layout = Geometry {
        ticks,
        top,
        columns: Vec::with_capacity(buckets.len()),
    };
    let slots = buckets.len().max(1) as f32;
    let slot_width = (PLOT_RIGHT - PLOT_LEFT) / slots;
    let width = (slot_width - GAP).clamp(1.0, MAX_COLUMN_WIDTH);
    for (index, bucket) in buckets.iter().enumerate() {
        let slot_x = PLOT_LEFT + index as f32 * slot_width;
        let x = slot_x + (slot_width - width) / 2.0;
        let mut segments = Vec::new();
        let mut stacked = 0;
        let non_empty = Kind::STACK
            .iter()
            .filter(|kind| kind.count(bucket) > 0)
            .count();
        for kind in Kind::STACK {
            let count = kind.count(bucket);
            if count == 0 {
                continue;
            }
            let bottom = layout.y_of(stacked);
            stacked += count;
            let top = layout.y_of(stacked);
            let raw_height = bottom - top;
            // The gap sits at the bottom of every segment above the baseline one; a sliver
            // keeps at least one pixel so a single line is still visible.
            let has_below = !segments.is_empty();
            let height = if has_below {
                (raw_height - GAP).max(raw_height.min(1.0))
            } else {
                raw_height.max(1.0)
            };
            let y = if has_below { top } else { bottom - height };
            segments.push(Segment {
                kind,
                y,
                height,
                top: segments.len() + 1 == non_empty,
            });
        }
        layout.columns.push(Column {
            slot_x,
            slot_width,
            x,
            width,
            segments,
        });
    }
    layout
}

/// A column segment as an SVG path: square at the bottom and, when `rounded`, rounded at the
/// top by up to [`CORNER`].
pub fn segment_path(x: f32, y: f32, width: f32, height: f32, rounded: bool) -> String {
    let r = if rounded {
        CORNER.min(width / 2.0).min(height)
    } else {
        0.0
    };
    if r <= 0.0 {
        return format!("M{x:.1},{y:.1}h{width:.1}v{height:.1}h{:.1}z", -width);
    }
    format!(
        "M{x:.1},{:.1}a{r:.1},{r:.1} 0 0 1 {r:.1},{:.1}h{:.1}a{r:.1},{r:.1} 0 0 1 {r:.1},{r:.1}v{:.1}h{:.1}z",
        y + r,
        -r,
        width - 2.0 * r,
        height - r,
        -width,
    )
}

/// The tooltip's class for a column centred at `center`: centred on the column, or hung to
/// the right of a column near the left edge and to the left of one near the right edge, so
/// it stays inside the plot.
pub fn tooltip_class(center: f32) -> &'static str {
    let fraction = center / VIEW_WIDTH;
    if fraction < 0.15 {
        "chart-tooltip chart-tooltip-start"
    } else if fraction > 0.85 {
        "chart-tooltip chart-tooltip-end"
    } else {
        "chart-tooltip chart-tooltip-centred"
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn bucket(minute: u64, info: u32, warn: u32, error: u32, other: u32) -> MinuteBucket {
        MinuteBucket {
            minute_start: UNIX_EPOCH + Duration::from_secs(minute * 60),
            info,
            warn,
            error,
            other,
        }
    }

    fn tick_values(layout: &Geometry) -> Vec<u32> {
        layout.ticks.iter().map(|(value, _)| *value).collect()
    }

    #[test]
    fn geometry_stacks_with_gaps_and_a_nice_scale() {
        let buckets: Vec<MinuteBucket> = (0..ACTIVITY_MINUTES as u64)
            .map(|i| match i {
                59 => bucket(i, 40, 5, 2, 3),
                10 => bucket(i, 1, 0, 0, 0),
                _ => bucket(i, 0, 0, 0, 0),
            })
            .collect();
        let layout = geometry(&buckets);
        // 50 lines at most: the line charts' 1-2-5 steps give 20, 40, 60.
        assert_eq!(tick_values(&layout), [20, 40, 60]);
        assert_eq!(layout.top, 60);
        assert_eq!(layout.ticks[0].1, "20");
        assert_eq!(layout.columns.len(), ACTIVITY_MINUTES);
        let slot = (PLOT_RIGHT - PLOT_LEFT) / 60.0;
        let column = &layout.columns[59];
        assert!((column.slot_width - slot).abs() < 0.01);
        assert!(column.width <= MAX_COLUMN_WIDTH && column.width <= slot - GAP);
        assert_eq!(column.segments.len(), 4);
        assert_eq!(column.segments[0].kind, Kind::Info);
        assert_eq!(column.segments[3].kind, Kind::Error);
        assert!(column.segments[3].top && !column.segments[0].top);
        // The baseline segment touches the axis; the ones above leave a gap below them.
        let info = &column.segments[0];
        assert!((info.y + info.height - PLOT_BOTTOM).abs() < 0.01);
        for pair in column.segments.windows(2) {
            let gap = pair[0].y - (pair[1].y + pair[1].height);
            assert!((gap - GAP).abs() < 0.01, "gap {gap}");
        }
        // The total height matches the scale.
        let top = column.segments[3].y;
        assert!((top - layout.y_of(50)).abs() < 0.01);
        // A single line is still a visible sliver.
        let sliver = &layout.columns[10];
        assert_eq!(sliver.segments.len(), 1);
        assert!(sliver.segments[0].height >= 1.0);
        assert!(sliver.segments[0].top);
        // Empty columns draw nothing.
        assert!(layout.columns[0].segments.is_empty());
        // Nothing at all still has a scale, above zero.
        let empty = geometry(&[]);
        assert_eq!(tick_values(&empty), [1, 2]);
        assert_eq!(empty.top, 2);
        assert!(empty.columns.is_empty());
        // Thousands of lines a minute: the labels are compact, as on the line charts.
        let busy = [bucket(0, 25_000, 0, 0, 0)];
        let layout = geometry(&busy);
        assert_eq!(tick_values(&layout), [10_000, 20_000, 30_000]);
        let labels: Vec<&str> = layout
            .ticks
            .iter()
            .map(|(_, label)| label.as_str())
            .collect();
        assert_eq!(labels, ["10K", "20K", "30K"]);
    }

    #[test]
    fn tooltip_hangs_inward_near_the_edges() {
        let buckets: Vec<MinuteBucket> = (0..ACTIVITY_MINUTES as u64)
            .map(|i| bucket(i, 1, 0, 0, 0))
            .collect();
        let layout = geometry(&buckets);
        let class_of = |index: usize| tooltip_class(layout.columns[index].center());
        // The oldest and newest minutes (Home and End) would otherwise overhang the plot.
        assert_eq!(class_of(0), "chart-tooltip chart-tooltip-start");
        assert_eq!(class_of(5), "chart-tooltip chart-tooltip-start");
        assert_eq!(class_of(6), "chart-tooltip chart-tooltip-centred");
        assert_eq!(class_of(30), "chart-tooltip chart-tooltip-centred");
        assert_eq!(class_of(50), "chart-tooltip chart-tooltip-centred");
        assert_eq!(class_of(51), "chart-tooltip chart-tooltip-end");
        assert_eq!(
            class_of(ACTIVITY_MINUTES - 1),
            "chart-tooltip chart-tooltip-end"
        );
    }

    #[test]
    fn segment_paths() {
        assert_eq!(
            segment_path(10.0, 20.0, 8.0, 30.0, false),
            "M10.0,20.0h8.0v30.0h-8.0z"
        );
        let rounded = segment_path(10.0, 20.0, 8.0, 30.0, true);
        assert!(
            rounded.starts_with(
                "M10.0,24.0a4.0,4.0 0 0 1 4.0,-4.0h0.0a4.0,4.0 0 0 1 4.0,4.0v26.0h-8.0z"
            ),
            "{rounded}"
        );
        // The corner shrinks to fit a short segment.
        let short = segment_path(0.0, 0.0, 8.0, 2.0, true);
        assert!(short.contains("a2.0,2.0"), "{short}");
    }

    #[test]
    fn kinds() {
        assert_eq!(Kind::LEGEND.len(), Kind::STACK.len());
        for kind in Kind::STACK {
            assert!(!kind.label().is_empty());
        }
    }
}
