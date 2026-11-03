//! The upload budget: one bucket of KiB permits for every connection together, refilled by a
//! ticker at the configured rate and holding at most one second of it.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use tokio::{sync::Semaphore, task::JoinHandle};

/// How often the bucket is refilled.
const TICK: Duration = Duration::from_millis(100);

/// The smallest bucket, so one read's worth always fits.
const MIN_CAPACITY_KIB: u32 = 64;

/// The bucket's size for `kbit` per second: one second's worth, in KiB.
fn capacity_kib(kbit: u32) -> u32 {
    (kbit.saturating_mul(125) / 1024).max(MIN_CAPACITY_KIB)
}

/// The upload budget. Dropping it stops the ticker.
#[derive(Debug)]
pub(crate) struct Bucket {
    permits: Arc<Semaphore>,
    /// The rate in kbit/s; 0 means no limit.
    kbit: Arc<AtomicU32>,
    ticker: JoinHandle<()>,
}

impl Drop for Bucket {
    fn drop(&mut self) {
        self.ticker.abort();
    }
}

impl Bucket {
    /// A full bucket for `kbit` per second, `None` for no limit.
    pub(crate) fn new(kbit: Option<u32>) -> Arc<Self> {
        let rate = kbit.unwrap_or(0);
        let permits = Arc::new(Semaphore::new(capacity_kib(rate) as usize));
        let kbit = Arc::new(AtomicU32::new(rate));
        let ticker = tokio::spawn(refill(permits.clone(), kbit.clone()));
        Arc::new(Bucket {
            permits,
            kbit,
            ticker,
        })
    }

    /// Changes the rate. Lifting the limit releases every waiting writer.
    pub(crate) fn set(&self, kbit: Option<u32>) {
        let rate = kbit.unwrap_or(0);
        self.kbit.store(rate, Ordering::Relaxed);
        if rate == 0 {
            self.permits.add_permits(capacity_kib(0) as usize);
        }
    }

    /// Waits until `bytes` more may be sent. `debt` carries the bytes under a whole KiB from
    /// one call to the next, per connection.
    pub(crate) async fn spend(&self, bytes: usize, debt: &mut usize) {
        let rate = self.kbit.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        *debt += bytes;
        let mut kib = *debt / 1024;
        *debt %= 1024;
        let capacity = capacity_kib(rate) as usize;
        while kib > 0 {
            if self.kbit.load(Ordering::Relaxed) == 0 {
                return;
            }
            let take = kib.min(capacity);
            match self.permits.acquire_many(take as u32).await {
                Ok(permits) => permits.forget(),
                Err(_) => return,
            }
            kib -= take;
        }
    }
}

async fn refill(permits: Arc<Semaphore>, kbit: Arc<AtomicU32>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick is immediate, and the bucket starts full.
    tick.tick().await;
    // Bytes per tick in KiB, with the remainder carried over.
    let mut carry: u64 = 0;
    loop {
        tick.tick().await;
        let rate = kbit.load(Ordering::Relaxed);
        if rate == 0 {
            continue;
        }
        let bytes = u64::from(rate) * 125 * TICK.as_millis() as u64 / 1000 + carry;
        let kib = bytes / 1024;
        carry = bytes % 1024;
        let capacity = capacity_kib(rate) as usize;
        let room = capacity.saturating_sub(permits.available_permits());
        let add = (kib as usize).min(room);
        if add > 0 {
            permits.add_permits(add);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn a_bucket_holds_one_second() {
        assert_eq!(capacity_kib(10_000), 1220);
        assert_eq!(capacity_kib(100), MIN_CAPACITY_KIB);
    }

    #[tokio::test]
    async fn spending_past_the_burst_waits_for_the_rate() {
        // 1,024 kbit/s is 125 KiB a second; the bucket starts with that much.
        let bucket = Bucket::new(Some(1_024));
        let mut debt = 0;
        let start = Instant::now();
        bucket.spend(125 * 1024, &mut debt).await;
        assert!(start.elapsed() < Duration::from_millis(100));
        bucket.spend(25 * 1024, &mut debt).await;
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
        // Small writes add up instead of each costing a KiB.
        bucket.spend(1000, &mut debt).await;
        assert_eq!(debt, 1000);
        bucket.spend(100, &mut debt).await;
        assert_eq!(debt, 76);
    }

    #[tokio::test]
    async fn no_limit_never_waits() {
        let bucket = Bucket::new(None);
        let mut debt = 0;
        let start = Instant::now();
        bucket.spend(100 * 1024 * 1024, &mut debt).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
