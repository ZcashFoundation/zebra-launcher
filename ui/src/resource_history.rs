//! A short history of the samples of a node's process tree, for the Resources card: its
//! one-minute rates, its sparklines and the last run of a stopped node. Pure Rust, tested
//! natively.

use std::{
    collections::VecDeque,
    time::{Duration, SystemTime},
};

use launcher_types::NodeResources;

use crate::charts::{MAX_POINTS, downsample};

/// How many samples are kept: four hours of the sampler's 5-second readings.
pub const MAX_SAMPLES: usize = 4 * 60 * 60 / 5;

/// How many of the newest samples are kept whole, with their process lists: one minute.
pub const WHOLE_SAMPLES: usize = 12;

/// The window of the card's rates: the one-minute average Resource Monitor uses.
pub const RATE_WINDOW: Duration = Duration::from_secs(60);

/// A rate since the spawn, or since a process started, is averaged over at least this long,
/// like the backend's first CPU rate: a sample taken right at the spawn does not turn the
/// first reads from disk into many MB/s.
const MIN_RATE_SECS: f64 = 1.0;

/// One sample reduced to its totals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub at: SystemTime,
    pub run: u64,
    pub run_secs: f64,
    pub cpu_secs: f64,
    pub memory: u64,
    pub peak: u64,
    pub read: Option<u64>,
    pub written: Option<u64>,
    pub processes: u32,
}

impl Point {
    fn of(sample: &NodeResources) -> Self {
        Point {
            at: sample.at,
            run: sample.run,
            run_secs: sample.run_secs,
            cpu_secs: sample.cpu_secs,
            memory: sample.memory_bytes,
            peak: sample.peak_memory_bytes,
            read: sample.read_bytes,
            written: sample.written_bytes,
            processes: sample.processes,
        }
    }
}

/// What a sparkline plots, over the newest run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SparkKind {
    /// Cores over each interval.
    Cpu,
    /// Bytes at each sample.
    Memory,
    /// Bytes written per second over each interval.
    Written,
}

/// What one process of the newest sample used over the kept minute.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessRate {
    pub pid: u32,
    pub cores: Option<f64>,
    pub read_per_sec: Option<f64>,
    pub written_per_sec: Option<f64>,
}

/// How the newest run went: what the card of a stopped node shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunSummary {
    pub run: u64,
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
    /// From the spawn to the last sample.
    pub length: Duration,
    /// `None` when CPU could not be read in that run.
    pub avg_cores: Option<f64>,
    pub peak: u64,
    pub read: Option<u64>,
    pub written: Option<u64>,
}

/// The samples since the launcher opened, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResourceHistory {
    points: Vec<Point>,
    /// The newest [`WHOLE_SAMPLES`] samples as published.
    whole: VecDeque<NodeResources>,
}

/// The growth of a counter between two points, per second of the run; `None` when either
/// point lacks it.
fn growth(a: Option<u64>, b: Option<u64>) -> Option<f64> {
    Some(b?.saturating_sub(a?) as f64)
}

impl ResourceHistory {
    /// Records a published sample. A sample of the newest sample's run that is not newer
    /// than it is a repeat and is ignored; past [`MAX_SAMPLES`] the oldest go.
    pub fn push(&mut self, sample: &NodeResources) {
        if self
            .points
            .last()
            .is_some_and(|last| last.run == sample.run && sample.run_secs <= last.run_secs)
        {
            return;
        }
        self.points.push(Point::of(sample));
        if self.points.len() > MAX_SAMPLES {
            let excess = self.points.len() - MAX_SAMPLES;
            self.points.drain(..excess);
        }
        self.whole.push_back(sample.clone());
        while self.whole.len() > WHOLE_SAMPLES {
            self.whole.pop_front();
        }
    }

    /// The newest sample, whole.
    pub fn latest(&self) -> Option<&NodeResources> {
        self.whole.back()
    }

    /// The points from `now - span` on, and the one just before, which starts the first
    /// interval in the window. All of them for `None`.
    fn window(&self, span: Option<Duration>, now: SystemTime) -> &[Point] {
        let Some(cutoff) = span.and_then(|span| now.checked_sub(span)) else {
            return &self.points;
        };
        let start = self.points.partition_point(|p| p.at < cutoff);
        &self.points[start.saturating_sub(1)..]
    }

