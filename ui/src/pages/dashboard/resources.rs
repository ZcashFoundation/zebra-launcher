//! The Resources card (docs/RESOURCES.md): what the node's process tree uses, from the samples
//! the backend takes every 5 s from the spawn to the end of the run. Four tiles (CPU, Memory,
//! Disk or I/O, Processes) with one-minute rates, the table of the tree's processes, and a line
//! about the computer. The card stays hidden until the node's first sample; a stopped node
//! shows its last run, never live-looking numbers. No figure here takes a warning tone. The
//! sentences are pure functions, tested natively.

use std::time::SystemTime;

use launcher_types::{MachineResources, NodeResources, Platform};
use leptos::prelude::*;

use crate::{
    charts::{
        byte_rate, hhmm, tz_offset_min_at, uptime_text,
        widgets::{StatTile, Tone},
    },
    format::{bytes, cores, cores_number, mem_bytes, mem_of, share, thousands},
    insights::SyncStage,
    resource_history::{ProcessRate, RATE_WINDOW, RunSummary, SparkKind},
};

use super::Dash;

/// The card's subtitle: `zebrad and the processes it started · every 5 s`. On Windows the job
/// holds every process the node starts, so it says `every process`; on Unix a process that
/// leaves the group and is orphaned before it is first seen is missed.
pub fn subtitle(root: &str, platform: Platform) -> String {
    let which = if platform.tree_is_exact() {
        "every process"
    } else {
        "the processes"
    };
    format!("{root} and {which} it started · every 5 s")
}

/// What the figures mean on `platform`, said once instead of on every number.
pub fn basis_line(platform: Platform) -> String {
    let io = match platform {
        Platform::Linux => Some("disk bytes at the storage layer"),
        Platform::Macos => None,
        Platform::Windows => Some("I/O counts files, network and devices"),
    };
    match io {
        Some(io) => format!("{} · {io}", platform.memory_label()),
        None => platform.memory_label().to_owned(),
    }
}

/// The CPU tile's detail: the share of the computer, then why it is high while the node syncs
/// and the tree uses half the CPUs or more (`70 % of 8 CPUs · syncing uses every CPU`), or
/// the run's average otherwise (`3 % of 8 CPUs · 2.1 on average this run`).
pub fn cpu_detail(
    cores_now: Option<f64>,
    logical_cpus: Option<u32>,
    syncing: bool,
    run_average: Option<f64>,
) -> String {
    let mut parts = Vec::with_capacity(2);
    let cpus = logical_cpus.filter(|&cpus| cpus > 0);
    if let (Some(cores_now), Some(cpus)) = (cores_now, cpus) {
        parts.push(format!(
            "{} of {cpus} CPUs",
            share(cores_now / f64::from(cpus))
        ));
    }
    let busy = cores_now
        .zip(cpus)
        .is_some_and(|(cores, cpus)| cores / f64::from(cpus) >= 0.5);
    if syncing && busy {
        parts.push("syncing uses every CPU".to_owned());
    } else if let Some(average) = run_average {
        parts.push(format!("{} on average this run", cores_number(average)));
    }
    parts.join(" · ")
}

/// The Memory tile's detail: `38 % of 16 GiB · peak 6.4 GiB`, and on Windows the commit
/// charge, `· 7.0 GiB committed`.
pub fn memory_detail(memory: u64, total: Option<u64>, peak: u64, committed: Option<u64>) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(total) = total.filter(|&total| total > 0) {
        parts.push(format!(
            "{} of {}",
            share(memory as f64 / total as f64),
            mem_bytes(total)
        ));
    }
    parts.push(format!("peak {}", mem_bytes(peak)));
    if let Some(committed) = committed {
        parts.push(format!("{} committed", mem_bytes(committed)));
    }
    parts.join(" · ")
}

