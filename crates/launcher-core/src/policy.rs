//! When to restart zebrad after an unexpected exit.

use std::time::{Duration, Instant};

use launcher_types::ExitClass;

/// Restart policy parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Delay before the first restart; doubles on each consecutive crash.
    pub initial_backoff: Duration,
    /// Cap on the doubling delay.
    pub max_backoff: Duration,
    /// Crashes within this window count towards `max_crashes`, and a run that
    /// lasts this long resets the backoff.
    pub window: Duration,
    /// Give up after this many crashes inside `window`.
    pub max_crashes: u32,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(60),
            window: Duration::from_secs(10 * 60),
            max_crashes: 5,
        }
    }
}

/// What to do after a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Restart after this delay.
    RestartAfter(Duration),
    /// Stop trying: either the cause is deterministic or crashes are too frequent.
    GiveUp,
}

/// Tracks recent crashes and decides whether to restart.
#[derive(Clone, Debug)]
pub struct RestartTracker {
    policy: RestartPolicy,
    crashes: Vec<Instant>,
}

impl RestartTracker {
    /// Creates a tracker with no crash history.
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            crashes: Vec::new(),
        }
    }

    /// Records an unexpected exit at `now` and decides what to do next.
    pub fn on_exit(&mut self, class: ExitClass, now: Instant) -> Decision {
        if class.is_deterministic() {
            return Decision::GiveUp;
        }
        self.forget_old(now);
        self.crashes.push(now);
        let count = u32::try_from(self.crashes.len()).unwrap_or(u32::MAX);
        if count >= self.policy.max_crashes {
            return Decision::GiveUp;
        }
        let doublings = count.saturating_sub(1).min(16);
        let backoff = self
            .policy
            .initial_backoff
            .saturating_mul(1 << doublings)
            .min(self.policy.max_backoff);
        Decision::RestartAfter(backoff)
    }

    /// Call when a run has been healthy for the policy's window, or when the
    /// user started zebrad explicitly: the crash history no longer applies.
    pub fn reset(&mut self) {
        self.crashes.clear();
    }

    /// The number of crashes inside the window as of `now`.
    pub fn recent_crashes(&mut self, now: Instant) -> u32 {
        self.forget_old(now);
        u32::try_from(self.crashes.len()).unwrap_or(u32::MAX)
    }

    fn forget_old(&mut self, now: Instant) {
        let window = self.policy.window;
        self.crashes
            .retain(|at| now.saturating_duration_since(*at) <= window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RestartPolicy {
        RestartPolicy {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(8),
            window: Duration::from_secs(600),
            max_crashes: 5,
        }
    }

    #[test]
    fn backoff_doubles_until_the_cap_then_gives_up() {
        let mut tracker = RestartTracker::new(policy());
        let start = Instant::now();
        let decisions: Vec<_> = (0..5)
            .map(|i| tracker.on_exit(ExitClass::Crash, start + Duration::from_secs(i)))
            .collect();
        assert_eq!(
            decisions,
            vec![
                Decision::RestartAfter(Duration::from_secs(1)),
                Decision::RestartAfter(Duration::from_secs(2)),
                Decision::RestartAfter(Duration::from_secs(4)),
                Decision::RestartAfter(Duration::from_secs(8)),
                Decision::GiveUp,
            ]
        );
    }

    #[test]
    fn crashes_outside_the_window_are_forgotten() {
        let mut tracker = RestartTracker::new(policy());
        let start = Instant::now();
        for i in 0..4 {
            tracker.on_exit(ExitClass::Crash, start + Duration::from_secs(i));
        }
        let later = start + Duration::from_secs(700);
        assert_eq!(
            tracker.on_exit(ExitClass::Crash, later),
            Decision::RestartAfter(Duration::from_secs(1))
        );
        assert_eq!(tracker.recent_crashes(later), 1);
    }

    #[test]
    fn deterministic_failures_never_restart() {
        let mut tracker = RestartTracker::new(policy());
        for class in [
            ExitClass::ConfigError,
            ExitClass::EosHalt,
            ExitClass::DbLocked,
            ExitClass::PortInUse,
        ] {
            assert_eq!(
                tracker.on_exit(class, Instant::now()),
                Decision::GiveUp,
                "{class:?}"
            );
        }
        assert_eq!(tracker.recent_crashes(Instant::now()), 0);
    }

    #[test]
    fn reset_clears_history() {
        let mut tracker = RestartTracker::new(policy());
        let now = Instant::now();
        tracker.on_exit(ExitClass::Crash, now);
        tracker.on_exit(ExitClass::Crash, now);
        tracker.reset();
        assert_eq!(
            tracker.on_exit(ExitClass::Crash, now),
            Decision::RestartAfter(Duration::from_secs(1))
        );
    }
}
