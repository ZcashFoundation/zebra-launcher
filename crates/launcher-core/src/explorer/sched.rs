//! The order the explorer's reads go out in, and how many at once: pure, tested without tokio.
//!
//! zebrad has no rate limit or backpressure of its own (its read service is always ready), so
//! the explorer limits itself: a few reads at a time, interactive ones first and the newest of
//! those first, prefetches in what is left. The actor owns the scheduler and the permits; a
//! read's lane is counted until its task reports back.

use std::collections::VecDeque;

/// Who is waiting for a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Lane {
    /// A page on screen.
    Interactive,
    /// Work a page can wait for, and following the tip.
    Prefetch,
}

/// How many reads may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// In all.
    pub(crate) total: u32,
    pub(crate) interactive: u32,
    pub(crate) prefetch: u32,
}

impl Limits {
    /// While the node is at the tip.
    pub(crate) const NORMAL: Limits = Limits {
        total: 6,
        interactive: 6,
        prefetch: 2,
    };
    /// While the node syncs, it has less to spare.
    pub(crate) const SYNCING: Limits = Limits {
        total: 4,
        interactive: 4,
        prefetch: 1,
    };
}

#[derive(Debug)]
pub(crate) struct Scheduler<K> {
    interactive: VecDeque<K>,
    prefetch: VecDeque<K>,
    running: [u32; 2],
    limits: Limits,
}

impl<K: Clone + PartialEq> Scheduler<K> {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            interactive: VecDeque::new(),
            prefetch: VecDeque::new(),
            running: [0, 0],
            limits,
        }
    }

    pub(crate) fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// Queues `key` in `lane`.
    pub(crate) fn push(&mut self, key: K, lane: Lane) {
        match lane {
            Lane::Interactive => self.interactive.push_back(key),
            Lane::Prefetch => self.prefetch.push_back(key),
        }
    }

    /// A queued prefetch that a page now waits for moves to the interactive lane.
    pub(crate) fn promote(&mut self, key: &K) {
        if let Some(position) = self.prefetch.iter().position(|queued| queued == key)
            && let Some(key) = self.prefetch.remove(position)
        {
            self.interactive.push_back(key);
        }
    }

    /// The next read to start, if the limits allow one; it counts as running from now.
    /// Interactive reads go first, the newest first; prefetches in arrival order.
    pub(crate) fn next(&mut self) -> Option<(K, Lane)> {
        if self.running[0] + self.running[1] >= self.limits.total {
            return None;
        }
        if self.running[0] < self.limits.interactive
            && let Some(key) = self.interactive.pop_back()
        {
            self.running[0] += 1;
            return Some((key, Lane::Interactive));
        }
        if self.running[1] < self.limits.prefetch
            && let Some(key) = self.prefetch.pop_front()
        {
            self.running[1] += 1;
            return Some((key, Lane::Prefetch));
        }
        None
    }

    /// A read of `lane` finished, however it ended.
    pub(crate) fn done(&mut self, lane: Lane) {
        let slot = match lane {
            Lane::Interactive => 0,
            Lane::Prefetch => 1,
        };
        self.running[slot] = self.running[slot].saturating_sub(1);
    }

    /// Reads waiting to start.
    #[cfg(test)]
    pub(crate) fn queued(&self) -> usize {
        self.interactive.len() + self.prefetch.len()
    }

    /// Drops every queued read; running ones still report back.
    pub(crate) fn clear_queue(&mut self) -> Vec<K> {
        self.interactive
            .drain(..)
            .chain(self.prefetch.drain(..))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_reads_go_first_and_the_newest_first() {
        let mut sched = Scheduler::new(Limits {
            total: 2,
            interactive: 2,
            prefetch: 1,
        });
        sched.push("prefetch", Lane::Prefetch);
        sched.push("old", Lane::Interactive);
        sched.push("new", Lane::Interactive);
        assert_eq!(sched.next(), Some(("new", Lane::Interactive)));
        assert_eq!(sched.next(), Some(("old", Lane::Interactive)));
        assert_eq!(sched.next(), None, "both slots taken");
        sched.done(Lane::Interactive);
        assert_eq!(sched.next(), Some(("prefetch", Lane::Prefetch)));
        assert_eq!(sched.queued(), 0);
    }

    #[test]
    fn prefetches_have_their_own_cap() {
        let mut sched = Scheduler::new(Limits::NORMAL);
        for key in 0..5 {
            sched.push(key, Lane::Prefetch);
        }
        assert_eq!(sched.next(), Some((0, Lane::Prefetch)));
        assert_eq!(sched.next(), Some((1, Lane::Prefetch)));
        assert_eq!(sched.next(), None, "two prefetches at most");
        // A page wants one of the queued prefetches: it goes now.
        sched.promote(&4);
        assert_eq!(sched.next(), Some((4, Lane::Interactive)));
        sched.done(Lane::Prefetch);
        assert_eq!(sched.next(), Some((2, Lane::Prefetch)));
    }

    #[test]
    fn syncing_lowers_the_limits() {
        let mut sched = Scheduler::new(Limits::NORMAL);
        sched.set_limits(Limits::SYNCING);
        for key in 0..6 {
            sched.push(key, Lane::Interactive);
        }
        let started = std::iter::from_fn(|| sched.next()).count();
        assert_eq!(started, 4);
        assert_eq!(sched.clear_queue().len(), 2);
        // A finished read of a lane never drops below zero.
        sched.done(Lane::Prefetch);
        assert_eq!(sched.next(), None);
    }
}