/// The Disk (or I/O) tile's value over the last minute, on two lines: `35.0 MB/s read`,
/// `95.0 MB/s written`.
pub fn io_value(read: Option<f64>, written: Option<f64>) -> String {
    match (read, written) {
        (None, None) => "—".to_owned(),
        (read, written) => {
            let rate = |rate: Option<f64>| rate.map_or_else(|| "—".to_owned(), byte_rate);
            format!("{} read\n{} written", rate(read), rate(written))
        }
    }
}

/// The Disk (or I/O) tile's detail: the run's totals, `12.3 GB read, 40.1 GB written this
/// run`, and on Windows what the counters count.
pub fn io_detail(read: Option<u64>, written: Option<u64>, platform: Platform) -> String {
    let total = |value: Option<u64>| value.map_or_else(|| "—".to_owned(), bytes);
    let totals = if read.is_none() && written.is_none() {
        "the system does not report them".to_owned()
    } else {
        format!("{} read, {} written this run", total(read), total(written))
    };
    match platform {
        Platform::Windows => format!("{totals} · files, network and devices"),
        Platform::Linux | Platform::Macos => totals,
    }
}

/// The Processes tile's value, on two lines: `1 process`, `68 threads`; `2 processes` alone
/// without a thread count.
pub fn processes_value(processes: u32, threads: Option<u32>) -> String {
    let processes = match processes {
        1 => "1 process".to_owned(),
        n => format!("{} processes", thousands(u64::from(n))),
    };
    match threads {
        Some(1) => format!("{processes}\n1 thread"),
        Some(threads) => format!("{processes}\n{} threads", thousands(u64::from(threads))),
        None => processes,
    }
}

/// The Processes tile's detail: `412 open files`, `1,204 handles`.
pub fn descriptors_detail(descriptors: Option<u32>, platform: Platform) -> String {
    descriptors.map_or_else(String::new, |count| {
        format!(
            "{} {}",
            thousands(u64::from(count)),
            platform.descriptors_label()
        )
    })
}

/// What a stopped node's card says about its last run: `last run: average 2.1 cores · peak
/// 6.8 GiB · read 1,234.5 GB, wrote 340.2 GB · 3 h 12 min`.
pub fn last_run_text(summary: &RunSummary) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(average) = summary.avg_cores {
        parts.push(format!("average {}", cores(average)));
    }
    parts.push(format!("peak {}", mem_bytes(summary.peak)));
    match (summary.read, summary.written) {
        (Some(read), Some(written)) => {
            parts.push(format!("read {}, wrote {}", bytes(read), bytes(written)));
        }
        (Some(read), None) => parts.push(format!("read {}", bytes(read))),
        (None, Some(written)) => parts.push(format!("wrote {}", bytes(written))),
        (None, None) => {}
    }
    parts.push(uptime_text(summary.length));
    format!("last run: {}", parts.join(" · "))
}

/// The line about the computer: `This computer: 8 CPUs, 81 % busy · 16 GiB, 4.7 GiB
/// available · swap 0.3 of 2 GiB`, with the commit charge on Windows and the memory pressure
/// on macOS.
pub fn machine_text(machine: &MachineResources) -> String {
    let cpus = match machine.logical_cpus {
        1 => "1 CPU".to_owned(),
        n => format!("{n} CPUs"),
    };
    let cpu = match machine.cpu_busy_cores {
        Some(busy) if machine.logical_cpus > 0 => format!(
            "{cpus}, {} busy",
            share(busy / f64::from(machine.logical_cpus))
        ),
        _ => cpus,
    };
    let memory = match machine.memory_available_bytes {
        Some(available) => format!(
            "{}, {} available",
            mem_bytes(machine.memory_total_bytes),
            mem_bytes(available)
        ),
        None => mem_bytes(machine.memory_total_bytes),
    };
    let mut parts = vec![cpu, memory];
    match (machine.swap_used_bytes, machine.swap_total_bytes) {
        (_, Some(0)) => parts.push("no swap".to_owned()),
        (Some(used), Some(total)) => parts.push(format!("swap {}", mem_of(used, total))),
        _ => {}
    }
    if let (Some(commit), Some(limit)) = (machine.commit_bytes, machine.commit_limit_bytes) {
        parts.push(format!("committed {}", mem_of(commit, limit)));
    }
    if let Some(pressure) = machine.pressure {
        parts.push(format!("memory pressure {}", pressure.as_str()));
    }
    format!("This computer: {}", parts.join(" · "))
}

