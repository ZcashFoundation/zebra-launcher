//! A short history of the computer's samples (docs/RESOURCES.md): its CPU busy over the last
//! minute, for the Resources card's footer; its figures beside a node's in the card's charts;
//! and how long its memory and its volumes have been low, with the leave margins the verdict's
//! rules need ([`Episode`]). Pure Rust, tested natively.

use std::{
    collections::VecDeque,
    time::{Duration, SystemTime},
};

use launcher_types::{MachineResources, MemoryPressure, NodeId, VolumeUsage};

use crate::resource_history::GAP;

/// More than this between two readings of a volume is a gap in them, and a newest reading
/// older than this is stale: three of the sampler's 30 s intervals.
pub const VOLUME_GAP: Duration = Duration::from_secs(90);

/// A condition with a sustain time and a leave margin: it begins once the readings have stayed
/// past an entry threshold for the sustain time, and ends only at a reading past a leave
/// threshold on the other side, so a value that wanders around one threshold does not flap.
/// Readings further apart than a gap (the computer asleep, or not sampled) are not one run:
/// what came before the gap counts for nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Episode {
    /// The first reading of the run that began it.
    pub since: SystemTime,
    /// The newest reading.
    pub until: SystemTime,
}

impl Episode {
    /// The episode going on at the newest of `readings`, oldest first: a reading that `enters`
    /// starts a run, which begins the episode once it has lasted `sustain` (at once for zero);
    /// a reading that neither enters nor is in an episode ends the run; the episode ends at a
    /// reading that `leaves`. A reading without a value changes nothing. A reading more than
    /// `max_gap` after the one before starts the run afresh, or, in an episode, counts it from
    /// there.
    pub fn current(
        readings: impl IntoIterator<Item = (SystemTime, Option<f64>)>,
        enters: impl Fn(f64) -> bool,
        leaves: impl Fn(f64) -> bool,
        sustain: Duration,
        max_gap: Duration,
    ) -> Option<Episode> {
        let mut fold = EpisodeFold::default();
        for (at, value) in readings {
            fold.step(at, value, &enters, &leaves, sustain, max_gap);
        }
        fold.episode()
    }
}

/// What [`Episode::current`] knows after some readings, to go on from with newer ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EpisodeFold {
    /// The first reading of the run going on.
    run: Option<SystemTime>,
    /// The run has begun an episode, and no reading has left it since.
    on: bool,
    /// The newest reading with a value.
    previous: Option<SystemTime>,
    /// The newest reading.
    newest: Option<SystemTime>,
}

impl EpisodeFold {
    /// Goes on with the reading `value` at `at`, by the rules of [`Episode::current`].
    pub fn step(
        &mut self,
        at: SystemTime,
        value: Option<f64>,
        enters: impl Fn(f64) -> bool,
        leaves: impl Fn(f64) -> bool,
        sustain: Duration,
        max_gap: Duration,
    ) {
        self.newest = Some(at);
        let Some(value) = value else {
            return;
        };
        let gap = self
            .previous
            .is_some_and(|previous| at.duration_since(previous).unwrap_or_default() > max_gap);
        self.previous = Some(at);
        if self.on {
            if leaves(value) {
                self.on = false;
                self.run = None;
            } else if gap {
                // It held before the gap and after it; "for N" leaves the gap out.
                self.run = Some(at);
            }
        } else if enters(value) {
            if gap {
                self.run = None;
            }
            let since = *self.run.get_or_insert(at);
            self.on = at.duration_since(since).unwrap_or_default() >= sustain;
        } else {
            self.run = None;
        }
    }

    /// The episode going on after the readings so far.
    pub fn episode(&self) -> Option<Episode> {
        Some(Episode {
            since: self.run.filter(|_| self.on)?,
            until: self.newest?,
        })
    }
}

/// Where the newest run of `readings` with the newest one's label starts.
fn run_start(readings: &[&VolumeUsage]) -> usize {
    readings
        .last()
        .and_then(|newest| {
            readings
                .iter()
                .rposition(|volume| volume.label != newest.label)
        })
        .map_or(0, |before| before + 1)
}

/// A pressure level as a number, for [`Episode::current`].
fn level(pressure: MemoryPressure) -> f64 {
    match pressure {
        MemoryPressure::Normal => 0.0,
        MemoryPressure::Warning => 1.0,
        MemoryPressure::Critical => 2.0,
    }
}

