//! A short history of the computer's samples (docs/RESOURCES.md): its CPU busy over the last
//! minute, for the Resources card's footer; its figures beside a node's in the card's charts;
//! and how long its memory and its volumes have been low, with the leave margins the verdict's
//! rules need ([`Episode`]), carried past the samples it lets go. Pure Rust, tested natively.

use std::{
    collections::{BTreeMap, VecDeque},
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

    /// Whether a run or an episode is going on. A fold without either goes on as a new one
    /// would.
    fn busy(&self) -> bool {
        self.on || self.run.is_some()
    }
}

/// A condition on the computer's memory that rule 20 follows. More than [`GAP`] between two
/// samples breaks it, as in the charts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MemoryCondition {
    /// The memory available under `enter` of the total for `sustain`; it ends above `leave`.
    AvailableBelow {
        enter: f64,
        sustain: Duration,
        leave: f64,
    },
    /// macOS's memory pressure at `level` or above for `sustain`; it ends at a lower level.
    PressureAtLeast {
        level: MemoryPressure,
        sustain: Duration,
    },
    /// Windows' commit charge above `enter` of its limit for `sustain`; it ends under `leave`.
    CommitAbove {
        enter: f64,
        sustain: Duration,
        leave: f64,
    },
}

impl MemoryCondition {
    /// Goes on with one of the computer's samples.
    fn step(self, fold: &mut EpisodeFold, sample: &MachineResources) {
        match self {
            MemoryCondition::AvailableBelow {
                enter,
                sustain,
                leave,
            } => fold.step(
                sample.at,
                available_share(sample),
                |share| share < enter,
                |share| share > leave,
                sustain,
                GAP,
            ),
            MemoryCondition::PressureAtLeast {
                level: at_least,
                sustain,
            } => {
                let threshold = level(at_least);
                fold.step(
                    sample.at,
                    sample.pressure.map(level),
                    |value| value >= threshold,
                    |value| value < threshold,
                    sustain,
                    GAP,
                );
            }
            MemoryCondition::CommitAbove {
                enter,
                sustain,
                leave,
            } => fold.step(
                sample.at,
                commit_share(sample),
                |share| share > enter,
                |share| share < leave,
                sustain,
                GAP,
            ),
        }
    }
}

/// A condition on the free space of the volume a node's data is on, which rule 19 follows:
/// under `bytes`, or under `share` of the volume where that is less, for `sustain`; it ends
/// `margin` above that. More than [`VOLUME_GAP`] between two readings breaks it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FreeBelow {
    pub bytes: u64,
    pub share: Option<f64>,
    pub sustain: Duration,
    pub margin: u64,
}

impl FreeBelow {
    /// The free space under which `volume` enters it.
    fn threshold(self, volume: &VolumeUsage) -> u64 {
        self.share.map_or(self.bytes, |share| {
            ((volume.total_bytes as f64 * share) as u64).min(self.bytes)
        })
    }

    /// Goes on with a reading of the volume.
    fn step(self, fold: &mut EpisodeFold, volume: &VolumeUsage) {
        let threshold = self.threshold(volume);
        let leave = threshold.saturating_add(self.margin);
        fold.step(
            volume.measured_at,
            Some(volume.free_bytes as f64),
            |free| free < threshold as f64,
            |free| free > leave as f64,
            self.sustain,
            VOLUME_GAP,
        );
    }
}

/// What the volume readings of a node that a history let go left its [`FreeBelow`]
/// conditions, while one of them is still in a run or an episode.
#[derive(Clone, Debug, PartialEq)]
struct LetGo {
    /// The volume of the newest of them, and when it was read.
    label: String,
    measured_at: Option<SystemTime>,
    /// One per condition the history follows, in its order.
    folds: Vec<EpisodeFold>,
}