/// One row of the process table.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessRow {
    pub pid: u32,
    pub name: String,
    /// How far under the first process: 0 for it and for processes whose parent is not in
    /// the tree, 1 for its children and so on.
    pub depth: usize,
    pub started_at: SystemTime,
    /// Cores over the last minute, `5.6`.
    pub cpu: String,
    pub memory: String,
    pub read: String,
    pub written: String,
    pub threads: String,
}

/// The process table's rows: the sample's processes in its order (tree order, each process
/// before its children), with the rates of [`ResourceHistory::process_rates`] beside them.
///
/// [`ResourceHistory::process_rates`]: crate::resource_history::ResourceHistory::process_rates
pub fn process_rows(sample: &NodeResources, rates: &[ProcessRate]) -> Vec<ProcessRow> {
    let mut depths: Vec<(u32, usize)> = Vec::with_capacity(sample.members.len());
    let dash = || "—".to_owned();
    sample
        .members
        .iter()
        .map(|member| {
            let depth = member
                .parent
                .and_then(|parent| depths.iter().find(|(pid, _)| *pid == parent))
                .map_or(0, |(_, depth)| depth + 1);
            depths.push((member.pid, depth));
            let rate = rates.iter().find(|rate| rate.pid == member.pid);
            ProcessRow {
                pid: member.pid,
                name: member.name.clone(),
                depth,
                started_at: member.started_at,
                cpu: rate
                    .and_then(|rate| rate.cores)
                    .map_or_else(dash, cores_number),
                memory: member.memory_bytes.map_or_else(dash, mem_bytes),
                read: rate
                    .and_then(|rate| rate.read_per_sec)
                    .map_or_else(dash, byte_rate),
                written: rate
                    .and_then(|rate| rate.written_per_sec)
                    .map_or_else(dash, byte_rate),
                threads: member
                    .threads
                    .map_or_else(dash, |threads| thousands(u64::from(threads))),
            }
        })
        .collect()
}

/// The last row when the sample lists fewer processes than the tree has: `and 12 more`.
pub fn more_text(sample: &NodeResources) -> Option<String> {
    let listed = u32::try_from(sample.members.len()).unwrap_or(u32::MAX);
    (sample.processes > listed).then(|| format!("and {} more", sample.processes - listed))
}

/// `value` while the node has a run, a dash otherwise.
fn while_live(
    live: Memo<bool>,
    value: impl Fn() -> String + Send + Sync + 'static,
) -> Signal<String> {
    Signal::derive(move || {
        if live.get() {
            value()
        } else {
            "—".to_owned()
        }
    })
}

/// Whether zebrad's own progress lines say it is catching up. They say so whenever it is
/// behind the tip, also while it has no peers to sync from.
fn is_syncing(stage: Option<SyncStage>) -> bool {
    matches!(
        stage,
        Some(SyncStage::Syncing | SyncStage::Slow | SyncStage::NearTip)
    )
}

