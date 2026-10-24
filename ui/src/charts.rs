//! Inline SVG charts and stat tiles for the dashboard, with no chart library.
//!
//! This file holds the pure geometry and formatting helpers, unit-tested natively. The Leptos
//! views live in [`line_chart`] (the chart cards) and [`widgets`] (range control, stat tiles,
//! sparklines, the sync meter).

pub mod line_chart;
pub mod widgets;

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use launcher_types::Phase;

use crate::{
    format::{rough_duration, thousands},
    stats_history::{Field, Sample},
};

/// The chart's drawing surface, in `viewBox` units: `0 0 640 220`.
pub const VIEW_W: f64 = 640.0;
pub const VIEW_H: f64 = 220.0;
/// Room for the y-axis labels.
pub const PAD_L: f64 = 64.0;
/// Room for the end-dot and the direct label after the last sample.
pub const PAD_R: f64 = 56.0;
pub const PAD_T: f64 = 14.0;
/// The x-axis band.
pub const PAD_B: f64 = 30.0;

/// The sparkline's surface: `0 0 120 32`.
pub const SPARK_W: f64 = 120.0;
pub const SPARK_H: f64 = 32.0;

/// At most this many sparkline points and table rows.
pub const MAX_POINTS: usize = 60;

/// The time range the dashboard's rates, tiles and charts cover.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    Min15,
    #[default]
    Hour1,
    Hour4,
    All,
}

impl Range {
    pub const ALL: [Range; 4] = [Range::Min15, Range::Hour1, Range::Hour4, Range::All];

    pub fn label(self) -> &'static str {
        match self {
            Range::Min15 => "15 min",
            Range::Hour1 => "1 h",
            Range::Hour4 => "4 h",
            Range::All => "All",
        }
    }

    /// How far back the range reaches; `None` is everything since the launcher opened.
    pub fn span(self) -> Option<Duration> {
        match self {
            Range::Min15 => Some(Duration::from_secs(15 * 60)),
            Range::Hour1 => Some(Duration::from_secs(60 * 60)),
            Range::Hour4 => Some(Duration::from_secs(4 * 60 * 60)),
            Range::All => None,
        }
    }

    /// Words for a delta or an average: `+1,240 in the last hour`.
    pub fn phrase(self) -> &'static str {
        match self {
            Range::Min15 => "in the last 15 min",
            Range::Hour1 => "in the last hour",
            Range::Hour4 => "in the last 4 h",
            Range::All => "since the launcher opened",
        }
    }

    /// The range `by` steps to the right (negative: left), wrapping around.
    pub fn step(self, by: i32) -> Range {
        let index = Range::ALL.iter().position(|r| *r == self).unwrap_or(0) as i32;
        let len = Range::ALL.len() as i32;
        Range::ALL[(index + by).rem_euclid(len) as usize]
    }
}

/// The unit a magnitude is written in: `1e3` and `K` for thousands, up to trillions.
fn unit_for(magnitude: f64) -> (f64, &'static str) {
    const UNITS: [(f64, &str); 4] = [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")];
    UNITS
        .into_iter()
        .find(|(unit, _)| magnitude >= *unit)
        .unwrap_or((1.0, ""))
}

/// A count with three significant figures: `999`, `12.3K`, `2.95M`, `1.20B`.
pub fn compact(value: u64) -> String {
    if value < 1000 {
        return value.to_string();
    }
    let value = value as f64;
    // 999,999 reads `1.00M`, not `1000K`: pick the unit after rounding to three figures.
    let (unit, suffix) = unit_for(value * (1.0 + 5e-4));
    let scaled = value / unit;
    let text = if scaled < 9.995 {
        format!("{scaled:.2}")
    } else if scaled < 99.95 {
        format!("{scaled:.1}")
    } else {
        format!("{scaled:.0}")
    };
    format!("{text}{suffix}")
}

/// Clean tick values covering `min..=max`: a 1, 2 or 5 step, at most `count + 1` ticks, the
/// first at or below `min` and the last at or above `max`.
pub fn nice_ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    let (mut lo, mut hi) = if min <= max { (min, max) } else { (max, min) };
    if !lo.is_finite() || !hi.is_finite() {
        return vec![0.0, 1.0];
    }
    if hi - lo <= f64::EPSILON * hi.abs().max(1.0) {
        let pad = (hi.abs() * 0.001).max(1.0);
        lo -= pad;
        hi += pad;
    }
    let max_ticks = count.max(2) + 1;
    let raw = (hi - lo) / (max_ticks - 1) as f64;
    let mut magnitude = 10f64.powf(raw.log10().floor());
    loop {
        for factor in [1.0, 2.0, 5.0] {
            let step = factor * magnitude;
            let start = (lo / step).floor() * step;
            let end = (hi / step).ceil() * step;
            let steps = ((end - start) / step).round() as usize;
            if steps < max_ticks {
                return (0..=steps).map(|i| start + step * i as f64).collect();
            }
        }
        magnitude *= 10.0;
    }
}

