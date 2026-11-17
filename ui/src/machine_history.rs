//! A short history of the computer's samples (docs/RESOURCES.md): its CPU busy over the last
//! minute, for the Resources card's footer, and its figures beside a node's in the card's
//! charts. Pure Rust, tested natively.

use std::{
    collections::VecDeque,
    time::{Duration, SystemTime},
};

use launcher_types::MachineResources;

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