/// The card, after the chart grid.
#[component]
pub(super) fn ResourcesCard(dash: Dash) -> impl IntoView {
    let state = dash.state;
    let history = move || state.node().resource_history;
    // The node has a run: the samples are live. Otherwise the history's newest sample is the
    // last run's last.
    let live = Memo::new(move |_| state.node().resources.with(Option::is_some));
    let latest = Memo::new(move |_| history().with(|history| history.latest().cloned()));
    let shown = move || latest.with(Option::is_some);
    let summary = Memo::new(move |_| history().with(|history| history.last_run()));
    let machine = state.machine;

    let tone = Signal::derive(move || {
        if live.get() {
            Tone::Normal
        } else {
            Tone::Muted
        }
    });
    let spark = move |kind: SparkKind| {
        Signal::derive(move || {
            let now = state.now.get();
            history().with(|history| history.spark(kind, dash.span(), now))
        })
    };

    // CPU: cores over the last minute, and the run's average once the sync is done.
    let cpu_rate = Memo::new(move |_| {
        let now = state.now.get();
        history().with(|history| history.cpu_cores(RATE_WINDOW, now))
    });
    let cpu_value = while_live(live, move || {
        cpu_rate
            .get()
            .map_or_else(|| "not available".to_owned(), cores)
    });
    let cpu_tone = Signal::derive(move || {
        if live.get() && cpu_rate.get().is_some() {
            Tone::Normal
        } else {
            Tone::Muted
        }
    });
    let cpu_detail = Signal::derive(move || {
        if !live.get() {
            return String::new();
        }
        let syncing = is_syncing(
            state
                .node()
                .insights
                .with(|insights| insights.sync().map(|sync| sync.stage)),
        );
        let cpus = machine.with(|machine| machine.as_ref().map(|machine| machine.logical_cpus));
        let average = summary.get().and_then(|summary| summary.avg_cores);
        cpu_detail(cpu_rate.get(), cpus, syncing, average)
    });

    // Memory: the live processes' memory and the run's peak, against the computer's.
    let memory_value = while_live(live, move || {
        latest
            .get()
            .map_or_else(|| "—".to_owned(), |sample| mem_bytes(sample.memory_bytes))
    });
    let memory_detail = Signal::derive(move || {
        if !live.get() {
            return String::new();
        }
        let total = machine.with(|machine| machine.as_ref().map(|m| m.memory_total_bytes));
        latest.get().map_or_else(String::new, |sample| {
            memory_detail(
                sample.memory_bytes,
                total,
                sample.peak_memory_bytes,
                sample.committed_bytes,
            )
        })
    });

    // Disk or I/O: the read and write rates over the last minute and the run's totals. The
    // sparkline plots the write rate, the larger of the two while zebrad syncs.
    let io_rates = Memo::new(move |_| {
        let now = state.now.get();
        history().with(|history| history.io_rates(RATE_WINDOW, now))
    });
    let io_value = while_live(live, move || {
        let (read, written) = io_rates.get();
        io_value(read, written)
    });
    let io_detail = Signal::derive(move || {
        if !live.get() {
            return String::new();
        }
        latest.get().map_or_else(String::new, |sample| {
            io_detail(sample.read_bytes, sample.written_bytes, sample.platform)
        })
    });

    // Processes: the tree's processes and threads, and its open files or handles.
    let processes_value = while_live(live, move || {
        latest.get().map_or_else(
            || "—".to_owned(),
            |sample| processes_value(sample.processes, sample.threads),
        )
    });
    let processes_detail = Signal::derive(move || {
        if !live.get() {
            return String::new();
        }
        latest.get().map_or_else(String::new, |sample| {
            descriptors_detail(sample.descriptors, sample.platform)
        })
    });

    let rows = Memo::new(move |_| {
        history().with(|history| {
            history
                .latest()
                .map(|sample| process_rows(sample, &history.process_rates()))
                .unwrap_or_default()
        })
    });
    let more = Memo::new(move |_| latest.with(|sample| sample.as_ref().and_then(more_text)));

    view! {
        <Show when=shown>
            {move || {
                // The platform never changes while the launcher runs.
                let platform = latest
                    .with_untracked(|sample| sample.as_ref().map(|sample| sample.platform))
                    .unwrap_or(Platform::Linux);
                let root = state
                    .node()
                    .info
                    .with_untracked(|info| info.kind.program().map(str::to_owned))
                    .or_else(|| {
                        latest.with_untracked(|sample| {
                            sample
                                .as_ref()
                                .and_then(|sample| sample.members.first())
                                .map(|member| member.name.clone())
                        })
                    })
                    .unwrap_or_else(|| "The node".to_owned());
                let threads = platform != Platform::Windows;
                let columns = if threads { "8" } else { "7" };
                let process_row = move |row: ProcessRow| {
                    let started = hhmm(row.started_at, tz_offset_min_at(row.started_at));
                    view! {
                        <tr>
                            <td>
                                <span
                                    class="dash-resources-name"
                                    style:padding-left=format!("{}em", row.depth as f64 * 1.2)
                                >
                                    {row.name}
                                </span>
                            </td>
                            <td class="dash-resources-wide">{row.pid}</td>
                            <td>{row.cpu}</td>
                            <td>{row.memory}</td>
                            <td class="dash-resources-wide">{row.read}</td>
                            <td class="dash-resources-wide">{row.written}</td>
                            {threads.then(|| view! { <td class="dash-resources-wide">{row.threads.clone()}</td> })}
                            <td class="dash-resources-wide">{started}</td>
                        </tr>
                    }
                };
                view! {
                    <section class="card dash-resources" aria-labelledby="dash-resources-title">
                        <div class="dash-resources-head">
                            <h3 id="dash-resources-title">"Resources"</h3>
                            <p class="chart-subtitle">{subtitle(&root, platform)}</p>
                        </div>
                        <p class="chart-subtitle dash-resources-basis">{basis_line(platform)}</p>
                        <div class="tile-row dash-resources-tiles" role="group" aria-label="Resources of this node">
                            <StatTile
                                label="CPU"
                                value=cpu_value
                                detail=cpu_detail
                                spark=spark(SparkKind::Cpu)
                                tone=cpu_tone
                            />
                            <StatTile
                                label="Memory"
                                value=memory_value
                                detail=memory_detail
                                spark=spark(SparkKind::Memory)
                                tone=tone
                            />
                            <StatTile
                                label=platform.io_label()
                                value=io_value
                                detail=io_detail
                                spark=spark(SparkKind::Written)
                                tone=tone
                            />
                            <StatTile
                                label="Processes"
                                value=processes_value
                                detail=processes_detail
                                tone=tone
                            />
                        </div>
                        <Show
                            when=move || live.get()
                            fallback=move || {
                                view! {
                                    <p class="muted dash-resources-line">
                                        {move || summary.get().map(|summary| last_run_text(&summary))}
                                    </p>
                                }
                            }
                        >
                            <div class="chart-table-wrap dash-resources-processes">
                                <table class="chart-table">
                                    <caption>"Processes of this node"</caption>
                                    <thead>
                                        <tr>
                                            <th scope="col">"Process"</th>
                                            <th scope="col" class="dash-resources-wide">"PID"</th>
                                            <th scope="col" title="Cores over the last minute">"CPU"</th>
                                            <th scope="col">"Memory"</th>
                                            <th scope="col" class="dash-resources-wide" title="Over the last minute">"Read"</th>
                                            <th scope="col" class="dash-resources-wide" title="Over the last minute">"Written"</th>
                                            {threads.then(|| view! { <th scope="col" class="dash-resources-wide">"Threads"</th> })}
                                            <th scope="col" class="dash-resources-wide">"Started"</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        {move || rows.get().into_iter().map(process_row).collect_view()}
                                        {move || {
                                            more.get().map(|text| {
                                                view! {
                                                    <tr>
                                                        <td class="muted" colspan=columns>{text}</td>
                                                    </tr>
                                                }
                                            })
                                        }}
                                    </tbody>
                                </table>
                            </div>
                        </Show>
                        {move || {
                            machine.with(|machine| {
                                machine.as_ref().map(|machine| {
                                    view! { <p class="muted dash-resources-line">{machine_text(machine)}</p> }
                                })
                            })
                        }}
                    </section>
                }
            }}
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use launcher_types::{MemoryPressure, ProcessUsage};

    use super::*;

    const GIB: u64 = 1 << 30;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn titles_say_what_is_measured() {
        assert_eq!(
            subtitle("zebrad", Platform::Linux),
            "zebrad and the processes it started · every 5 s"
        );
        assert_eq!(
            subtitle("zebrad", Platform::Windows),
            "zebrad and every process it started · every 5 s"
        );
        assert_eq!(
            basis_line(Platform::Linux),
            "Resident memory (RSS) · disk bytes at the storage layer"
        );
        assert_eq!(
            basis_line(Platform::Macos),
            "Memory as Activity Monitor shows it"
        );
        assert_eq!(
            basis_line(Platform::Windows),
            "Working set · I/O counts files, network and devices"
        );
    }

    #[test]
    fn tile_sentences() {
        assert_eq!(
            cpu_detail(Some(5.6), Some(8), true, Some(2.1)),
            "70 % of 8 CPUs · syncing uses every CPU"
        );
        assert_eq!(
            cpu_detail(Some(0.25), Some(8), false, Some(2.1)),
            "3 % of 8 CPUs · 2.1 on average this run"
        );
        // zebrad still says it syncs without peers, while its tree is nearly idle.
        assert_eq!(
            cpu_detail(Some(0.1), Some(8), true, Some(0.2)),
            "1 % of 8 CPUs · 0.2 on average this run"
        );
        assert_eq!(
            cpu_detail(Some(4.0), Some(8), true, Some(2.1)),
            "50 % of 8 CPUs · syncing uses every CPU"
        );
        assert_eq!(cpu_detail(None, Some(8), false, None), "");
        assert_eq!(
            cpu_detail(Some(1.0), None, false, Some(0.05)),
            "0.05 on average this run"
        );
        assert_eq!(
            memory_detail(6_549_000_000, Some(16 * GIB), 6_871_947_673, None),
            "38 % of 16 GiB · peak 6.4 GiB"
        );
        assert_eq!(
            memory_detail(6_549_000_000, None, 6_871_947_673, Some(7_516_192_768)),
            "peak 6.4 GiB · 7 GiB committed"
        );
        assert_eq!(
            io_value(Some(35_000_000.0), Some(95_000_000.0)),
            "35.0 MB/s read\n95.0 MB/s written"
        );
        assert_eq!(io_value(None, Some(1_000.0)), "— read\n1.0 kB/s written");
        assert_eq!(io_value(None, None), "—");
        assert_eq!(
            io_detail(Some(12_300_000_000), Some(40_100_000_000), Platform::Linux),
            "12.3 GB read, 40.1 GB written this run"
        );
        assert_eq!(
            io_detail(
                Some(12_300_000_000),
                Some(40_100_000_000),
                Platform::Windows
            ),
            "12.3 GB read, 40.1 GB written this run · files, network and devices"
        );
        assert_eq!(
            io_detail(None, None, Platform::Linux),
            "the system does not report them"
        );
        assert_eq!(processes_value(1, Some(68)), "1 process\n68 threads");
        assert_eq!(processes_value(2, None), "2 processes");
        assert_eq!(processes_value(1, Some(1)), "1 process\n1 thread");
        assert_eq!(
            descriptors_detail(Some(412), Platform::Linux),
            "412 open files"
        );
        assert_eq!(
            descriptors_detail(Some(1204), Platform::Windows),
            "1,204 handles"
        );
        assert_eq!(descriptors_detail(None, Platform::Linux), "");
    }

    #[test]
    fn the_last_run_in_one_line() {
        let summary = RunSummary {
            run: 2,
            started_at: at(0),
            ended_at: at(11_520),
            length: Duration::from_secs(11_520),
            avg_cores: Some(2.1),
            peak: 7_301_444_403,
            read: Some(1_234_500_000_000),
            written: Some(340_200_000_000),
        };
        assert_eq!(
            last_run_text(&summary),
            "last run: average 2.1 cores · peak 6.8 GiB · read 1,234.5 GB, wrote 340.2 GB · 3 h 12 min"
        );
        let blind = RunSummary {
            avg_cores: None,
            read: None,
            written: None,
            length: Duration::from_secs(40),
            ..summary
        };
        assert_eq!(last_run_text(&blind), "last run: peak 6.8 GiB · 40 s");
    }

    #[test]
    fn the_computer_in_one_line() {
        let machine = MachineResources {
            at: at(0),
            logical_cpus: 8,
            cpu_busy_cores: Some(6.5),
            memory_total_bytes: 16 * GIB,
            memory_available_bytes: Some(5_046_586_573),
            swap_total_bytes: Some(2 * GIB),
            swap_used_bytes: Some(322_122_547),
            commit_bytes: None,
            commit_limit_bytes: None,
            pressure: None,
        };
        assert_eq!(
            machine_text(&machine),
            "This computer: 8 CPUs, 81 % busy · 16 GiB, 4.7 GiB available · swap 0.3 of 2 GiB"
        );
        let first = MachineResources {
            cpu_busy_cores: None,
            swap_total_bytes: Some(0),
            swap_used_bytes: Some(0),
            ..machine.clone()
        };
        assert_eq!(
            machine_text(&first),
            "This computer: 8 CPUs · 16 GiB, 4.7 GiB available · no swap"
        );
        let windows = MachineResources {
            swap_total_bytes: None,
            swap_used_bytes: None,
            commit_bytes: Some(15_139_759_309),
            commit_limit_bytes: Some(24 * GIB),
            ..machine.clone()
        };
        assert_eq!(
            machine_text(&windows),
            "This computer: 8 CPUs, 81 % busy · 16 GiB, 4.7 GiB available · committed 14.1 of 24 GiB"
        );
        let mac = MachineResources {
            pressure: Some(MemoryPressure::Normal),
            ..machine
        };
        assert!(
            machine_text(&mac).ends_with(" · swap 0.3 of 2 GiB · memory pressure normal"),
            "{}",
            machine_text(&mac)
        );
    }

    fn member(pid: u32, parent: Option<u32>, name: &str) -> ProcessUsage {
        ProcessUsage {
            pid,
            parent,
            name: name.into(),
            started_at: at(1000 + u64::from(pid)),
            cpu_secs: 1.0,
            memory_bytes: Some(40 << 20),
            read_bytes: None,
            written_bytes: Some(0),
            threads: Some(2),
        }
    }

    #[test]
    fn process_rows_follow_the_tree() {
        let sample = NodeResources {
            run: 1,
            at: at(2000),
            run_secs: 1000.0,
            platform: Platform::Linux,
            cpu_secs: 10.0,
            cpu_cores: Some(1.0),
            memory_bytes: 0,
            peak_memory_bytes: 0,
            committed_bytes: None,
            read_bytes: None,
            written_bytes: None,
            processes: 16,
            threads: None,
            descriptors: None,
            members: vec![
                member(10, None, "python3"),
                member(11, Some(10), "python3"),
                member(12, Some(11), "zebend-p2p"),
                member(13, Some(10), "python3"),
                // Its parent is not in the tree: it starts a line of its own.
                member(20, Some(1), "orphan"),
            ],
        };
        let rates = [ProcessRate {
            pid: 11,
            cores: Some(3.27),
            read_per_sec: None,
            written_per_sec: Some(2_500_000.0),
        }];
        let rows = process_rows(&sample, &rates);
        let depths: Vec<(u32, usize)> = rows.iter().map(|row| (row.pid, row.depth)).collect();
        assert_eq!(depths, vec![(10, 0), (11, 1), (12, 2), (13, 1), (20, 0)]);
        assert_eq!(rows[1].cpu, "3.3");
        assert_eq!(rows[1].written, "2.5 MB/s");
        assert_eq!(rows[1].read, "—");
        assert_eq!(rows[0].cpu, "—", "no rate yet");
        assert_eq!(rows[0].memory, "40 MiB");
        assert_eq!(rows[0].threads, "2");
        assert_eq!(more_text(&sample), Some("and 11 more".to_owned()));
        let all = NodeResources {
            processes: 5,
            ..sample
        };
        assert_eq!(more_text(&all), None);
    }

    #[test]
    fn syncing_is_what_zebrad_says() {
        assert!(is_syncing(Some(SyncStage::Syncing)));
        assert!(is_syncing(Some(SyncStage::NearTip)));
        assert!(!is_syncing(Some(SyncStage::AtTip)));
        assert!(!is_syncing(Some(SyncStage::Stalled)));
        assert!(!is_syncing(None));
    }
}