/// Labels for value ticks: compact, with just enough decimals to tell neighbours apart, or
/// the full number when even three decimals would not.
pub fn tick_labels(ticks: &[f64]) -> Vec<String> {
    let full = |tick: &f64| thousands(tick.round().max(0.0) as u64);
    let max = ticks.iter().fold(0.0f64, |max, tick| max.max(tick.abs()));
    if max < 1000.0 {
        return ticks.iter().map(full).collect();
    }
    let (unit, suffix) = unit_for(max);
    for decimals in 0..=3 {
        let labels: Vec<String> = ticks
            .iter()
            .map(|tick| format!("{:.*}{suffix}", decimals, tick / unit))
            .collect();
        let distinct = labels
            .iter()
            .enumerate()
            .all(|(i, label)| labels[..i].iter().all(|other| other != label));
        if distinct {
            return labels;
        }
    }
    ticks.iter().map(full).collect()
}

/// A linear map from a data domain to a drawing range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    domain: (f64, f64),
    range: (f64, f64),
}

pub fn scale(domain: (f64, f64), range: (f64, f64)) -> Scale {
    Scale { domain, range }
}

impl Scale {
    pub fn apply(&self, value: f64) -> f64 {
        let (d0, d1) = self.domain;
        let (r0, r1) = self.range;
        if (d1 - d0).abs() <= f64::EPSILON {
            return (r0 + r1) / 2.0;
        }
        r0 + (value - d0) / (d1 - d0) * (r1 - r0)
    }
}

/// An SVG path through the points, one decimal per coordinate: `M64.0,190.0 L80.5,120.2`.
pub fn line_path(points: &[(f64, f64)]) -> String {
    let mut d = String::with_capacity(points.len() * 14);
    for (index, (x, y)) in points.iter().enumerate() {
        if index > 0 {
            d.push(' ');
        }
        let _ = write!(d, "{}{x:.1},{y:.1}", if index == 0 { 'M' } else { 'L' });
    }
    d
}

/// The line closed down to `baseline`, for an area wash.
pub fn area_path(points: &[(f64, f64)], baseline: f64) -> String {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return String::new();
    };
    let mut d = line_path(points);
    let _ = write!(
        d,
        " L{:.1},{baseline:.1} L{:.1},{baseline:.1} Z",
        last.0, first.0
    );
    d
}

/// The index of the value in the sorted `xs` closest to `x`; 0 when empty.
pub fn nearest_index(xs: &[f64], x: f64) -> usize {
    if xs.is_empty() {
        return 0;
    }
    let after = xs.partition_point(|&v| v < x);
    if after == 0 {
        return 0;
    }
    if after >= xs.len() {
        return xs.len() - 1;
    }
    if x - xs[after - 1] <= xs[after] - x {
        after - 1
    } else {
        after
    }
}

/// Indices that pick at most `max_rows` of `len` items evenly, first and last included.
pub fn downsample_indices(len: usize, max_rows: usize) -> Vec<usize> {
    if len <= max_rows {
        return (0..len).collect();
    }
    match max_rows {
        0 => Vec::new(),
        1 => vec![len - 1],
        _ => (0..max_rows)
            .map(|i| i * (len - 1) / (max_rows - 1))
            .collect(),
    }
}

/// At most `max_rows` of the items, picked evenly, first and last included.
pub fn downsample<T: Copy>(items: &[T], max_rows: usize) -> Vec<T> {
    downsample_indices(items.len(), max_rows)
        .into_iter()
        .map(|index| items[index])
        .collect()
}

