//! The order the explorer's reads go out in, and how many at once: pure, tested without tokio.
//!
//! zebrad has no rate limit or backpressure of its own (its read service is always ready), so
//! the explorer limits itself: a few reads at a time, interactive ones first and the newest of
//! those first, prefetches in what is left. A heavy read (a whole block at verbosity 3) weighs
//! three; one runs while other interactive reads wait, two otherwise. The actor owns the
//! scheduler and the permits; a read's lane and weight are counted until its task reports back.

use std::collections::VecDeque;

/// What a heavy read weighs; a cheap one weighs one.
pub(crate) const HEAVY: u32 = 3;

/// Who is waiting for a read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Lane {
    /// A page on screen, and following the tip, which pages wait for.
    Interactive,
    /// Work a page can wait for: the Chain page's coinbases, hover cards, neighbours, the
    /// outputs a big block's rows spend.
    Prefetch,
}

/// How much may run, in weight: a cheap read weighs one, a heavy one [`HEAVY`].
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
    interactive: VecDeque<(K, u32)>,
    prefetch: VecDeque<(K, u32)>,
    /// The weight running in each lane.
    running: [u32; 2],
    /// Heavy reads running.
    heavy: u32,
    limits: Limits,
}

impl<K: Clone + PartialEq> Scheduler<K> {
    pub(crate) fn new(limits: Limits) -> Self {
        Self {
            interactive: VecDeque::new(),
            prefetch: VecDeque::new(),
            running: [0, 0],
            heavy: 0,
            limits,
        }
    }

    pub(crate) fn set_limits(&mut self, limits: Limits) {
        self.limits = limits;
    }

    /// Queues `key`, weighing `weight`, in `lane`. A prefetch heavier than its lane may ever
    /// run goes in the interactive lane, so it cannot hold the prefetch lane up for good.
    pub(crate) fn push(&mut self, key: K, lane: Lane, weight: u32) {
        let weight = weight.max(1);
        match lane {
            Lane::Prefetch if weight <= Limits::SYNCING.prefetch.min(self.limits.prefetch) => {
                self.prefetch.push_back((key, weight))
            }
            _ => self.interactive.push_back((key, weight)),
        }
    }

    /// A queued prefetch that a page now waits for moves to the interactive lane.
    pub(crate) fn promote(&mut self, key: &K) {
        if let Some(position) = self.prefetch.iter().position(|(queued, _)| queued == key)
            && let Some(entry) = self.prefetch.remove(position)
        {
            self.interactive.push_back(entry);
        }
    }

    /// The next read to start, with its lane and weight, if the limits allow one; it counts as
    /// running from now. Interactive reads go first, the newest first; prefetches in arrival
    /// order, while no interactive read waits for room. A heavy read that does not fit holds
    /// back the reads queued behind it, so it is not starved; one held back by the heavy rule
    /// lets them pass.
    pub(crate) fn next(&mut self) -> Option<(K, Lane, u32)> {
        let free = self
            .limits
            .total
            .saturating_sub(self.running[0] + self.running[1]);
        let room = free.min(self.limits.interactive.saturating_sub(self.running[0]));
        let waiting = self.interactive.len();
        for position in (0..waiting).rev() {
            let weight = self.interactive[position].1;
            let heavy = weight > 1;
            if heavy && !self.heavy_allowed(waiting > 1) {
                continue;
            }
            if weight > room {
                // Held for room: nothing behind it goes, prefetches neither.
                return None;
            }
            let (key, weight) = self.interactive.remove(position)?;
            return Some(self.start(key, Lane::Interactive, weight));
        }
        let room = free.min(self.limits.prefetch.saturating_sub(self.running[1]));
        let (_, weight) = self.prefetch.front()?;
        let weight = *weight;
        if weight > room || (weight > 1 && !self.heavy_allowed(waiting > 0)) {
            return None;
        }
        let (key, weight) = self.prefetch.pop_front()?;
        Some(self.start(key, Lane::Prefetch, weight))
    }

    /// Whether another heavy read may start: none runs, or one does and no other interactive
    /// read waits.
    fn heavy_allowed(&self, others_wait: bool) -> bool {
        self.heavy == 0 || (self.heavy == 1 && !others_wait)
    }

    fn start(&mut self, key: K, lane: Lane, weight: u32) -> (K, Lane, u32) {
        self.running[slot(lane)] += weight;
        if weight > 1 {
            self.heavy += 1;
        }
        (key, lane, weight)
    }