    /// Whether the newest sample was taken within `span` of `now`.
    fn fresh(&self, span: Duration, now: SystemTime) -> Option<&NodeResources> {
        let newest = self.latest()?;
        let age = now.duration_since(newest.at).unwrap_or(Duration::ZERO);
        (age <= span).then_some(newest)
    }

    /// The intervals of the window within the newest run, as `(earlier, later)` pairs: after a
    /// restart, the previous run's last minute is not this run's.
    fn intervals(
        &self,
        span: Duration,
        now: SystemTime,
    ) -> impl Iterator<Item = (&Point, &Point)> + '_ {
        let newest = self.points.last().map(|point| point.run);
        self.window(Some(span), now)
            .windows(2)
            .filter(move |pair| {
                Some(pair[1].run) == newest
                    && pair[0].run == pair[1].run
                    && pair[1].run_secs > pair[0].run_secs
            })
            .map(|pair| (&pair[0], &pair[1]))
    }

    /// The cores the tree used over the last `span`: the CPU time's growth over the run time
    /// between the newest run's samples. With a single sample of the newest run, that
    /// sample's own rate (since the spawn, for a run's first). `None` without a recent sample,
    /// or when CPU cannot be read.
    pub fn cpu_cores(&self, span: Duration, now: SystemTime) -> Option<f64> {
        let newest = self.fresh(span, now)?;
        newest.cpu_cores?;
        let (cpu, secs) = self
            .intervals(span, now)
            .fold((0.0, 0.0), |(cpu, secs), (a, b)| {
                (
                    cpu + (b.cpu_secs - a.cpu_secs).max(0.0),
                    secs + (b.run_secs - a.run_secs),
                )
            });
        if secs > 0.0 {
            Some(cpu / secs)
        } else {
            newest.cpu_cores
        }
    }

    /// Bytes read and written per second over the last `span`, like [`Self::cpu_cores`]; a
    /// run's only sample gives its average since the spawn, over at least a second.
    pub fn io_rates(&self, span: Duration, now: SystemTime) -> (Option<f64>, Option<f64>) {
        let Some(newest) = self.fresh(span, now) else {
            return (None, None);
        };
        let rate = |field: fn(&Point) -> Option<u64>| {
            let (bytes, secs) = self
                .intervals(span, now)
                .filter_map(|(a, b)| Some((growth(field(a), field(b))?, b.run_secs - a.run_secs)))
                .fold((0.0, 0.0), |(bytes, secs), (b, s)| (bytes + b, secs + s));
            if secs > 0.0 {
                Some(bytes / secs)
            } else {
                let total = field(&Point::of(newest))? as f64;
                Some(total / newest.run_secs.max(MIN_RATE_SECS))
            }
        };
        (rate(|p| p.read), rate(|p| p.written))
    }

    /// What each process of the newest sample used over the kept minute: its growth since
    /// the oldest whole sample of the run that lists it, or, for a process seen once, its
    /// totals since it started, over at least a second. In the newest sample's order.
    pub fn process_rates(&self) -> Vec<ProcessRate> {
        let Some(newest) = self.latest() else {
            return Vec::new();
        };
        newest
            .members
            .iter()
            .map(|member| {
                let earlier = self
                    .whole
                    .iter()
                    .filter(|sample| sample.run == newest.run && sample.run_secs < newest.run_secs)
                    .find_map(|sample| {
                        let seen = sample
                            .members
                            .iter()
                            .find(|m| m.pid == member.pid && m.started_at == member.started_at)?;
                        Some((seen, newest.run_secs - sample.run_secs))
                    });
                let (base, secs) = match earlier {
                    Some((seen, secs)) => (Some(seen), secs),
                    None => (
                        None,
                        newest
                            .at
                            .duration_since(member.started_at)
                            .map_or(0.0, |age| age.as_secs_f64())
                            .max(MIN_RATE_SECS),
                    ),
                };
                let per_sec = |now: Option<u64>, then: Option<u64>| {
                    let grown = match base {
                        Some(_) => growth(then, now)?,
                        None => now? as f64,
                    };
                    (secs > 0.0).then(|| grown / secs)
                };
                let cpu = match base {
                    Some(seen) => (member.cpu_secs - seen.cpu_secs).max(0.0),
                    None => member.cpu_secs,
                };
                ProcessRate {
                    pid: member.pid,
                    cores: (newest.cpu_cores.is_some() && secs > 0.0).then(|| cpu / secs),
                    read_per_sec: per_sec(member.read_bytes, base.and_then(|b| b.read_bytes)),
                    written_per_sec: per_sec(
                        member.written_bytes,
                        base.and_then(|b| b.written_bytes),
                    ),
                }
            })
            .collect()
    }

    /// A sparkline's values over the newest run's samples in the range (all of them for
    /// `None`), at most [`MAX_POINTS`]. Each rate is over the interval that ends at its
    /// sample: since the run's previous sample, or since the spawn for its first, over at
    /// least a second. Empty while the newest sample cannot read what it plots.
    pub fn spark(&self, kind: SparkKind, span: Option<Duration>, now: SystemTime) -> Vec<f64> {
        let Some(run) = self.points.last().map(|p| p.run) else {
            return Vec::new();
        };
        // Nothing to plot while the OS refuses the counter: a flat line would read as zero.
        let unreadable = match kind {
            SparkKind::Cpu => self.latest().is_some_and(|s| s.cpu_cores.is_none()),
            SparkKind::Memory => false,
            SparkKind::Written => self.points.last().is_some_and(|p| p.written.is_none()),
        };
        if unreadable {
            return Vec::new();
        }
        let window = self.window(span, now);
        let cutoff = span.and_then(|span| now.checked_sub(span));
        let values: Vec<f64> = window
            .iter()
            .enumerate()
            .filter(|(_, point)| point.run == run && cutoff.is_none_or(|cutoff| point.at >= cutoff))
            .map(|(index, point)| {
                let previous = index
                    .checked_sub(1)
                    .and_then(|i| window.get(i))
                    .filter(|previous| previous.run == run && previous.run_secs < point.run_secs);
                let (secs, base_cpu, base_written) = match previous {
                    Some(p) => (point.run_secs - p.run_secs, p.cpu_secs, p.written),
                    None => (point.run_secs.max(MIN_RATE_SECS), 0.0, Some(0)),
                };
                match kind {
                    SparkKind::Memory => point.memory as f64,
                    _ if secs <= 0.0 => 0.0,
                    SparkKind::Cpu => (point.cpu_secs - base_cpu).max(0.0) / secs,
                    SparkKind::Written => growth(base_written, point.written).unwrap_or(0.0) / secs,
                }
            })
            .collect();
        downsample(&values, MAX_POINTS)
    }

    /// The newest run as its last sample left it.
    pub fn last_run(&self) -> Option<RunSummary> {
        let point = self.points.last()?;
        let cpu_known = self
            .latest()
            .is_some_and(|sample| sample.cpu_cores.is_some());
        let length = Duration::try_from_secs_f64(point.run_secs.max(0.0)).unwrap_or_default();
        Some(RunSummary {
            run: point.run,
            started_at: point.at.checked_sub(length).unwrap_or(point.at),
            ended_at: point.at,
            length,
            avg_cores: (cpu_known && point.run_secs > 0.0).then(|| point.cpu_secs / point.run_secs),
            peak: point.peak,
            read: point.read,
            written: point.written,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use launcher_types::{Platform, ProcessUsage};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    /// A one-process sample of `run`, `run_secs` into it, spawned at 1000 s.
    fn sample(run: u64, run_secs: u64, cpu: f64, written: u64) -> NodeResources {
        NodeResources {
            run,
            at: at(1000 + run_secs),
            run_secs: run_secs as f64,
            platform: Platform::Linux,
            cpu_secs: cpu,
            cpu_cores: Some(1.0),
            memory_bytes: 1000 + run_secs,
            peak_memory_bytes: 2000,
            committed_bytes: None,
            read_bytes: Some(written / 2),
            written_bytes: Some(written),
            processes: 1,
            threads: Some(68),
            descriptors: Some(412),
            members: vec![ProcessUsage {
                pid: 42,
                parent: None,
                name: "zebrad".into(),
                started_at: at(1000),
                cpu_secs: cpu,
                memory_bytes: Some(1000 + run_secs),
                read_bytes: Some(written / 2),
                written_bytes: Some(written),
                threads: Some(68),
            }],
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn repeats_are_ignored_and_the_history_is_bounded() {
        let mut history = ResourceHistory::default();
        assert_eq!(history.latest(), None);
        history.push(&sample(1, 5, 1.0, 0));
        history.push(&sample(1, 5, 1.0, 0));
        assert_eq!(history.points.len(), 1, "the same sample twice");
        // A new run starts its own counters.
        history.push(&sample(2, 0, 0.0, 0));
        assert_eq!(history.points.len(), 2);
        for secs in 1..(MAX_SAMPLES as u64 + 10) {
            history.push(&sample(2, secs * 5, secs as f64, secs));
        }
        assert_eq!(history.points.len(), MAX_SAMPLES);
        assert_eq!(history.whole.len(), WHOLE_SAMPLES);
        assert_eq!(
            history.latest().map(|s| s.run_secs),
            Some(((MAX_SAMPLES as u64 + 9) * 5) as f64)
        );
    }

    #[test]
    fn rates_are_over_the_last_minute_within_runs() {
        let mut history = ResourceHistory::default();
        // Run 1 burns 4 cores; run 2 starts 100 s later and burns 2.
        for secs in (0..=60).step_by(5) {
            history.push(&sample(1, secs, 4.0 * secs as f64, 1_000_000 * secs));
        }
        let now = at(1060);
        assert!(close(history.cpu_cores(RATE_WINDOW, now).unwrap(), 4.0));
        let (read, written) = history.io_rates(RATE_WINDOW, now);
        assert!(close(written.unwrap(), 1_000_000.0));
        assert!(close(read.unwrap(), 500_000.0));
        // A stale history gives no rate.
        assert_eq!(history.cpu_cores(RATE_WINDOW, at(2000)), None);

        // The first sample of run 2 has nothing to pair with in its run: its own rate.
        let mut first = sample(2, 100, 200.0, 0);
        first.at = at(1170);
        first.cpu_cores = Some(2.0);
        history.push(&first);
        assert!(close(
            history.cpu_cores(RATE_WINDOW, at(1170)).unwrap(),
            2.0
        ));
        // Its I/O: the totals since the spawn.
        let (_, written) = history.io_rates(RATE_WINDOW, at(1170));
        assert!(close(written.unwrap(), 0.0));
        // The pair across the two runs never counts.
        let mut second = sample(2, 105, 210.0, 50);
        second.at = at(1175);
        history.push(&second);
        assert!(close(
            history.cpu_cores(RATE_WINDOW, at(1175)).unwrap(),
            2.0
        ));
        assert!(close(
            history.io_rates(RATE_WINDOW, at(1175)).1.unwrap(),
            10.0
        ));
    }

    #[test]
    fn a_restart_starts_the_rates_afresh() {
        // Run 1 burns 4 cores for 15 s, and run 2 starts within the same minute.
        let mut history = ResourceHistory::default();
        for secs in (0..=15).step_by(5) {
            history.push(&sample(1, secs, 4.0 * secs as f64, 1_000_000 * secs));
        }
        let mut first = sample(2, 4, 3.0, 400);
        first.at = at(1020);
        first.cpu_cores = Some(0.75);
        history.push(&first);
        let now = at(1020);
        assert!(close(history.cpu_cores(RATE_WINDOW, now).unwrap(), 0.75));
        let (read, written) = history.io_rates(RATE_WINDOW, now);
        assert!(close(read.unwrap(), 50.0), "{read:?}");
        assert!(close(written.unwrap(), 100.0), "{written:?}");
    }

    #[test]
    fn rates_right_at_the_spawn_are_over_a_second() {
        // The first sample, 2 ms after the spawn: the exec already read 147,456 bytes from a
        // cold disk and used 10 ms of CPU, and zebrad started 3 ms before the sample.
        let mut first = sample(1, 0, 0.01, 0);
        first.run_secs = 0.002;
        first.at = at(1000) + Duration::from_millis(3);
        first.read_bytes = Some(147_456);
        first.members[0].read_bytes = Some(147_456);
        let mut history = ResourceHistory::default();
        history.push(&first);
        let now = first.at;
        let (read, written) = history.io_rates(RATE_WINDOW, now);
        assert!(close(read.unwrap(), 147_456.0), "{read:?}");
        assert!(close(written.unwrap(), 0.0));
        let rates = history.process_rates();
        assert!(close(rates[0].cores.unwrap(), 0.01), "{rates:?}");
        assert!(
            close(rates[0].read_per_sec.unwrap(), 147_456.0),
            "{rates:?}"
        );
        assert_eq!(history.spark(SparkKind::Cpu, None, now), vec![0.01]);
    }

    #[test]
    fn unknown_cpu_and_io_give_no_rate() {
        let mut history = ResourceHistory::default();
        let mut blind = sample(1, 5, 0.0, 0);
        blind.cpu_cores = None;
        blind.read_bytes = None;
        blind.written_bytes = None;
        history.push(&blind);
        assert_eq!(history.cpu_cores(RATE_WINDOW, at(1005)), None);
        assert_eq!(history.io_rates(RATE_WINDOW, at(1005)), (None, None));
        assert!(history.spark(SparkKind::Written, None, at(1005)).is_empty());
        assert!(history.spark(SparkKind::Cpu, None, at(1005)).is_empty());
        assert_eq!(history.last_run().unwrap().avg_cores, None);
    }

    #[test]
    fn process_rates_use_the_kept_minute() {
        let mut history = ResourceHistory::default();
        for secs in (0..=30).step_by(5) {
            history.push(&sample(1, secs, 2.0 * secs as f64, 100 * secs));
        }
        // A child appears in the newest sample only, 5 s after it started.
        let mut newest = sample(1, 35, 70.0, 3500);
        newest.members.push(ProcessUsage {
            pid: 43,
            parent: Some(42),
            name: "sh".into(),
            started_at: at(1030),
            cpu_secs: 1.0,
            memory_bytes: Some(10),
            read_bytes: None,
            written_bytes: Some(500),
            threads: Some(1),
        });
        history.push(&newest);
        let rates = history.process_rates();
        assert_eq!(rates.len(), 2);
        assert_eq!(rates[0].pid, 42);
        assert!(close(rates[0].cores.unwrap(), 2.0), "{rates:?}");
        assert!(close(rates[0].written_per_sec.unwrap(), 100.0));
        assert!(close(rates[0].read_per_sec.unwrap(), 50.0));
        assert_eq!(rates[1].pid, 43);
        assert!(
            close(rates[1].cores.unwrap(), 0.2),
            "since it started: {rates:?}"
        );
        assert!(close(rates[1].written_per_sec.unwrap(), 100.0));
        assert_eq!(rates[1].read_per_sec, None);
    }

    #[test]
    fn sparks_cover_the_newest_run() {
        let mut history = ResourceHistory::default();
        history.push(&sample(1, 5, 50.0, 0));
        history.push(&sample(1, 10, 100.0, 0));
        for (secs, cpu) in [(5, 10.0), (10, 25.0), (15, 30.0)] {
            let mut s = sample(2, secs, cpu, 1000 * secs);
            s.at = at(2000 + secs);
            history.push(&s);
        }
        let now = at(2015);
        assert_eq!(
            history.spark(SparkKind::Cpu, None, now),
            vec![2.0, 3.0, 1.0],
            "the first since the spawn, then per interval"
        );
        assert_eq!(
            history.spark(SparkKind::Memory, None, now),
            vec![1005.0, 1010.0, 1015.0]
        );
        assert_eq!(
            history.spark(SparkKind::Written, None, now),
            vec![1000.0, 1000.0, 1000.0]
        );
        // A range that starts inside the run keeps the interval that ends in it.
        assert_eq!(
            history.spark(SparkKind::Cpu, Some(Duration::from_secs(7)), now),
            vec![3.0, 1.0]
        );
        assert!(
            ResourceHistory::default()
                .spark(SparkKind::Cpu, None, now)
                .is_empty()
        );
    }

    #[test]
    fn the_last_run_is_the_newest_runs_last_sample() {
        assert_eq!(ResourceHistory::default().last_run(), None);
        let mut history = ResourceHistory::default();
        history.push(&sample(1, 100, 50.0, 0));
        history.push(&sample(2, 3600, 7200.0, 4_000_000));
        let summary = history.last_run().unwrap();
        assert_eq!(summary.run, 2);
        assert_eq!(summary.length, Duration::from_secs(3600));
        assert_eq!(summary.ended_at, at(4600));
        assert_eq!(summary.started_at, at(1000));
        assert!(close(summary.avg_cores.unwrap(), 2.0));
        assert_eq!(summary.peak, 2000);
        assert_eq!(summary.written, Some(4_000_000));
        assert_eq!(summary.read, Some(2_000_000));
    }
}
