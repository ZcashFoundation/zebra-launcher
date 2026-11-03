//! The resource sampler against `fake-zebrad`'s process trees, through the supervisor's probe.
//! Linux only until the macOS and Windows readers exist (docs/RESOURCES.md, phase 3).
//!
//! Every wait is bounded, so a hang fails fast instead of stalling the test run.

#![cfg(target_os = "linux")]

use std::time::Duration;

use launcher_core::{
    resources::{ResourceSampler, platform_reader},
    supervisor::{self, SupervisorHandle},
};
use launcher_types::{NodeResources, Platform};
use tokio::sync::watch;

#[path = "support/helpers.rs"]
mod support;
use support::{LIMIT, fake_zebrad_command, fast_supervisor_config, within, within_limit};

/// The sampler's interval in these tests.
const INTERVAL: Duration = Duration::from_millis(200);

struct Live {
    _dir: tempfile::TempDir,
    handle: SupervisorHandle,
    samples: watch::Receiver<Option<NodeResources>>,
}

impl Live {
    /// fake-zebrad with `env`, on a config in a directory on the build's disk (a tmpfs `/tmp`
    /// would not count written bytes), sampled every `interval`.
    async fn start(env: &[(&str, &str)], interval: Duration) -> Live {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir");
        let config = dir.path().join("zebrad.toml");
        std::fs::write(&config, "[network]\nnetwork = \"Regtest\"\n").expect("write config");
        let mut command = fake_zebrad_command("ok", &config);
        command
            .env
            .extend(env.iter().map(|(key, value)| (key.into(), value.into())));
        let handle = supervisor::spawn(fast_supervisor_config(command)).expect("supervisor");
        let samples =
            ResourceSampler::spawn_with(handle.watch_probe(), interval, platform_reader());
        within(handle.start()).await.expect("start");
        Live {
            _dir: dir,
            handle,
            samples,
        }
    }

    /// The first sample for which `done` holds, within `limit`; every sample on the way is
    /// passed to `seen`.
    async fn until(
        &mut self,
        limit: Duration,
        what: &str,
        mut seen: impl FnMut(&NodeResources),
        mut done: impl FnMut(&NodeResources) -> bool,
    ) -> NodeResources {
        within_limit(limit, what, async {
            loop {
                if let Some(sample) = self.samples.borrow_and_update().clone() {
                    seen(&sample);
                    if done(&sample) {
                        return sample;
                    }
                }
                self.samples.changed().await.expect("the sampler is alive");
            }
        })
        .await
    }

    async fn stop(self) {
        within(self.handle.shutdown()).await.expect("shutdown");
    }
}