    /// A read of `lane` weighing `weight` finished, however it ended.
    pub(crate) fn done(&mut self, lane: Lane, weight: u32) {
        let slot = slot(lane);
        self.running[slot] = self.running[slot].saturating_sub(weight);
        if weight > 1 {
            self.heavy = self.heavy.saturating_sub(1);
        }
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
            .map(|(key, _)| key)
            .collect()
    }
}

fn slot(lane: Lane) -> usize {
    match lane {
        Lane::Interactive => 0,
        Lane::Prefetch => 1,
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
        sched.push("prefetch", Lane::Prefetch, 1);
        sched.push("old", Lane::Interactive, 1);
        sched.push("new", Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("new", Lane::Interactive, 1)));
        assert_eq!(sched.next(), Some(("old", Lane::Interactive, 1)));
        assert_eq!(sched.next(), None, "both slots taken");
        sched.done(Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("prefetch", Lane::Prefetch, 1)));
        assert_eq!(sched.queued(), 0);
    }

    #[test]
    fn prefetches_have_their_own_cap() {
        let mut sched = Scheduler::new(Limits::NORMAL);
        for key in 0..5 {
            sched.push(key, Lane::Prefetch, 1);
        }
        assert_eq!(sched.next(), Some((0, Lane::Prefetch, 1)));
        assert_eq!(sched.next(), Some((1, Lane::Prefetch, 1)));
        assert_eq!(sched.next(), None, "two prefetches at most");
        // A page wants one of the queued prefetches: it goes now.
        sched.promote(&4);
        assert_eq!(sched.next(), Some((4, Lane::Interactive, 1)));
        sched.done(Lane::Prefetch, 1);
        assert_eq!(sched.next(), Some((2, Lane::Prefetch, 1)));
    }

    #[test]
    fn syncing_lowers_the_limits() {
        let mut sched = Scheduler::new(Limits::NORMAL);
        sched.set_limits(Limits::SYNCING);
        for key in 0..6 {
            sched.push(key, Lane::Interactive, 1);
        }
        let started = std::iter::from_fn(|| sched.next()).count();
        assert_eq!(started, 4);
        assert_eq!(sched.clear_queue().len(), 2);
        // A finished read of a lane never drops below zero.
        sched.done(Lane::Prefetch, 1);
        assert_eq!(sched.next(), None);
    }

    #[test]
    fn a_heavy_read_weighs_three_and_one_runs_while_others_wait() {
        let mut sched = Scheduler::new(Limits::NORMAL);
        sched.push("v3 a", Lane::Interactive, HEAVY);
        sched.push("v3 b", Lane::Interactive, HEAVY);
        sched.push("tx", Lane::Interactive, 1);
        // The newest first: the cheap read, then one heavy read; the second waits while
        // another interactive read is queued.
        assert_eq!(sched.next(), Some(("tx", Lane::Interactive, 1)));
        assert_eq!(sched.next(), Some(("v3 b", Lane::Interactive, HEAVY)));
        sched.push("tx 2", Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("tx 2", Lane::Interactive, 1)));
        assert_eq!(
            sched.next(),
            None,
            "five of six taken; a heavy read needs three"
        );
        // With nothing else waiting, a second heavy read may run beside the first.
        sched.done(Lane::Interactive, 1);
        sched.done(Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("v3 a", Lane::Interactive, HEAVY)));
        sched.done(Lane::Interactive, HEAVY);
        sched.done(Lane::Interactive, HEAVY);
        assert_eq!(sched.queued(), 0);
    }

    #[test]
    fn a_heavy_read_waiting_for_room_holds_the_reads_behind_it() {
        let mut sched = Scheduler::new(Limits::NORMAL);
        for key in ["a", "b", "c", "d"] {
            sched.push(key, Lane::Interactive, 1);
        }
        assert_eq!(std::iter::from_fn(|| sched.next()).count(), 4);
        // Two left: the heavy read waits for a third, and neither the older cheap read nor a
        // prefetch takes the room meanwhile.
        sched.push("older", Lane::Interactive, 1);
        sched.push("v3", Lane::Interactive, HEAVY);
        sched.push("prefetch", Lane::Prefetch, 1);
        assert_eq!(sched.next(), None);
        sched.done(Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("v3", Lane::Interactive, HEAVY)));
        assert_eq!(sched.next(), None, "all six taken");
        sched.done(Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("older", Lane::Interactive, 1)));
        sched.done(Lane::Interactive, 1);
        assert_eq!(sched.next(), Some(("prefetch", Lane::Prefetch, 1)));
    }
}