impl LetGo {
    fn new(label: &str, conditions: usize) -> Self {
        LetGo {
            label: label.to_owned(),
            measured_at: None,
            folds: vec![EpisodeFold::default(); conditions],
        }
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

/// A pressure level as a number, for an episode's readings.
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

/// The computer's samples since the launcher opened, oldest first, and what the samples it
/// let go left the conditions it follows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MachineHistory {
    samples: VecDeque<MachineResources>,
    /// Each memory condition it follows, with its fold over the samples let go.
    memory: Vec<(MemoryCondition, EpisodeFold)>,
    /// The free space conditions it follows.
    free: Vec<FreeBelow>,
    /// Their folds over each node's volume readings let go.
    let_go: BTreeMap<NodeId, LetGo>,
}

impl MachineHistory {
    /// An empty history that carries the episodes of `memory` and `free` past the samples it
    /// lets go, so each ends only at a reading that leaves it, however long it holds. Any
    /// other condition is judged on the samples kept.
    pub fn following(memory: &[MemoryCondition], free: &[FreeBelow]) -> Self {
        MachineHistory {
            memory: memory
                .iter()
                .map(|&condition| (condition, EpisodeFold::default()))
                .collect(),
            free: free.to_vec(),
            ..MachineHistory::default()
        }
    }

    /// Records a published sample. One taken when the newest was replaces it (the sampler
    /// published the same reading again with more in it); an older one is ignored. Past
    /// [`MAX_SAMPLES`] the oldest go, into the folds of the conditions it follows.
    pub fn push(&mut self, sample: &MachineResources) {
        match self.samples.back_mut() {
            Some(newest) if newest.at == sample.at => *newest = sample.clone(),
            Some(newest) if newest.at > sample.at => {}
            _ => {
                self.samples.push_back(sample.clone());
                while self.samples.len() > MAX_SAMPLES {
                    if let Some(gone) = self.samples.pop_front() {
                        self.let_go(&gone);
                    }
                }
            }
        }
    }