fn epoch_secs(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Seconds into the day at `time` in a zone `tz_offset_min` minutes behind UTC (the
/// `Date.getTimezoneOffset()` convention; 0 is UTC).
fn day_seconds(time: SystemTime, tz_offset_min: i32) -> u64 {
    let local = epoch_secs(time) - i64::from(tz_offset_min) * 60;
    local.rem_euclid(86_400) as u64
}

/// `18:35` at `time`, in the zone `tz_offset_min` minutes behind UTC.
pub fn hhmm(time: SystemTime, tz_offset_min: i32) -> String {
    let secs = day_seconds(time, tz_offset_min);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// `18:35:01` at `time`, in the zone `tz_offset_min` minutes behind UTC.
pub fn hhmmss(time: SystemTime, tz_offset_min: i32) -> String {
    let secs = day_seconds(time, tz_offset_min);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The browser's zone, as minutes behind UTC, for [`hhmm`] and [`hhmmss`].
pub fn tz_offset_min() -> i32 {
    js_sys::Date::new_0().get_timezone_offset() as i32
}

/// Tick times for the x axis: at most four clock times on a clean step (15 s up to a day),
/// aligned to the local midnight of the zone `tz_offset_min` minutes behind UTC, inside
/// `start..=end`. The flag says whether the labels need seconds to differ.
pub fn time_ticks(
    start: SystemTime,
    end: SystemTime,
    tz_offset_min: i32,
) -> (Vec<SystemTime>, bool) {
    const STEPS: [i64; 14] = [
        15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 14_400, 21_600, 43_200, 86_400,
    ];
    let shift = i64::from(tz_offset_min) * 60;
    let (lo, hi) = (epoch_secs(start) - shift, epoch_secs(end) - shift);
    for step in STEPS {
        let first = lo + (step - lo.rem_euclid(step)) % step;
        if first > hi {
            // No clean time of this step inside the span, so none of a larger step either.
            break;
        }
        let count = (hi - first) / step + 1;
        if count <= 4 {
            let ticks = (0..count)
                .map(|i| {
                    let secs = (first + i * step + shift).max(0) as u64;
                    UNIX_EPOCH + Duration::from_secs(secs)
                })
                .collect();
            return (ticks, step < 60);
        }
    }
    (Vec::new(), false)
}

/// `about 2 h left`, `about 12 min left`, `under a minute left`.
pub fn eta_text(eta: Duration) -> String {
    if eta < Duration::from_secs(60) {
        "under a minute left".to_owned()
    } else {
        format!("about {} left", rough_duration(eta))
    }
}

/// An uptime: `45 s`, `12 min`, `3 h 5 min`, `2 d 4 h`.
pub fn uptime_text(uptime: Duration) -> String {
    let secs = uptime.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3600),
    }
}

/// A delta with its sign and thousands separators: `+1,240`, `-3`, `0`.
pub fn signed(delta: i64) -> String {
    let magnitude = thousands(delta.unsigned_abs());
    match delta {
        0 => magnitude,
        d if d > 0 => format!("+{magnitude}"),
        _ => format!("-{magnitude}"),
    }
}

/// A progress fraction with one decimal, rounded down so `99.99 %` never reads `100.0 %`.
pub fn percent_1(fraction: f64) -> String {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let tenths = (fraction * 1000.0 + 1e-6).floor() / 10.0;
    format!("{tenths:.1} %")
}

/// The words of the sync hero's line for a phase other than running.
pub fn phase_note(phase: &Phase) -> Option<&'static str> {
    match phase {
        Phase::Running => None,
        Phase::Starting => Some("zebrad is starting"),
        Phase::Stopping => Some("zebrad is stopping"),
        Phase::Stopped => Some("zebrad is stopped"),
        Phase::Backoff { .. } => Some("zebrad is restarting"),
        Phase::Failed { .. } => Some("zebrad failed"),
    }
}

/// What the sync hero shows.
#[derive(Clone, Debug, PartialEq)]
pub struct SyncSummary {
    /// The hero number: `94.6 %`, `100 %`, or `—` without data.
    pub figure: String,
    /// The line under the meter.
    pub line: String,
    /// The meter's fill, 0 to 1.
    pub fraction: f64,
    /// Synced: the line gets a check icon.
    pub at_tip: bool,
    /// No data yet: the line is muted.
    pub waiting: bool,
}

/// The sync hero's texts from the newest sample, the range's rate and ETA, and the phase.
pub fn sync_summary(
    latest: Option<&Sample>,
    rate: Option<f64>,
    eta: Option<Duration>,
    note: Option<&str>,
) -> SyncSummary {
    let Some(sample) = latest else {
        return SyncSummary {
            figure: "—".to_owned(),
            line: "Waiting for zebrad's RPC…".to_owned(),
            fraction: 0.0,
            at_tip: false,
            waiting: true,
        };
    };
    let mut parts = Vec::with_capacity(4);
    let (figure, fraction) = if sample.at_tip() {
        parts.push(format!(
            "At chain tip · height {}",
            thousands(sample.height)
        ));
        ("100 %".to_owned(), 1.0)
    } else {
        parts.push(format!(
            "{} of {} blocks",
            thousands(sample.height),
            thousands(sample.estimated_height)
        ));
        match rate {
            None => parts.push("estimating…".to_owned()),
            Some(rate) => {
                parts.push(format!(
                    "{} blocks/min",
                    thousands(rate.round().max(0.0) as u64)
                ));
                parts.push(match eta {
                    Some(eta) => eta_text(eta),
                    None => "not catching up".to_owned(),
                });
            }
        }
        (
            percent_1(sample.verification_progress),
            sample.verification_progress.clamp(0.0, 1.0),
        )
    };
    if let Some(note) = note {
        parts.push(note.to_owned());
    }
    SyncSummary {
        figure,
        line: parts.join(" · "),
        fraction,
        at_tip: sample.at_tip(),
        waiting: false,
    }
}

/// The paths of a sparkline over `values`: the whole line, its last segment, and the end point.
/// `None` with fewer than two values.
pub fn spark_paths(values: &[f64]) -> Option<(String, String, (f64, f64))> {
    if values.len() < 2 {
        return None;
    }
    const PAD: f64 = 3.0;
    let (lo, hi) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(*v), hi.max(*v))
        });
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let x = scale((0.0, (values.len() - 1) as f64), (PAD, SPARK_W - PAD));
    let y = scale((lo, hi), (SPARK_H - PAD, PAD));
    let points: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (x.apply(i as f64), y.apply(*v)))
        .collect();
    let last = points[points.len() - 1];
    Some((
        line_path(&points),
        line_path(&points[points.len() - 2..]),
        last,
    ))
}

