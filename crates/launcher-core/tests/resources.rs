//! The resource sampler against `fake-zebrad`'s process trees, through the supervisor's probe,
//! on Linux, macOS and Windows.
//!
//! On Unix a reaped child's CPU reaches its parent's rollup; on Windows the job keeps the CPU of
//! every process ever in it, and members have no parent links. Every wait is bounded, so a hang
//! fails fast instead of stalling the test run.

#![cfg(any(target_os = "linux", target_os = "macos", windows))]

use std::time::Duration;

#[cfg(windows)]
use launcher_core::pidfile;
use launcher_core::{
    resources::{self, ResourceSampler, platform_reader},
    supervisor::{self, SupervisorHandle},
};
use launcher_types::{NodeResources, Platform, ProcessUsage};
use tokio::sync::watch;

#[path = "support/helpers.rs"]
mod support;
use support::{LIMIT, fake_zebrad_command, fast_supervisor_config, within, within_limit};

/// The sampler's interval in these tests.
const INTERVAL: Duration = Duration::from_millis(200);

/// Whether members name their parent: on Unix, not on Windows, where the job's list is the tree.
const PARENT_LINKS: bool = cfg!(unix);

/// Whether a child that has exited stays a member until it is reaped: a zombie on Unix keeps
/// its final times, while Windows lists no process that has exited.
const EXITED_ARE_LISTED: bool = cfg!(unix);

/// fake-zebrad as the OS names it.
const FAKE_NAME: &str = if cfg!(windows) {
    "fake-zebrad.exe"
} else {
    "fake-zebrad"
};

struct Live {
    _dir: tempfile::TempDir,
    handle: SupervisorHandle,
    samples: watch::Receiver<Option<NodeResources>>,
}

impl Live {
    /// fake-zebrad with `env`, on a config in a directory on the build's disk (a tmpfs `/tmp`
    /// would not count written bytes), sampled every `interval`.
    async fn start(env: &[(&str, &str)], interval: Duration) -> Live {
        Self::start_with(env, interval, |_| {}).await
    }