/// Whether a volume reading still says how the volume is at `now`: it was read within
/// [`VOLUME_GAP`]. A folder whose read hangs keeps its last reading in the samples, with its
/// time, so an older one would show figures that no longer hold.
pub fn volume_fresh(volume: &VolumeUsage, now: SystemTime) -> bool {
    now.duration_since(volume.measured_at).unwrap_or_default() <= VOLUME_GAP
}

/// The share of a computer's memory available: `None` without the figure.
pub fn available_share(machine: &MachineResources) -> Option<f64> {
    let available = machine.memory_available_bytes?;
    (machine.memory_total_bytes > 0).then(|| available as f64 / machine.memory_total_bytes as f64)
}

/// The commit charge as a share of its limit (Windows): `None` elsewhere.
pub fn commit_share(machine: &MachineResources) -> Option<f64> {
    let (commit, limit) = (machine.commit_bytes?, machine.commit_limit_bytes?);
    (limit > 0).then(|| commit as f64 / limit as f64)
}

/// How many samples are kept: four hours of the sampler's 5-second readings, like the node's
/// [`crate::resource_history::MAX_SAMPLES`].
pub const MAX_SAMPLES: usize = 4 * 60 * 60 / 5;

/// The computer's samples since the launcher opened, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MachineHistory {
    samples: VecDeque<MachineResources>,
}

impl MachineHistory {
    /// Records a published sample. One taken when the newest was replaces it (the sampler
    /// published the same reading again with more in it); an older one is ignored. Past
    /// [`MAX_SAMPLES`] the oldest go.
    pub fn push(&mut self, sample: &MachineResources) {
        match self.samples.back_mut() {
            Some(newest) if newest.at == sample.at => *newest = sample.clone(),
            Some(newest) if newest.at > sample.at => {}
            _ => {
                self.samples.push_back(sample.clone());
                while self.samples.len() > MAX_SAMPLES {
                    self.samples.pop_front();
                }
            }
        }
    }

    /// The samples taken from `now - span` on.
    fn since(&self, span: Duration, now: SystemTime) -> impl Iterator<Item = &MachineResources> {
        let cutoff = now.checked_sub(span);
        let start = cutoff.map_or(0, |cutoff| {
            self.samples.partition_point(|sample| sample.at < cutoff)
        });
        self.samples.range(start..)
    }

    /// The newest sample taken at or before `at`, if it is no more than `within` older: the
    /// computer's figures beside a node's sample, whose sampler reads on its own clock.
    pub fn at(&self, at: SystemTime, within: Duration) -> Option<&MachineResources> {
        let after = self.samples.partition_point(|sample| sample.at <= at);
        let sample = self.samples.get(after.checked_sub(1)?)?;
        let age = at.duration_since(sample.at).unwrap_or_default();
        (age <= within).then_some(sample)
    }

    /// The newest sample.
    pub fn latest(&self) -> Option<&MachineResources> {
        self.samples.back()
    }

    /// The episode of the available memory under `enter` of the total for `sustain`, which
    /// ends above `leave`. More than [`GAP`] between two samples breaks it, as in the charts.
    pub fn available_below_for(
        &self,
        enter: f64,
        sustain: Duration,
        leave: f64,
    ) -> Option<Episode> {
        Episode::current(
            self.samples
                .iter()
                .map(|sample| (sample.at, available_share(sample))),
            |share| share < enter,
            |share| share > leave,
            sustain,
            GAP,
        )
    }

    /// The episode of macOS's memory pressure at `at_least` or above for `sustain`, which ends
    /// at a lower level.
    pub fn pressure_at_least_for(
        &self,
        at_least: MemoryPressure,
        sustain: Duration,
    ) -> Option<Episode> {
        let threshold = level(at_least);
        Episode::current(
            self.samples
                .iter()
                .map(|sample| (sample.at, sample.pressure.map(level))),
            |value| value >= threshold,
            |value| value < threshold,
            sustain,
            GAP,
        )
    }

    /// The episode of Windows' commit charge above `enter` of its limit for `sustain`, which
    /// ends under `leave`.
    pub fn commit_above_for(&self, enter: f64, sustain: Duration, leave: f64) -> Option<Episode> {
        Episode::current(
            self.samples
                .iter()
                .map(|sample| (sample.at, commit_share(sample))),
            |share| share > enter,
            |share| share < leave,
            sustain,
            GAP,
        )
    }