/// One line of a chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub name: &'static str,
    /// The CSS class that sets `--series`: `chart-series-1` or `chart-series-muted`.
    pub class: &'static str,
    pub values: Vec<f64>,
}

/// What a chart plots: sample times and one value per series per time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartData {
    pub times: Vec<SystemTime>,
    pub series: Vec<Series>,
}

impl ChartData {
    /// The window's samples as series, one per `(name, class, field)`.
    pub fn from_samples(
        samples: &[Sample],
        fields: &[(&'static str, &'static str, Field)],
    ) -> Self {
        ChartData {
            times: samples.iter().map(|s| s.at).collect(),
            series: fields
                .iter()
                .map(|&(name, class, field)| Series {
                    name,
                    class,
                    values: samples.iter().map(|s| s.get(field) as f64).collect(),
                })
                .collect(),
        }
    }

    pub fn len(&self) -> usize {
        self.times.len()
    }
}

/// Everything the chart view draws, in `viewBox` units.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// The x of every sample.
    pub xs: Vec<f64>,
    /// The y of every sample, per series.
    pub ys: Vec<Vec<f64>>,
    /// One path per series.
    pub lines: Vec<String>,
    /// The first series closed down to the baseline.
    pub area: String,
    /// `(y, label)` per horizontal gridline.
    pub y_ticks: Vec<(f64, String)>,
    /// `(x, label)` per x-axis label.
    pub x_ticks: Vec<(f64, String)>,
    /// The label at the end of the first series.
    pub end_label: String,
}