/// The root's own CPU, its children's excluded.
fn root_cpu(sample: &NodeResources) -> f64 {
    sample.members.first().map_or(0.0, |root| root.cpu_secs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_thread_shows_as_cpu() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_BURN_MS", "1500")], INTERVAL).await;
    let mut busiest = 0.0_f64;
    let sample = live
        .until(
            LIMIT,
            "1.5 s of spinning",
            |sample| busiest = busiest.max(sample.cpu_cores.unwrap_or(0.0)),
            |sample| sample.cpu_secs >= 0.8,
        )
        .await;
    assert_eq!(sample.platform, Platform::Linux);
    assert_eq!(sample.processes, 1);
    let root = &sample.members[0];
    assert_eq!(Some(root.pid), live.handle.status().pid);
    assert_eq!(root.parent, None);
    assert!(
        root.threads.unwrap_or(0) >= 2,
        "the spinning thread: {root:?}"
    );
    assert!(sample.descriptors.unwrap_or(0) > 0);
    assert!(busiest > 0.3, "some interval was busy: {busiest}");
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_memory_shows_with_its_peak() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_ALLOC_MB", "128")], INTERVAL).await;
    let sample = live
        .until(
            LIMIT,
            "128 MiB resident",
            |_| {},
            |sample| sample.memory_bytes >= 100 << 20,
        )
        .await;
    assert!(
        sample.peak_memory_bytes >= sample.memory_bytes,
        "{sample:?}"
    );
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn written_bytes_count_from_the_spawn() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_WRITE_MB", "32")], INTERVAL).await;
    let sample = live
        .until(
            LIMIT,
            "32 MiB written",
            |_| {},
            |sample| sample.written_bytes.unwrap_or(0) >= 32_000_000,
        )
        .await;
    assert!(sample.read_bytes.is_some(), "{sample:?}");
    live.stop().await;
}

/// Waits until the child shows up and is gone again, and checks that its CPU, still visible
/// while it lived, is in the total once: the root's own time plus the child's.
async fn child_is_counted_once(live: &mut Live) -> NodeResources {
    let child = live
        .until(LIMIT, "the child", |_| {}, |sample| sample.processes == 2)
        .await;
    let root = child.members[0].pid;
    assert_eq!(child.members[1].parent, Some(root), "{child:?}");
    let mut child_cpu = child.members[1].cpu_secs;
    let gone = live
        .until(
            LIMIT,
            "the child to be reaped",
            |sample| {
                if let Some(member) = sample.members.get(1) {
                    child_cpu = child_cpu.max(member.cpu_secs);
                }
            },
            |sample| sample.processes == 1,
        )
        .await;
    assert!(child_cpu > 0.2, "the child spun: {child_cpu}");
    let rollup = gone.cpu_secs - root_cpu(&gone);
    assert!(
        (rollup - child_cpu).abs() < 0.05,
        "the child's {child_cpu} s are counted once: the total holds {rollup} s beside the root's own"
    );
    gone
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_counts_while_it_lives_and_after_it_is_reaped() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_CHILD", "busy")], INTERVAL).await;
    let child = live
        .until(LIMIT, "the child", |_| {}, |sample| sample.processes == 2)
        .await;
    assert_eq!(child.members[1].name, "fake-zebrad");
    child_is_counted_once(&mut live).await;
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_in_a_session_of_its_own_is_a_member() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_CHILD", "session")], INTERVAL).await;
    let sample = live
        .until(LIMIT, "the child", |_| {}, |sample| sample.processes == 2)
        .await;
    assert_eq!(sample.members[1].parent, Some(sample.members[0].pid));
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_between_two_samples_is_counted_but_never_listed() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_CHILD", "short")], Duration::from_secs(1)).await;
    let mut most = 0;
    let sample = live
        .until(
            LIMIT,
            "three seconds of samples",
            |sample| most = most.max(sample.processes),
            |sample| sample.run_secs >= 3.0,
        )
        .await;
    assert_eq!(most, 1, "the child lived between two samples");
    let rollup = sample.cpu_secs - root_cpu(&sample);
    assert!(
        rollup >= 0.02,
        "its CPU reached the root's rollup: {rollup}"
    );
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_reaped_late_is_counted_once() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_CHILD", "late")], INTERVAL).await;
    // A zombie for 15 s, then reaped.
    child_is_counted_once(&mut live).await;
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn samples_stop_with_the_run_and_start_again_with_the_next() {
    let mut live = Live::start(&[], INTERVAL).await;
    let first = live.until(LIMIT, "a sample", |_| {}, |_| true).await;
    within(live.handle.stop()).await.expect("stop");
    within(async {
        while live.samples.borrow_and_update().is_some() {
            live.samples.changed().await.expect("the sampler is alive");
        }
    })
    .await;
    within(live.handle.start()).await.expect("start again");
    let second = live
        .until(
            LIMIT,
            "the next run's sample",
            |_| {},
            |sample| sample.run != first.run,
        )
        .await;
    assert!(second.run > first.run);
    assert!(
        second.cpu_secs < first.cpu_secs + 5.0,
        "counted from the new spawn"
    );
    live.stop().await;
}