    /// The readings of the volume that holds `node`'s data folder, oldest first: one per
    /// reading, though the samples carry each for 30 s.
    pub fn volume_readings(&self, node: &NodeId) -> Vec<&VolumeUsage> {
        let mut readings: Vec<&VolumeUsage> = Vec::new();
        for volume in self
            .samples
            .iter()
            .filter_map(|sample| sample.volume_of(node))
        {
            if readings
                .last()
                .is_none_or(|last| last.measured_at < volume.measured_at)
            {
                readings.push(volume);
            }
        }
        readings
    }

    /// The readings of the volume that holds `node`'s data folder now, oldest first: the
    /// newest run of [`MachineHistory::volume_readings`] with the newest's label, so the
    /// readings of a volume its data folder was moved from are left out.
    pub fn current_volume_readings(&self, node: &NodeId) -> Vec<&VolumeUsage> {
        let mut readings = self.volume_readings(node);
        let start = run_start(&readings);
        readings.split_off(start)
    }

    /// Whether a sample taken from `from` to `to` passes `test`.
    pub fn any_between(
        &self,
        from: SystemTime,
        to: SystemTime,
        test: impl Fn(&MachineResources) -> bool,
    ) -> bool {
        let start = self.samples.partition_point(|sample| sample.at < from);
        self.samples
            .range(start..)
            .take_while(|sample| sample.at <= to)
            .any(test)
    }