/// Lays out `data`; `None` with fewer than two samples. The y domain has nice bounds around
/// the values, from zero when `from_zero`.
pub fn geometry(data: &ChartData, from_zero: bool, tz_offset_min: i32) -> Option<Geometry> {
    let n = data.len();
    let (first, last) = (*data.times.first()?, *data.times.last()?);
    let span = last.duration_since(first).ok()?.as_secs_f64();
    if n < 2 || data.series.is_empty() || span <= 0.0 {
        return None;
    }
    let x = scale((0.0, span), (PAD_L, VIEW_W - PAD_R));
    let since_first =
        |time: SystemTime| time.duration_since(first).map_or(0.0, |d| d.as_secs_f64());
    let xs: Vec<f64> = data
        .times
        .iter()
        .map(|t| x.apply(since_first(*t)))
        .collect();

    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for value in data.series.iter().flat_map(|s| s.values.iter()) {
        lo = lo.min(*value);
        hi = hi.max(*value);
    }
    if from_zero {
        lo = lo.min(0.0);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let ticks = nice_ticks(lo, hi, 3);
    let baseline = VIEW_H - PAD_B;
    let y = scale((ticks[0], ticks[ticks.len() - 1]), (baseline, PAD_T));
    let y_ticks = ticks
        .iter()
        .zip(tick_labels(&ticks))
        .map(|(tick, label)| (y.apply(*tick), label))
        .collect();
    let (times, with_seconds) = time_ticks(first, last, tz_offset_min);
    let x_ticks = times
        .iter()
        .map(|time| {
            let label = if with_seconds {
                hhmmss(*time, tz_offset_min)
            } else {
                hhmm(*time, tz_offset_min)
            };
            (x.apply(since_first(*time)), label)
        })
        .collect();

    let ys: Vec<Vec<f64>> = data
        .series
        .iter()
        .map(|s| s.values.iter().map(|v| y.apply(*v)).collect())
        .collect();
    let points = |series: &[f64]| -> Vec<(f64, f64)> {
        xs.iter().copied().zip(series.iter().copied()).collect()
    };
    let lines = ys.iter().map(|series| line_path(&points(series))).collect();
    let area = area_path(&points(&ys[0]), baseline);
    let end_label = compact(data.series[0].values[n - 1].round().max(0.0) as u64);
    Some(Geometry {
        xs,
        ys,
        lines,
        area,
        y_ticks,
        x_ticks,
        end_label,
    })
}

/// The chart in one sentence, for its `aria-label`.
pub fn summary(title: &str, data: &ChartData, tz_offset_min: i32) -> String {
    let n = data.len();
    let (Some(first), Some(last)) = (data.times.first(), data.times.last()) else {
        return format!("{title}: no samples yet");
    };
    let latest: Vec<String> = data
        .series
        .iter()
        .filter_map(|s| {
            s.values
                .last()
                .map(|v| format!("{} {}", s.name, thousands(v.round().max(0.0) as u64)))
        })
        .collect();
    format!(
        "{title}: {n} samples from {} to {}. Latest: {}.",
        hhmmss(*first, tz_offset_min),
        hhmmss(*last, tz_offset_min),
        latest.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sample(secs: u64, height: u64, peers: u32) -> Sample {
        Sample {
            at: at(secs),
            height,
            estimated_height: 3_120_000,
            peers,
            mempool_transactions: 10,
            verification_progress: height as f64 / 3_120_000.0,
        }
    }

    #[test]
    fn range_steps_and_words() {
        assert_eq!(Range::default(), Range::Hour1);
        assert_eq!(Range::Hour1.step(1), Range::Hour4);
        assert_eq!(Range::Min15.step(-1), Range::All);
        assert_eq!(Range::All.step(1), Range::Min15);
        assert_eq!(Range::Hour4.span(), Some(Duration::from_secs(14_400)));
        assert_eq!(Range::All.span(), None);
        assert_eq!(Range::Min15.label(), "15 min");
        assert_eq!(Range::Hour1.phrase(), "in the last hour");
    }

    #[test]
    fn compact_numbers() {
        assert_eq!(compact(0), "0");
        assert_eq!(compact(999), "999");
        assert_eq!(compact(1000), "1.00K");
        assert_eq!(compact(12_345), "12.3K");
        assert_eq!(compact(999_499), "999K");
        assert_eq!(compact(999_999), "1.00M");
        assert_eq!(compact(2_950_120), "2.95M");
        assert_eq!(compact(9_995_000), "10.0M");
        assert_eq!(compact(1_200_000_000), "1.20B");
        assert_eq!(compact(3_400_000_000_000), "3.40T");
    }

    #[test]
    fn ticks_are_clean_and_few() {
        assert_eq!(nice_ticks(0.0, 8.0, 3), vec![0.0, 5.0, 10.0]);
        assert_eq!(nice_ticks(0.0, 12.0, 3), vec![0.0, 5.0, 10.0, 15.0]);
        assert_eq!(
            nice_ticks(2_950_000.0, 2_953_000.0, 3),
            vec![2_950_000.0, 2_951_000.0, 2_952_000.0, 2_953_000.0]
        );
        assert_eq!(
            nice_ticks(2_950_000.0, 3_120_000.0, 3),
            vec![2_900_000.0, 3_000_000.0, 3_100_000.0, 3_200_000.0]
        );
        // A flat series still gets a band around it.
        let flat = nice_ticks(8.0, 8.0, 3);
        assert!(flat.len() >= 2 && flat[0] <= 8.0 && flat[flat.len() - 1] >= 8.0);
        // Reversed bounds and non-finite values do not panic.
        assert_eq!(nice_ticks(10.0, 0.0, 3), vec![0.0, 5.0, 10.0]);
        assert_eq!(nice_ticks(f64::NAN, 1.0, 3), vec![0.0, 1.0]);
        for (lo, hi) in [(0.0, 1.0), (3.0, 97.0), (1234.0, 1236.0), (0.0, 1e9)] {
            let ticks = nice_ticks(lo, hi, 3);
            assert!((2..=4).contains(&ticks.len()), "{lo}..{hi}: {ticks:?}");
            assert!(ticks[0] <= lo && ticks[ticks.len() - 1] >= hi);
        }
    }

    #[test]
    fn tick_labels_tell_neighbours_apart() {
        assert_eq!(tick_labels(&[0.0, 5.0, 10.0]), vec!["0", "5", "10"]);
        assert_eq!(
            tick_labels(&[2_900_000.0, 3_000_000.0, 3_100_000.0]),
            vec!["2.9M", "3.0M", "3.1M"]
        );
        assert_eq!(
            tick_labels(&[2_950_000.0, 2_955_000.0, 2_960_000.0]),
            vec!["2.950M", "2.955M", "2.960M"]
        );
        assert_eq!(
            tick_labels(&[3_119_990.0, 3_119_995.0, 3_120_000.0]),
            vec!["3,119,990", "3,119,995", "3,120,000"]
        );
        assert_eq!(
            tick_labels(&[0.0, 20_000.0, 40_000.0]),
            vec!["0K", "20K", "40K"]
        );
    }

    #[test]
    fn scales_and_paths() {
        let s = scale((0.0, 10.0), (100.0, 0.0));
        assert_eq!(s.apply(0.0), 100.0);
        assert_eq!(s.apply(5.0), 50.0);
        assert_eq!(s.apply(10.0), 0.0);
        assert_eq!(scale((3.0, 3.0), (0.0, 10.0)).apply(3.0), 5.0);
        assert_eq!(line_path(&[]), "");
        assert_eq!(
            line_path(&[(64.0, 190.0), (80.54, 120.25), (97.0, 14.0)]),
            "M64.0,190.0 L80.5,120.2 L97.0,14.0"
        );
        assert_eq!(
            area_path(&[(64.0, 190.0), (97.0, 14.0)], 190.0),
            "M64.0,190.0 L97.0,14.0 L97.0,190.0 L64.0,190.0 Z"
        );
        assert_eq!(area_path(&[], 190.0), "");
    }

    #[test]
    fn nearest_and_downsample() {
        let xs = [0.0, 10.0, 20.0, 30.0];
        assert_eq!(nearest_index(&xs, -5.0), 0);
        assert_eq!(nearest_index(&xs, 4.9), 0);
        assert_eq!(nearest_index(&xs, 5.0), 0, "ties go to the earlier sample");
        assert_eq!(nearest_index(&xs, 5.1), 1);
        assert_eq!(nearest_index(&xs, 26.0), 3);
        assert_eq!(nearest_index(&xs, 99.0), 3);
        assert_eq!(nearest_index(&[], 1.0), 0);

        assert_eq!(downsample_indices(3, 60), vec![0, 1, 2]);
        assert_eq!(downsample_indices(5, 3), vec![0, 2, 4]);
        assert_eq!(downsample_indices(5, 1), vec![4]);
        assert_eq!(downsample_indices(5, 0), Vec::<usize>::new());
        let picked = downsample_indices(2880, MAX_POINTS);
        assert_eq!(picked.len(), MAX_POINTS);
        assert_eq!((picked[0], picked[MAX_POINTS - 1]), (0, 2879));
        assert!(picked.windows(2).all(|w| w[0] < w[1]));
        let values: Vec<u64> = (0..100).collect();
        assert_eq!(downsample(&values, 5), vec![0, 24, 49, 74, 99]);
    }

    #[test]
    fn clock_text() {
        let time = at(1_000_000_000); // 2001-09-09 01:46:40 UTC
        assert_eq!(hhmm(time, 0), "01:46");
        assert_eq!(hhmmss(time, 0), "01:46:40");
        assert_eq!(hhmm(time, 300), "20:46", "UTC-5 is the evening before");
        assert_eq!(hhmm(time, -330), "07:16", "UTC+5:30");
    }

    #[test]
    fn time_ticks_pick_a_clean_step() {
        let start = at(1_000_000_000 + 25); // 01:47:05
        let (ticks, with_seconds) = time_ticks(start, start + Duration::from_secs(14 * 60), 0);
        assert!(!with_seconds);
        assert_eq!(
            ticks.iter().map(|t| hhmm(*t, 0)).collect::<Vec<_>>(),
            vec!["01:50", "01:55", "02:00"]
        );
        // One hour of data: 15-minute ticks.
        let (ticks, _) = time_ticks(start, start + Duration::from_secs(3600), 0);
        assert_eq!(
            ticks.iter().map(|t| hhmm(*t, 0)).collect::<Vec<_>>(),
            vec!["02:00", "02:15", "02:30", "02:45"]
        );
        // The first seconds: 15 s ticks with seconds in the labels.
        let (ticks, with_seconds) = time_ticks(start, start + Duration::from_secs(40), 0);
        assert!(with_seconds);
        assert_eq!(
            ticks.iter().map(|t| hhmmss(*t, 0)).collect::<Vec<_>>(),
            vec!["01:47:15", "01:47:30", "01:47:45"]
        );
        // Shorter than the smallest step with no clean time inside: no ticks.
        let (ticks, _) = time_ticks(start, start + Duration::from_secs(3), 0);
        assert!(ticks.is_empty());
        // Hour ticks align to the local hour in a half-hour zone.
        let (ticks, _) = time_ticks(start, start + Duration::from_secs(4 * 3600), -330);
        assert_eq!(hhmm(ticks[0], -330), "08:00");
        assert!(ticks.len() <= 4);
    }

    #[test]
    fn durations_and_numbers() {
        assert_eq!(eta_text(Duration::from_secs(30)), "under a minute left");
        assert_eq!(eta_text(Duration::from_secs(12 * 60)), "about 12 min left");
        assert_eq!(eta_text(Duration::from_secs(2 * 3600)), "about 2 h left");
        assert_eq!(uptime_text(Duration::from_secs(45)), "45 s");
        assert_eq!(uptime_text(Duration::from_secs(12 * 60)), "12 min");
        assert_eq!(
            uptime_text(Duration::from_secs(3 * 3600 + 5 * 60)),
            "3 h 5 min"
        );
        assert_eq!(
            uptime_text(Duration::from_secs(2 * 86_400 + 4 * 3600)),
            "2 d 4 h"
        );
        assert_eq!(signed(1240), "+1,240");
        assert_eq!(signed(-3), "-3");
        assert_eq!(signed(0), "0");
        assert_eq!(percent_1(0.946), "94.6 %");
        assert_eq!(percent_1(0.99995), "99.9 %");
        assert_eq!(percent_1(0.0), "0.0 %");
        assert_eq!(percent_1(f64::NAN), "0.0 %");
        assert_eq!(percent_1(1.5), "100.0 %");
    }

    #[test]
    fn hero_summary() {
        let waiting = sync_summary(None, None, None, None);
        assert_eq!(waiting.figure, "—");
        assert_eq!(waiting.line, "Waiting for zebrad's RPC…");
        assert!(waiting.waiting && !waiting.at_tip);

        let syncing = sample(0, 2_950_120, 8);
        let estimating = sync_summary(Some(&syncing), None, None, None);
        assert_eq!(estimating.figure, "94.5 %");
        assert_eq!(
            estimating.line,
            "2,950,120 of 3,120,000 blocks · estimating…"
        );
        assert!((estimating.fraction - 0.9455).abs() < 1e-3);

        let known = sync_summary(
            Some(&syncing),
            Some(1240.4),
            Some(Duration::from_secs(2 * 3600 + 100)),
            Some("zebrad is stopped"),
        );
        assert_eq!(
            known.line,
            "2,950,120 of 3,120,000 blocks · 1,240 blocks/min · about 2 h left · zebrad is stopped"
        );
        let stuck = sync_summary(Some(&syncing), Some(0.0), None, None);
        assert_eq!(
            stuck.line,
            "2,950,120 of 3,120,000 blocks · 0 blocks/min · not catching up"
        );

        let tip = sync_summary(
            Some(&sample(0, 3_120_000, 8)),
            Some(5.0),
            Some(Duration::ZERO),
            None,
        );
        assert_eq!(tip.figure, "100 %");
        assert_eq!(tip.line, "At chain tip · height 3,120,000");
        assert!(tip.at_tip && tip.fraction == 1.0);

        assert_eq!(phase_note(&Phase::Running), None);
        assert_eq!(phase_note(&Phase::Stopped), Some("zebrad is stopped"));
    }

    #[test]
    fn sparkline_paths() {
        assert_eq!(spark_paths(&[1.0]), None);
        let (line, last, end) = spark_paths(&[1.0, 3.0, 2.0]).unwrap();
        assert_eq!(line, "M3.0,29.0 L60.0,3.0 L117.0,16.0");
        assert_eq!(last, "M60.0,3.0 L117.0,16.0");
        assert_eq!(end, (117.0, 16.0));
        // A flat line sits in the middle.
        let (line, _, _) = spark_paths(&[5.0, 5.0]).unwrap();
        assert_eq!(line, "M3.0,16.0 L117.0,16.0");
    }

    #[test]
    fn chart_geometry() {
        let samples: Vec<Sample> = (0..4)
            .map(|i| sample(i * 5, 2_950_000 + i * 100, 8))
            .collect();
        let fields = [
            ("Height", "chart-series-1", Field::Height),
            (
                "Estimated height",
                "chart-series-muted",
                Field::EstimatedHeight,
            ),
        ];
        let data = ChartData::from_samples(&samples, &fields);
        assert_eq!(data.len(), 4);
        assert_eq!(data.series[1].values, vec![3_120_000.0; 4]);

        let g = geometry(&data, false, 0).unwrap();
        assert_eq!(g.xs.len(), 4);
        assert_eq!(g.xs[0], PAD_L);
        assert_eq!(g.xs[3], VIEW_W - PAD_R);
        assert!(g.xs.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(g.lines.len(), 2);
        assert!(g.lines[0].starts_with("M64.0,"));
        assert!(g.area.ends_with(" Z"));
        assert!((2..=5).contains(&g.y_ticks.len()), "{:?}", g.y_ticks);
        // The estimated height is the top of the domain, so it sits at or above the plot top.
        assert!(g.ys[1][0] >= PAD_T && g.ys[1][0] < g.ys[0][0]);
        assert_eq!(g.end_label, "2.95M");
        // 15 s of data from a clean time: 15 s ticks at both ends, with seconds.
        let labels: Vec<_> = g.x_ticks.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(labels, vec!["00:00:00", "00:00:15"]);
        assert_eq!(g.x_ticks[0].0, PAD_L);

        // From zero for a count.
        let peers = ChartData::from_samples(&samples, &[("Peers", "chart-series-1", Field::Peers)]);
        let g = geometry(&peers, true, 0).unwrap();
        assert_eq!(g.y_ticks[0].1, "0");
        assert_eq!(g.y_ticks[0].0, VIEW_H - PAD_B);

        // Too little data.
        assert_eq!(
            geometry(&ChartData::from_samples(&samples[..1], &fields), false, 0),
            None
        );
        assert_eq!(geometry(&ChartData::default(), false, 0), None);

        assert_eq!(
            summary("Block height", &data, 0),
            "Block height: 4 samples from 00:00:00 to 00:00:15. Latest: Height 2,950,300, Estimated height 3,120,000."
        );
        assert_eq!(
            summary("Peers", &ChartData::default(), 0),
            "Peers: no samples yet"
        );
    }
}