    /// Folds a sample it lets go into the conditions it follows.
    fn let_go(&mut self, sample: &MachineResources) {
        for (condition, fold) in &mut self.memory {
            condition.step(fold, sample);
        }
        if self.free.is_empty() {
            return;
        }
        let free = &self.free;
        for volume in &sample.volumes {
            for node in &volume.nodes {
                let gone = self
                    .let_go
                    .entry(node.clone())
                    .or_insert_with(|| LetGo::new(&volume.label, free.len()));
                // The samples carry a reading for 30 s.
                if gone.measured_at.is_some_and(|at| volume.measured_at <= at) {
                    continue;
                }
                // Its data folder moved to another volume ([`Self::current_volume_readings`]).
                if gone.label != volume.label {
                    *gone = LetGo::new(&volume.label, free.len());
                }
                for (condition, fold) in free.iter().zip(&mut gone.folds) {
                    condition.step(fold, volume);
                }
                gone.measured_at = Some(volume.measured_at);
            }
        }
        // Without a run or an episode, the readings kept say all there is.
        self.let_go
            .retain(|_, gone| gone.folds.iter().any(EpisodeFold::busy));
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

    /// The episode of `condition` at the newest sample, over the samples it let go too when
    /// it follows the condition.
    pub fn episode(&self, condition: MemoryCondition) -> Option<Episode> {
        let mut fold = self
            .memory
            .iter()
            .find(|(followed, _)| *followed == condition)
            .map_or_else(EpisodeFold::default, |(_, fold)| *fold);
        for sample in &self.samples {
            condition.step(&mut fold, sample);
        }
        fold.episode()
    }

    /// The episode of `condition` on the volume `node`'s data is on now, over its readings
    /// ([`Self::current_volume_readings`]), and over those it let go of the same volume when it
    /// follows the condition.
    pub fn free_episode(&self, node: &NodeId, condition: FreeBelow) -> Option<Episode> {
        let readings = self.volume_readings(node);
        let newest = readings.last()?;
        let start = run_start(&readings);
        let gone = self
            .let_go
            .get(node)
            .filter(|gone| start == 0 && gone.label == newest.label);
        let index = self.free.iter().position(|followed| *followed == condition);
        let (mut fold, after) = match (gone, index) {
            (Some(gone), Some(index)) => (gone.folds[index], gone.measured_at),
            _ => (EpisodeFold::default(), None),
        };
        for volume in &readings[start..] {
            if after.is_none_or(|after| volume.measured_at > after) {
                condition.step(&mut fold, volume);
            }
        }
        fold.episode()
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
        let problem = |history: &MachineHistory| {
            history.episode(MemoryCondition::AvailableBelow {
                enter: 0.05,
                sustain: two_min,
                leave: 0.07,
            })
        };
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
        let pressure = |history: &MachineHistory, level| {
            history.episode(MemoryCondition::PressureAtLeast {
                level,
                sustain: Duration::from_secs(60),
            })
        };
        assert_eq!(
            pressure(&mac, MemoryPressure::Critical).map(|e| e.since),
            Some(at(5))
        );
        assert!(pressure(&mac, MemoryPressure::Warning).is_some());
        mac.push(&MachineResources {
            pressure: Some(MemoryPressure::Warning),
            ..sample(70, None)
        });
        assert_eq!(pressure(&mac, MemoryPressure::Critical), None);
        assert!(
            pressure(&mac, MemoryPressure::Warning).is_some(),
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
        let commit = |history: &MachineHistory, enter| {
            history.episode(MemoryCondition::CommitAbove {
                enter,
                sustain: two_min,
                leave: 0.93,
            })
        };
        assert!(commit(&windows, 0.95).is_some());
        assert_eq!(commit(&windows, 0.97), None);
        assert_eq!(commit(&history, 0.95), None, "not Windows");
    }

    #[test]
    fn followed_episodes_outlast_the_samples_kept() {
        let out = MemoryCondition::AvailableBelow {
            enter: 0.05,
            sustain: Duration::from_secs(120),
            leave: 0.07,
        };
        let mut followed = MachineHistory::following(&[out], &[]);
        let mut kept = MachineHistory::default();
        // 16 GiB: 0.6 GiB, under 5 %, for 2 min, then 1.0 GiB, 6.25 % and inside the margin,
        // for longer than the samples kept.
        let end = 120 + 5 * MAX_SAMPLES as u64 + 300;
        for secs in (0..=end).step_by(5) {
            let sample = memory(secs, if secs <= 120 { 0.6 } else { 1.0 });
            followed.push(&sample);
            kept.push(&sample);
        }
        assert!(kept.samples[0].at > at(120), "the low samples are gone");
        assert_eq!(
            followed.episode(out),
            Some(Episode {
                since: at(0),
                until: at(end)
            })
        );
        assert_eq!(kept.episode(out), None, "a condition it does not follow");
        // Above 7 % it ends.
        followed.push(&memory(end + 5, 1.2));
        assert_eq!(followed.episode(out), None);

        // The free space on a node's volume: 1,400 bytes on the first reading, then 2,500,
        // inside the margin, for longer than the samples kept.
        let full = FreeBelow {
            bytes: 2_000,
            share: None,
            sustain: Duration::ZERO,
            margin: 1_000,
        };
        let low = FreeBelow {
            bytes: 20_000,
            share: Some(0.05),
            sustain: Duration::from_secs(60),
            margin: 1_000,
        };
        let node = NodeId::new("zebrad-mainnet").unwrap();
        let read = |secs: u64, label: &str, free: u64| MachineResources {
            volumes: vec![VolumeUsage {
                label: label.into(),
                total_bytes: 1_000_000,
                free_bytes: free,
                nodes: vec![node.clone()],
                // Read every 30 s, and carried in the samples between.
                measured_at: at(secs / 30 * 30),
            }],
            ..sample(secs, None)
        };
        let mut history = MachineHistory::following(&[], &[full, low]);
        for secs in (0..=end).step_by(5) {
            history.push(&read(secs, "/", if secs < 30 { 1_400 } else { 2_500 }));
        }
        let since = |condition| history.free_episode(&node, condition).map(|e| e.since);
        assert_eq!(since(full), Some(at(0)));
        assert_eq!(since(low), Some(at(0)), "under 5 % of the volume");
        assert_eq!(
            since(FreeBelow {
                bytes: 2_001,
                ..full
            }),
            None,
            "not followed"
        );
        // Its data folder moved to another volume: what came before says nothing about it,
        // and once the old volume's readings are gone nothing of them is kept.
        let moved = end + 5;
        for secs in (moved..=moved + 5 * MAX_SAMPLES as u64).step_by(5) {
            history.push(&read(secs, "/b", 2_500));
        }
        assert_eq!(history.free_episode(&node, full), None);
        assert!(history.free_episode(&node, low).is_some());
        assert_eq!(history.let_go.len(), 1);
        for secs in (moved + 5 * MAX_SAMPLES as u64 + 5..).step_by(5).take(12) {
            history.push(&read(secs, "/b", 30_000));
        }
        assert_eq!(history.free_episode(&node, low), None);
        for secs in (moved + 5 * MAX_SAMPLES as u64 + 65..)
            .step_by(5)
            .take(MAX_SAMPLES)
        {
            history.push(&read(secs, "/b", 30_000));
        }
        assert!(history.let_go.is_empty(), "no episode to carry");
    }

    #[test]
    fn a_followed_episode_is_the_one_over_every_sample() {
        // Memory that wanders, with gaps and samples without the figure, for three times the
        // samples kept: carried across the samples let go, each episode is the one over every
        // sample since the start. From the 1,000th sample it is under 5 % for 3 min, and then
        // for 4,000 samples it never leaves the problem's margin, nor the pressure warning.
        let out = MemoryCondition::AvailableBelow {
            enter: 0.05,
            sustain: Duration::from_secs(120),
            leave: 0.07,
        };
        let pressure = MemoryCondition::PressureAtLeast {
            level: MemoryPressure::Warning,
            sustain: Duration::from_secs(60),
        };
        let mut history = MachineHistory::following(&[out, pressure], &[]);
        let mut every = Vec::new();
        let mut carried = false;
        let mut seed = 7u64;
        let mut secs = 0;
        for index in 0..3 * MAX_SAMPLES {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let draw = seed >> 33;
            let held = index < 500 || (1000..5000).contains(&index);
            let entering = (1000..1036).contains(&index);
            // Mostly 5 s apart; now and then a gap of 20 s.
            secs += if draw.is_multiple_of(401) && !entering {
                20
            } else {
                5
            };
            let gib = if entering {
                0.6
            } else {
                [0.6, 0.9, 1.0, 1.3][(draw / 7 % if held { 3 } else { 4 }) as usize]
            };
            let mut sample = memory(secs, gib);
            if draw.is_multiple_of(89) && !entering {
                sample.memory_available_bytes = None;
            }
            sample.pressure = Some(
                [
                    MemoryPressure::Warning,
                    MemoryPressure::Critical,
                    MemoryPressure::Normal,
                ][(draw / 11 % if held { 2 } else { 3 }) as usize],
            );
            history.push(&sample);
            every.push(sample);
            if every.len().is_multiple_of(50) {
                for condition in [out, pressure] {
                    let over = |samples: &mut dyn Iterator<Item = &MachineResources>| {
                        let mut fold = EpisodeFold::default();
                        for sample in samples {
                            condition.step(&mut fold, sample);
                        }
                        fold.episode()
                    };
                    let episode = history.episode(condition);
                    assert_eq!(episode, over(&mut every.iter()), "at {secs} s");
                    carried |= episode != over(&mut history.samples.iter());
                }
            }
        }
        assert!(carried, "an episode the samples kept would have ended");
    }

    #[test]
    fn a_followed_free_space_episode_is_the_one_over_every_reading() {
        // A volume read every 30 s whose free space wanders, now and then not read (its
        // folder missing), and twice moved to another volume, for three times the samples
        // kept: each episode is the one over every reading of the volume it is on now.
        let full = FreeBelow {
            bytes: 2_000,
            share: None,
            sustain: Duration::ZERO,
            margin: 1_000,
        };
        let low = FreeBelow {
            bytes: 20_000,
            share: Some(0.05),
            sustain: Duration::from_secs(60),
            margin: 1_000,
        };
        let node = NodeId::new("zebrad-mainnet").unwrap();
        let mut history = MachineHistory::following(&[], &[full, low]);
        let mut every = MachineHistory::default();
        let mut carried = false;
        let mut seed = 11u64;
        let mut reading: Option<VolumeUsage> = None;
        for index in 0..3 * MAX_SAMPLES {
            let secs = 5 * index as u64;
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let draw = seed >> 33;
            if secs.is_multiple_of(30) {
                // From the 1,200th sample on, inside both margins for 4,000 samples.
                let held = (1200..5200).contains(&index);
                let free = if (1200..1206).contains(&index) {
                    1_000
                } else if held {
                    [2_500, 2_900, 1_500][(draw / 7 % 3) as usize]
                } else {
                    [1_500, 2_500, 3_500, 19_000, 20_500, 21_500][(draw / 7 % 6) as usize]
                };
                let label = match index {
                    ..1000 => "/",
                    1000..6000 => "/b",
                    _ => "/",
                };
                reading = (!draw.is_multiple_of(53) || held).then(|| VolumeUsage {
                    label: label.into(),
                    total_bytes: 1_000_000,
                    free_bytes: free,
                    nodes: vec![node.clone()],
                    measured_at: at(secs),
                });
            }
            let sample = MachineResources {
                volumes: reading.iter().cloned().collect(),
                ..sample(secs, None)
            };
            history.push(&sample);
            every.samples.push_back(sample);
            if index.is_multiple_of(50) {
                for condition in [full, low] {
                    let over = |readings: Vec<&VolumeUsage>| {
                        let mut fold = EpisodeFold::default();
                        for volume in &readings[run_start(&readings)..] {
                            condition.step(&mut fold, volume);
                        }
                        fold.episode()
                    };
                    let episode = history.free_episode(&node, condition);
                    assert_eq!(episode, over(every.volume_readings(&node)), "at {secs} s");
                    carried |= episode != over(history.volume_readings(&node));
                }
            }
        }
        assert!(carried, "an episode the samples kept would have ended");
    }

    #[test]
    fn a_carried_free_space_episode_is_of_the_volume_the_data_is_on() {
        let full = FreeBelow {
            bytes: 2_000,
            share: None,
            sustain: Duration::ZERO,
            margin: 1_000,
        };
        let low = FreeBelow {
            bytes: 20_000,
            ..full
        };
        let node = NodeId::new("zebrad-mainnet").unwrap();
        let mut history = MachineHistory::following(&[], &[full, low]);
        // A reading every 5 s, the next at `secs`.
        let mut secs = 0;
        let mut push = |history: &mut MachineHistory, count: u64, label: &str, free: u64| {
            for _ in 0..count {
                history.push(&MachineResources {
                    volumes: vec![VolumeUsage {
                        label: label.into(),
                        total_bytes: 1_000_000,
                        free_bytes: free,
                        nodes: vec![node.clone()],
                        measured_at: at(secs),
                    }],
                    ..sample(secs, None)
                });
                secs += 5;
            }
        };
        let since = |history: &MachineHistory, condition| {
            history.free_episode(&node, condition).map(|e| e.since)
        };
        // Low on /a, then moved to /b at 500 s, 1,400 there once and then 2,500, inside the
        // problem's margin, for longer than the samples kept: both episodes are carried from
        // the move, though /a's warning was still going on when its readings were let go.
        push(&mut history, 100, "/a", 15_000);
        push(&mut history, 1, "/b", 1_400);
        push(&mut history, MAX_SAMPLES as u64 + 100, "/b", 2_500);
        assert!(history.samples[0].at > at(500), "the move is let go");
        assert_eq!(since(&history, full), Some(at(500)));
        assert_eq!(since(&history, low), Some(at(500)));
        // Moved to /c for 50 s and back to /b among the samples kept: the episodes carried on
        // /b before then end with the run they were in.
        push(&mut history, 10, "/c", 10_000);
        let back = history
            .latest()
            .map(|newest| newest.at + Duration::from_secs(5));
        push(&mut history, 10, "/b", 2_500);
        assert_eq!(since(&history, full), None);
        assert_eq!(since(&history, low), back);
        // And once those samples are let go too.
        push(&mut history, MAX_SAMPLES as u64, "/b", 2_500);
        assert_eq!(since(&history, full), None);
        assert_eq!(since(&history, low), back);
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