    /// Like [`Live::start`], with the supervisor's configuration changed by `configure`.
    async fn start_with(
        env: &[(&str, &str)],
        interval: Duration,
        configure: impl FnOnce(&mut supervisor::SupervisorConfig),
    ) -> Live {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir");
        let config = dir.path().join("zebrad.toml");
        std::fs::write(&config, "[network]\nnetwork = \"Regtest\"\n").expect("write config");
        let mut command = fake_zebrad_command("ok", &config);
        command
            .env
            .extend(env.iter().map(|(key, value)| (key.into(), value.into())));
        let mut config = fast_supervisor_config(command);
        configure(&mut config);
        let handle = supervisor::spawn(config).expect("supervisor");
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

/// Whether `member` is the console host Windows may run for a console program started without
/// a window, as Zeal starts zebrad. It can be a member of the job; these tests leave it out.
fn console_host(member: &ProcessUsage) -> bool {
    cfg!(windows) && member.name.eq_ignore_ascii_case("conhost.exe")
}

/// The tree's processes as the tests count them: without a console host.
fn tree(sample: &NodeResources) -> Vec<&ProcessUsage> {
    sample
        .members
        .iter()
        .filter(|member| !console_host(member))
        .collect()
}

/// The root's own CPU, its children's excluded, and a console host's.
fn root_cpu(sample: &NodeResources) -> f64 {
    let host: f64 = sample
        .members
        .iter()
        .filter(|member| console_host(member))
        .map(|member| member.cpu_secs)
        .sum();
    sample.members.first().map_or(0.0, |root| root.cpu_secs) + host
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
    assert_eq!(sample.platform, resources::platform());
    assert_eq!(tree(&sample).len(), 1, "{sample:?}");
    let root = &sample.members[0];
    assert_eq!(Some(root.pid), live.handle.status().pid);
    assert_eq!(root.parent, None);
    assert_eq!(root.name, FAKE_NAME);
    if sample.platform == Platform::Windows {
        assert_eq!(root.threads, None, "threads are not read on Windows");
    } else {
        assert!(
            root.threads.unwrap_or(0) >= 2,
            "the spinning thread: {root:?}"
        );
    }
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
/// while it lived, is in the total once: the root's own time plus the child's. On Windows the
/// child's last interval before its exit may go unseen, while the job's total holds it.
async fn child_is_counted_once(live: &mut Live) -> NodeResources {
    let child = live
        .until(LIMIT, "the child", |_| {}, |sample| tree(sample).len() == 2)
        .await;
    let members = tree(&child);
    let root = members[0].pid;
    let parent = PARENT_LINKS.then_some(root);
    assert_eq!(members[1].parent, parent, "{child:?}");
    let mut child_cpu = members[1].cpu_secs;
    let gone = live
        .until(
            LIMIT,
            "the child to be reaped",
            |sample| {
                if let Some(member) = tree(sample).get(1) {
                    child_cpu = child_cpu.max(member.cpu_secs);
                }
            },
            |sample| tree(sample).len() == 1,
        )
        .await;
    assert!(child_cpu > 0.2, "the child spun: {child_cpu}");
    let rollup = gone.cpu_secs - root_cpu(&gone);
    let unseen = if EXITED_ARE_LISTED {
        0.0
    } else {
        INTERVAL.as_secs_f64()
    };
    assert!(
        rollup > child_cpu - 0.05 && rollup < child_cpu + unseen + 0.05,
        "the child's {child_cpu} s are counted once: the total holds {rollup} s beside the root's own"
    );
    gone
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_counts_while_it_lives_and_after_it_is_reaped() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_CHILD", "busy")], INTERVAL).await;
    let child = live
        .until(LIMIT, "the child", |_| {}, |sample| tree(sample).len() == 2)
        .await;
    assert_eq!(tree(&child)[1].name, FAKE_NAME);
    child_is_counted_once(&mut live).await;
    live.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_in_a_session_of_its_own_is_a_member() {
    let mut live = Live::start(&[("FAKE_ZEBRAD_CHILD", "session")], INTERVAL).await;
    let sample = live
        .until(LIMIT, "the child", |_| {}, |sample| tree(sample).len() == 2)
        .await;
    // On Unix by its parent link; on Windows, where the child skips `setsid`, by the job.
    let members = tree(&sample);
    let parent = PARENT_LINKS.then_some(members[0].pid);
    assert_eq!(members[1].parent, parent);
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
            |sample| most = most.max(tree(sample).len()),
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

/// Windows: the sampler holds the run's job only while it reads, and what is left in the job
/// dies when the supervisor finalizes the run (`KILL_ON_JOB_CLOSE`, and the handle's drop).
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_job_closes_when_the_run_is_finalized() {
    // A graceful stop: fake-zebrad exits and leaves a grandchild that holds its pipes for 30 s,
    // so the supervisor waits out its exit drain and then finalizes the run.
    let mut live = Live::start_with(
        &[("FAKE_ZEBRAD_LINGER_MS", "30000")],
        Duration::from_millis(50),
        |config| {
            config.command.shutdown_on_stdin_close = true;
            config.exit_drain = Duration::from_millis(1500);
        },
    )
    .await;
    let first = live.until(LIMIT, "a sample", |_| {}, |_| true).await;
    let root = first.members[0].pid;
    let probe = live
        .handle
        .watch_probe()
        .borrow()
        .clone()
        .expect("the run's probe");
    let stop = tokio::spawn({
        let handle = live.handle.clone();
        async move { handle.stop().await }
    });
    // During the exit drain the job holds the grandchild alone.
    let lingering = live
        .until(
            LIMIT,
            "the grandchild",
            |_| {},
            |sample| tree(sample).iter().any(|member| member.pid != root),
        )
        .await;
    let grandchild = tree(&lingering)
        .into_iter()
        .find(|member| member.pid != root)
        .expect("the grandchild")
        .pid;
    within(stop).await.expect("the stop's task").expect("stop");
    // Finalized: closing the job killed the grandchild, long before its 30 s were up.
    within_limit(Duration::from_secs(5), "the grandchild's end", async {
        while pidfile::is_alive(grandchild) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // A sample read in flight may still hold the job for a moment after the stop: until it
    // lets go, the probe reads the closing job with no members left. Then it reads nothing.
    let err = within_limit(Duration::from_secs(5), "the job to close", async {
        loop {
            match platform_reader().tree(&probe, &[]) {
                Err(err) => break err,
                Ok(reading) => {
                    assert!(reading.members.is_empty(), "{reading:?}");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
    })
    .await;
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    live.stop().await;
}