    /// The logical CPUs the computer kept busy over the last `span`: the mean of its samples'
    /// figures there, each over its own interval. `None` without such a figure in the span.
    pub fn busy_cores(&self, span: Duration, now: SystemTime) -> Option<f64> {
        let (sum, count) = self
            .since(span, now)
            .filter_map(|sample| sample.cpu_busy_cores)
            .fold((0.0, 0u32), |(sum, count), busy| (sum + busy, count + 1));
        (count > 0).then(|| sum / f64::from(count))
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sample(secs: u64, busy: Option<f64>) -> MachineResources {
        MachineResources {
            at: at(secs),
            logical_cpus: 8,
            cpu_busy_cores: busy,
            memory_total_bytes: 16 << 30,
            memory_available_bytes: Some(4 << 30),
            swap_total_bytes: None,
            swap_used_bytes: None,
            commit_bytes: None,
            commit_limit_bytes: None,
            pressure: None,
            volumes: Vec::new(),
        }
    }

    #[test]
    fn the_history_is_bounded_and_ignores_older_samples() {
        let mut history = MachineHistory::default();
        for secs in 0..(MAX_SAMPLES as u64 + 10) {
            history.push(&sample(secs * 5, Some(1.0)));
        }
        assert_eq!(history.samples.len(), MAX_SAMPLES);
        assert_eq!(history.samples[0].at, at(50));
        // An older sample is a repeat; one with the newest's time replaces it.
        history.push(&sample(5, Some(7.0)));
        assert_eq!(history.samples.len(), MAX_SAMPLES);
        let newest = history.samples.back().unwrap().at;
        history.push(&MachineResources {
            cpu_busy_cores: Some(3.0),
            ..sample(0, None)
        });
        history.push(&MachineResources {
            at: newest,
            cpu_busy_cores: Some(3.0),
            ..sample(0, None)
        });
        assert_eq!(history.samples.len(), MAX_SAMPLES);
        assert_eq!(history.samples.back().unwrap().cpu_busy_cores, Some(3.0));
    }

    #[test]
    fn busy_cores_are_the_mean_over_the_span() {
        let mut history = MachineHistory::default();
        assert_eq!(history.busy_cores(Duration::from_secs(60), at(100)), None);
        // The first sample has no figure.
        history.push(&sample(0, None));
        for (secs, busy) in [(5, 6.0), (10, 2.0), (65, 4.0), (70, 5.0)] {
            history.push(&sample(secs, Some(busy)));
        }
        let minute = Duration::from_secs(60);
        assert_eq!(history.busy_cores(minute, at(70)), Some(11.0 / 3.0));
        assert_eq!(history.busy_cores(minute, at(120)), Some(4.5));
        assert_eq!(history.busy_cores(minute, at(500)), None, "nothing recent");
    }

    /// Readings every 5 s from 0 s on.
    fn readings(values: &[Option<f64>]) -> Vec<(SystemTime, Option<f64>)> {
        values
            .iter()
            .enumerate()
            .map(|(i, value)| (at(5 * i as u64), *value))
            .collect()
    }

    #[test]
    fn episodes_need_their_sustain_time_and_keep_their_leave_margin() {
        let gap = Duration::from_secs(15);
        let low = |values: &[f64], sustain: u64| {
            let values: Vec<Option<f64>> = values.iter().map(|v| Some(*v)).collect();
            Episode::current(
                readings(&values),
                |v| v < 5.0,
                |v| v > 7.0,
                Duration::from_secs(sustain),
                gap,
            )
        };
        // Under 5 for 10 s: an episode since the first low reading.
        assert_eq!(
            low(&[9.0, 4.0, 4.5, 3.0], 10),
            Some(Episode {
                since: at(5),
                until: at(15)
            })
        );
        // Not long enough yet.
        assert_eq!(low(&[9.0, 9.0, 4.0, 4.5], 10), None);
        // A reading of 6 inside the margin breaks a run that has not begun an episode...
        assert_eq!(low(&[4.0, 6.0, 4.0, 4.0], 10), None);
        // ...but not one that has.
        assert_eq!(
            low(&[4.0, 4.0, 4.0, 6.0, 6.5], 10),
            Some(Episode {
                since: at(0),
                until: at(20)
            })
        );
        // Past the leave threshold it ends, and needs its sustain time again.
        assert_eq!(low(&[4.0, 4.0, 4.0, 7.5, 4.0], 10), None);
        // No sustain: the first reading begins it.
        assert_eq!(
            low(&[9.0, 1.0], 0),
            Some(Episode {
                since: at(5),
                until: at(5)
            })
        );
        // A reading without a value changes nothing.
        let gaps = Episode::current(
            readings(&[Some(4.0), None, Some(4.0)]),
            |v| v < 5.0,
            |v| v > 7.0,
            Duration::from_secs(10),
            gap,
        );
        assert_eq!(gaps.map(|episode| episode.since), Some(at(0)));
        assert_eq!(
            Episode::current(readings(&[]), |_| true, |_| false, Duration::ZERO, gap),
            None
        );
        // Two low readings 8 h apart, the computer asleep between them, are not 8 h of it.
        let asleep = |values: &[(u64, f64)]| {
            Episode::current(
                values.iter().map(|&(secs, v)| (at(secs), Some(v))),
                |v| v < 5.0,
                |v| v > 7.0,
                Duration::from_secs(120),
                gap,
            )
        };
        assert_eq!(asleep(&[(0, 4.0), (28_800, 4.0)]), None);
        // After the gap the run starts afresh and needs its sustain time.
        let awake: Vec<(u64, f64)> = std::iter::once((0, 4.0))
            .chain((0..=24).map(|i| (28_800 + 5 * i, 4.0)))
            .collect();
        assert_eq!(
            asleep(&awake),
            Some(Episode {
                since: at(28_800),
                until: at(28_920)
            })
        );
        // An episode that held before the gap and holds after it goes on, from the gap.
        let mut held: Vec<(u64, f64)> = (0..=24).map(|i| (5 * i, 4.0)).collect();
        held.push((28_800, 6.0));
        assert_eq!(
            asleep(&held),
            Some(Episode {
                since: at(28_800),
                until: at(28_800)
            })
        );
    }

    fn memory(secs: u64, available_gib: f64) -> MachineResources {
        MachineResources {
            memory_available_bytes: Some((available_gib * (1u64 << 30) as f64) as u64),
            ..sample(secs, Some(1.0))
        }
    }

    #[test]
    fn memory_episodes() {
        let mut history = MachineHistory::default();
        // 16 GiB: 0.6 GiB is under 5 %, 1.0 GiB is 6.25 %, 1.2 GiB is 7.5 %.
        for secs in (0..=120).step_by(5) {
            history.push(&memory(secs, 0.6));
        }
        let two_min = Duration::from_secs(120);
        let problem = |history: &MachineHistory| history.available_below_for(0.05, two_min, 0.07);
        assert_eq!(problem(&history).map(|e| e.since), Some(at(0)));
        history.push(&memory(125, 1.0));
        assert!(problem(&history).is_some(), "inside the leave margin");
        history.push(&memory(130, 1.2));
        assert_eq!(problem(&history), None, "above 7 %");
        assert!(history.latest().is_some_and(|m| m.at == at(130)));

        let mut mac = MachineHistory::default();
        for (secs, pressure) in std::iter::once((0, MemoryPressure::Normal)).chain(
            (5..=65)
                .step_by(5)
                .map(|secs| (secs, MemoryPressure::Critical)),
        ) {
            mac.push(&MachineResources {
                pressure: Some(pressure),
                ..sample(secs, None)
            });
        }
        let minute = Duration::from_secs(60);
        assert_eq!(
            mac.pressure_at_least_for(MemoryPressure::Critical, minute)
                .map(|e| e.since),
            Some(at(5))
        );
        assert!(
            mac.pressure_at_least_for(MemoryPressure::Warning, minute)
                .is_some()
        );
        mac.push(&MachineResources {
            pressure: Some(MemoryPressure::Warning),
            ..sample(70, None)
        });
        assert_eq!(
            mac.pressure_at_least_for(MemoryPressure::Critical, minute),
            None
        );
        assert!(
            mac.pressure_at_least_for(MemoryPressure::Warning, minute)
                .is_some(),
            "still at warning or above"
        );

        let mut windows = MachineHistory::default();
        for secs in (0..=120).step_by(5) {
            windows.push(&MachineResources {
                commit_bytes: Some(96),
                commit_limit_bytes: Some(100),
                ..sample(secs, None)
            });
        }
        assert!(windows.commit_above_for(0.95, two_min, 0.93).is_some());
        assert_eq!(windows.commit_above_for(0.97, two_min, 0.93), None);
        assert_eq!(
            history.commit_above_for(0.95, two_min, 0.93),
            None,
            "not Windows"
        );
    }

    #[test]
    fn a_nodes_volume_readings_come_once_each() {
        let node = NodeId::new("zebrad-mainnet").unwrap();
        let other = NodeId::new("zebrad-testnet").unwrap();
        let volume = |measured: u64, free: u64| VolumeUsage {
            label: "/".into(),
            total_bytes: 1_000_000_000_000,
            free_bytes: free,
            nodes: vec![node.clone()],
            measured_at: at(measured),
        };
        let mut history = MachineHistory::default();
        for secs in (0..60).step_by(5) {
            let measured = secs / 30 * 30;
            history.push(&MachineResources {
                volumes: vec![volume(measured, 1000 - measured)],
                ..sample(secs, None)
            });
        }
        let readings = history.volume_readings(&node);
        let free: Vec<u64> = readings.iter().map(|volume| volume.free_bytes).collect();
        assert_eq!(free, vec![1000, 970]);
        // The newest, read at 30 s, says how the volume is for 90 s.
        assert!(volume_fresh(readings[1], at(120)));
        assert!(!volume_fresh(readings[1], at(121)));
        assert!(history.volume_readings(&other).is_empty());
        assert_eq!(history.current_volume_readings(&node), readings);
        assert!(history.current_volume_readings(&other).is_empty());
        assert!(history.any_between(at(10), at(20), |m| m.at == at(15)));

        // The data folder moved to another volume and back: only the newest run is the
        // volume it is on now.
        for (secs, label) in [(60, "/b"), (90, "/b"), (120, "/"), (150, "/")] {
            history.push(&MachineResources {
                volumes: vec![VolumeUsage {
                    label: label.into(),
                    ..volume(secs, secs)
                }],
                ..sample(secs, None)
            });
        }
        let labels = |readings: Vec<&VolumeUsage>| -> Vec<(String, u64)> {
            readings
                .iter()
                .map(|volume| (volume.label.clone(), volume.free_bytes))
                .collect()
        };
        assert_eq!(labels(history.volume_readings(&node)).len(), 6);
        assert_eq!(
            labels(history.current_volume_readings(&node)),
            vec![("/".to_owned(), 120), ("/".to_owned(), 150)]
        );
        assert!(!history.any_between(at(10), at(20), |m| m.at == at(25)));
    }

    #[test]
    fn a_node_sample_finds_the_computers_newest_before_it() {
        let mut history = MachineHistory::default();
        assert_eq!(history.at(at(10), Duration::from_secs(15)), None);
        for secs in [0, 5, 10, 40] {
            history.push(&sample(secs, Some(secs as f64)));
        }
        let within = Duration::from_secs(15);
        let busy = |secs| history.at(at(secs), within).and_then(|s| s.cpu_busy_cores);
        assert_eq!(busy(7), Some(5.0));
        assert_eq!(busy(10), Some(10.0));
        assert_eq!(busy(26), None, "the newest before it is too old");
        assert_eq!(busy(41), Some(40.0));
    }
}
