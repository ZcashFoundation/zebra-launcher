# Resource monitoring

> **Status, 2026-11-03: phase 1 is built (the Resources card against the mock); phases 2
> to 7 are the design.** [Phases](#phases) says what each built phase changed from this
> design. It answers the maintainer's question "can we also monitor resource usage of each
> node or child process?". [ARCHITECTURE.md](ARCHITECTURE.md) stays the description of what
> is built and is updated phase by phase as this lands. [MULTI-NODE.md](MULTI-NODE.md) is
> the plan it fits into.

Yes, for each node and for every process a node starts. Zeal already draws the boundary of
each node's process tree:

- **Windows:** each zebrad runs in its own job object. It joins the job while suspended,
  and its descendants cannot leave it (`process/windows.rs`, `attach_job`).
- **Unix:** zebrad leads its own process group (`process/unix.rs:16`,
  `cmd.process_group(0)`).

Nothing reads the OS's counters for that tree today. zebrad 7.0.0 exports no process
metrics, which is why the Grafana overview's CPU, memory and file-descriptor panels are
empty ([observability/README.md](observability/README.md)).

This design measures each tree with small per-OS readers in `launcher-core`. It reads every
5 s from the spawn until the supervisor finalizes the run, and a second sampler reads the
computer's totals. It adds no crate. The UI gains:

- a Resources card on each node's page;
- a resources table and sparklines on the overview;
- a one-line summary of the computer.

Disk and memory become causes in the health verdict: a disk that is nearly full or too
small for the chain, and nearly exhausted memory. CPU and I/O never raise a verdict.

[Alternatives considered](#alternatives-considered) compares this approach with building on
the sysinfo crate. In short:

- **Zeal's own readers and the job's accounting** report the figures the OS's own monitors
  show, and keep the CPU time of children that have already exited. sysinfo can do neither.
- **A quiet UI:** few verdicts, readable chart units, a mock flag per OS.
- **One sampler per node.**

## Goals and non-goals

Goals:

- **Each node's whole process tree.** Show what it uses, from its spawn to its exit: CPU,
  memory, disk I/O, processes, threads and open files. Include the children a wrapper
  starts and the children that have already exited.
- **The computer beside it.** Show CPU, memory, swap or commit, and the free space on each
  data volume. That makes each node's share clear and shows what other programs use.
- **The OS's own figures.** Report what top and htop, Activity Monitor and Task Manager
  show, and say which figure it is where the OSes differ.
- **Causes, not alarms.** Turn the resource facts that stop nodes (disk and memory) into
  causes in the verdict and into explanations of crashes. Stay quiet while a node syncs.
- **Cheap.** A few system calls per process every 5 s, so measuring never slows a syncing
  node.
- **House rules.**
  - No mutexes.
  - A thin Tauri layer over testable crates.
  - No new crates.
  - `unsafe` only around OS calls.
  - Everything demoable in the mock and tested natively.

Non-goals:

- **Per-process network traffic.** No OS gives it to an unprivileged program:
  - Linux would need `sock_diag` on every live socket.
  - macOS has only the private NetworkStatistics framework (unconfirmed).
  - Windows' `SetPerTcpConnectionEStats` needs Administrators.

  zebrad's own `zcash_net_{in,out}_bytes_total` counters (P2P message bytes) stay the node
  card's Network row.
- **Other figures:** GPU, power, temperature and per-file I/O. Also PSS and USS:
  `smaps_rollup` is slow and takes the target's memory-map lock.
- **Limits and priorities:** no job limits, cgroups or nice. This design never changes how
  a node is started or stopped.
- **Zeal itself and its webviews.** They count under "Other programs" (open question 2).
- **History across launcher restarts.** Histories stay in the UI's memory for 4 h, like
  statistics and metrics (open question 1).

## What is measured

Counters (CPU time, bytes read and written) are cumulative since the spawn and are read
against a monotonic clock. Rates come from their growth, so a missed sample loses no time.
Gauges (memory, counts, free space) are the value at the sample.

### CPU

| | |
| --- | --- |
| Meaning | User plus system CPU time of every process in the tree since the spawn, including processes that have exited. Its growth over wall time gives the rate. |
| Unit | Seconds (`f64`). The rate is in cores: 1.0 is one logical CPU fully busy. The UI also shows the rate as a share of the computer (cores ÷ logical CPUs). |
| Linux | From `/proc/<pid>/stat` for each member: `utime` + `stime` (fields 14–15) and `cutime` + `cstime` (fields 16–17, the children it waited for). Ticks ÷ `sysconf(CLK_TCK)` (USER_HZ, 100 on the usual kernels). |
| macOS | From `proc_pid_rusage(RUSAGE_INFO_V4)` for each member: `ri_user_time` + `ri_system_time` and `ri_child_user_time` + `ri_child_system_time`. Mach ticks × `mach_timebase_info` (125/3 on Apple silicon, 1/1 on Intel). |
| Windows | The job's `TotalUserTime` + `TotalKernelTime` from `QueryInformationJobObject(JobObjectBasicAndIoAccountingInformation)`, in 100 ns units. They include "all terminated processes no longer associated with the job". For each row of the process table: `GetProcessTimes`. |
| Cost | Linux: one 300-byte read per member, plus the tree scan. macOS: one call per member. Windows: one job query, plus one call per member. |
| Caveats | Linux has 10 ms resolution. On Unix, a member that is orphaned and then reaped outside the tree loses at most its last 5 s. On Linux, a child reaped automatically (its parent ignores `SIGCHLD`) is never folded into its parent and loses at most its last 5 s. On macOS XNU still adds it to the parent's `ri_child_*`, twice in the current source, so there it can be over-counted. No tree this design names ignores `SIGCHLD`: zebrad reaps with tokio, and Zebend's supervisor with `Popen.poll`. Under Rosetta (the x86-64 build on Apple silicon) the timebase is unconfirmed, so CPU reads "not available" there. A run's first rate is the average since the spawn. |

### Memory

| | |
| --- | --- |
| Meaning | Memory of the tree's live processes, summed, as the OS's own monitor shows it, and the highest it reached this run. |
| Unit | Bytes, shown in binary units (GiB, MiB) as the OS tools count. |
| Linux | `VmRSS` and `VmHWM` (the peak) from `/proc/<pid>/status`, in kB. This is htop's RES. |
| macOS | `ri_phys_footprint` and `ri_lifetime_max_phys_footprint` from the same `proc_pid_rusage` call. This is Activity Monitor's Memory column. |
| Windows | `WorkingSetSize` and `PeakWorkingSetSize` from `K32GetProcessMemoryInfo(PROCESS_MEMORY_COUNTERS_EX)`, plus `PrivateUsage` (commit charge) shown as "committed". |
| Cost | Linux: one read per member. macOS: no extra call. Windows: one call per member. |
| Caveats | A sum counts each shared page once per process. The tree's peak is exact for one process and a lower bound for several. proc_pid_statm(5) says the RSS values are "inaccurate because of a kernel-internal scalability optimization" (proc_pid_status(5) refers there for VmRSS and VmHWM); at zebrad's size that is negligible. Task Manager's default column, the private working set, reads a little lower than the working set. Its source, `PROCESS_MEMORY_COUNTERS_EX2`, needs Windows 10 or 11 22H2 with the September 2023 update and has no peak. zebrad's "State memory" (memtables plus block cache) is a lower bound for this figure and is shown beside it as a cross-check. |

### Disk I/O

| | |
| --- | --- |
| Meaning | Bytes the tree read from and wrote to storage since the spawn. |
| Unit | Bytes. Rates are shown in decimal MB/s, like the other disk figures. |
| Linux | `read_bytes` and `write_bytes` from `/proc/<pid>/io`. These count the storage layer. `rchar` and `wchar` are not used: they count every read and write call, sockets and pipes included. A process's counts include the children it waited for (`kernel/exit.c` adds a reaped child's counts to its parent). |
| macOS | `ri_diskio_bytesread` and `ri_diskio_byteswritten` from the same call. Reaped children are not rolled up: XNU's child rollup holds only times, wakeups and pageins. |
| Windows | The job's `IoInfo.ReadTransferCount` and `WriteTransferCount`. They cover every process ever in the job, but they count all I/O (files, network and devices), so Zeal labels them "I/O", not "Disk". For each process: `GetProcessIoCounters`. |
| Cost | Linux: one read per member. macOS and Windows: no extra call. |
| Caveats | Linux allows the read only for the same user's dumpable processes (`PTRACE_MODE_READ_FSCREDS`); otherwise the field is absent. Linux counts a write when the page is dirtied, so a file written and then deleted still counts. On macOS an exiting member keeps the bytes last seen, so a child that lives less than one interval is missed. Windows figures cannot be compared with the other two. |

### Processes, threads and open files

| | |
| --- | --- |
| Meaning | The tree's live processes, their threads, and their open files (Unix) or handles (Windows). |
| Unit | Counts. |
| Linux | The members come from the tree scan. Threads: `stat` field 20. Open files: the number of entries in `/proc/<pid>/fd`. |
| macOS | `proc_pidinfo(PROC_PIDTASKALLINFO)` gives `pti_threadnum`, and also the name, parent, process group and start time. Open files: the bytes `proc_pidinfo(PROC_PIDLISTFDS)` fills ÷ `size_of::<proc_fdinfo>()`. |
| Windows | The members come from the job's process list. Threads are not read, because that needs a machine-wide Toolhelp snapshot. Handles: `GetProcessHandleCount`. |
| Cost | Linux: one directory listing per member (zebrad holds a few hundred files). macOS and Windows: one call per member. |
| Caveats | Shown only, never used by a rule. zebrad manages its own file limit: it raises it to 1024 and gives RocksDB (limit − 48) / 2. |

### The computer's CPU and memory

| | |
| --- | --- |
| Meaning | CPU busy across the computer over the last interval. Total memory, memory available without swapping, swap (Linux, macOS) or commit (Windows), and macOS's memory pressure. |
| Unit | Busy logical CPUs (cores) and the CPU count; bytes; a pressure level. |
| Linux | CPU: the first line of `/proc/stat` (busy = user + nice + system + irq + softirq + steal; idle = idle + iowait). CPU count: `sysconf(_NPROCESSORS_ONLN)`. Memory: MemTotal, MemAvailable, SwapTotal and SwapFree from `/proc/meminfo`. |
| macOS | CPU: `host_statistics(HOST_CPU_LOAD_INFO)`. CPU count: `sysconf(_SC_NPROCESSORS_ONLN)` through libc; nix 0.31.3 defines `SysconfVar::_NPROCESSORS_ONLN` only for Linux and Android. Memory: `hw.memsize` and `vm.swapusage`. Pressure: `kern.memorystatus_vm_pressure_level` (1 normal, 2 warning, 4 critical), which XNU lets any process read on macOS. Available memory is estimated from `host_statistics64(HOST_VM_INFO64)` as total − (internal − purgeable + wired + compressor) pages. |
| Windows | CPU: `GetSystemTimes` (busy = kernel − idle + user). CPU count: `GetActiveProcessorCount(ALL_PROCESSOR_GROUPS)`. Memory: `K32GetPerformanceInfo` gives PhysicalTotal, PhysicalAvailable, CommitTotal and CommitLimit, in pages × PageSize. |
| Cost | One read, or two calls, per tick. |
| Caveats | `std::thread::available_parallelism` is not used: it follows cgroup quotas and CPU affinity, and on Windows it can undercount above 64 CPUs. `GlobalMemoryStatusEx` is not used for commit: its page-file fields are "the system or the current process, whichever is smaller". The macOS estimate is assumed to match Activity Monitor's "Memory Used" (unconfirmed), so verdicts there use the pressure level instead. |

### Free space on data volumes

| | |
| --- | --- |
| Meaning | Space available to this user on each volume that holds a node's data folder, the volume's size, and which nodes are on it. |
| Unit | Bytes, shown with `format::bytes` (decimal) as in the first run. |
| Linux | `statvfs` through nix, as `firstrun::free_space` does today: `f_bavail × f_frsize` and `f_blocks × f_frsize`. Volumes are grouped by `MetadataExt::dev()`. A volume's label is the folder's topmost ancestor on the same device. |
| macOS | The same as Linux. The label of the data volume can read `/Users`. |
| Windows | `GetDiskFreeSpaceExW` with its total out-parameter, which `firstrun.rs` passes as null today. Volumes are grouped and labelled by `GetVolumePathNameW` (for example `D:\`). |
| Cost | One call per volume every 30 s, on its own blocking task, because `statvfs` on a dead network mount can hang. |
| Caveats | As today, the nearest existing ancestor is measured, so a data folder that does not exist yet still works. |

### Chain data size

| | |
| --- | --- |
| Meaning | How much space the node's chain data takes. |
| Unit | Bytes, decimal. |
| Linux, macOS, Windows | zebrad: its own gauge, `zebra_state_rocksdb_total_disk_size_bytes`, which the Disk tile already shows. Other kinds (phase 7): `firstrun::dir_size` capped at `SIZE_WALK_LIMIT` (50,000 entries), shown as "at least" when the cap is hit. |
| Cost | zebrad: none (it is on the metrics page). Other kinds: one walk in `spawn_blocking`, after a stop and at most every 15 minutes while running, cached per node with its time. |
| Caveats | zebrad refreshes its gauge every 30 s. It counts SST files only (no WAL, MANIFEST, logs or non-finalized-state backup), so it reads a little below the folder's size. A running zebrad's folder is never walked: a Mainnet state holds about 16,000 SST files (estimate). |

## Process trees

A node's tree is the process Zeal started and everything that process starts. The process
Zeal started is zebrad or a wrapper, such as Zebend's Python supervisor. How the members
are found depends on the OS.

### Windows: the job

`attach_job` (`process/windows.rs:165`) puts the suspended child in a new job before the
child runs. The job allows no breakaway, so every descendant is in it. This includes
processes in jobs that a wrapper creates under it ([nested jobs][nested-jobs]):

- The process list "consists of all processes associated with the job and its child jobs".
- The accounting "describes the usage of every process associated with that job, including
  processes in child jobs".

Each tick:

1. **List the members** with `QueryInformationJobObject(JobObjectBasicProcessIdList)`.
   While `NumberOfProcessIdsInList < NumberOfAssignedProcesses`, grow the buffer and retry,
   at most three times. Also grow it when the call fails with `ERROR_MORE_DATA`, which the
   documentation does not mention (unconfirmed).
2. **Check each pid.**
   1. Open it with `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)`.
   2. `IsProcessInJob(h, job)` rejects a pid now used by another process. A pid is not
      reused while a handle to its process is open ([Raymond Chen, 2011][chen-reuse]), so
      the answer holds while the reader keeps the handle.
   3. `WaitForSingleObject(h, 0) == WAIT_TIMEOUT` skips a member that has exited but is
      still listed while handles to it are open. ActiveProcesses is decremented only "when
      the terminated process exits and all references to the process are released".
      [`GetProcessTimes`][getprocesstimes] cannot make this check: its exit time "is
      undefined" while the process runs.
   4. Then read the creation time (the identity), memory, I/O and handle count. Read the
      image name with `QueryFullProcessImageNameW` through the same handle. That is one
      more call per member each tick, and the reader keeps nothing between ticks.
3. **Totals.** CPU and I/O come from `JobObjectBasicAndIoAccountingInformation`. They are
   exact and include processes that have exited. Memory is the sum over live members.

Parent pids are not used on Windows: Toolhelp's can be stale ([Raymond Chen,
2015][chen-ppid]), and the job's list is authoritative.

The job handle never leaves `process/windows.rs`. The probe carries a `Weak<Job>` that is
upgraded only for one read, so the sampler can never hold the job open after its run.
Holding it open would delay `KILL_ON_JOB_CLOSE`; a `DuplicateHandle` copy would delay it by
up to one tick.

### Linux: the group, parent links and sticky members

zebrad leads its own process group, but children can leave the group. zebrad's own zcashd
sidecar, when it is enabled, takes a group of its own, as does each run of its
`[notify] block_notify_command`. Zebend's services start with `start_new_session=True`. So
members are found in three ways.

1. **Scan.** Read `/proc` and each numeric entry's `stat`: the pid, ppid (field 4), pgrp
   (field 5) and starttime (field 22). Parse after the last `)`, because `comm` can contain
   spaces and parentheses.
2. **Select.** A process is a member when it is:
   - the root;
   - in the root's process group (its pgrp is the root's pid);
   - a child of a member that started no earlier than that member, which rejects a stale
     parent link to a reused pid;
   - a sticky member: a process seen as a member before and still alive with the same
     start time. It stays a member after it is orphaned and re-parented to init or to a
     subreaper such as `systemd --user`.
3. **Read** each member's `status`, `io` and `fd` directory.

The scan reads each `/proc` file as bytes, so a comm with any bytes (a name cut
mid-character at 15 bytes, a tab, a newline) never fails a read. A member's name comes from
`status`'s `Name:`, with invalid UTF-8 replaced by U+FFFD and control characters replaced,
so a name is always one line of plain text. macOS and Windows names get the same treatment.

The process group is safe from pid reuse: "A process group ID shall not be reused by the
system until the process group lifetime ends" ([POSIX.1-2024][posix-reuse], Base
Definitions §4.17).

The root's identity is checked once, at the run's first reading: btime +
starttime ÷ CLK_TCK must be within 5 s of the spawn time. After that the root is known by
(pid, starttime).

**Cost.** The scan reads one `stat` per process on the computer. A Python scan of 146
processes took 0.84 ms. A Rust scan of a few hundred should take under 5 ms (unconfirmed;
phase 2 logs each tick's time). If hosts with thousands of processes prove slow, the
fallback is a full scan every 30 s and reads of known members in between.

`/proc/<pid>/task/<tid>/children` is not used: proc_tid_children(5) says it is reliable
only when the children are frozen.

**What escapes.** A child that leaves the group and is orphaned before it is first seen
(within one 5 s interval) is never found. Today's `kill_group` misses it too. A cgroup
would close that hole, but it would depend on how each desktop delegates cgroups. That is
out of scope.

### macOS: the same rules through libproc

- **Members:**
  - `proc_listpgrppids(root)` lists the group.
  - A breadth-first walk of `proc_listchildpids` from each member finds children that left
    the group.
  - Sticky members are kept as on Linux.
- **List buffers.** Both calls return a count of pids, while the buffer size is given in
  bytes (`libproc.c`). Grow the buffer and repeat the call while the count fills it.
- **Per member:**
  - `proc_pidinfo(PROC_PIDTASKALLINFO)` gives the parent, group, name, threads and start
    time. The start time (`pbi_start_tvsec`, `pbi_start_tvusec`) is the identity.
  - On a zombie, `PROC_PIDTASKALLINFO` fails with ESRCH. The reader then calls
    `proc_pidinfo(PROC_PIDTBSDINFO)` with `arg` 1, which XNU answers for zombies:
    `pbi_status` is `SZOMB`, and `pbi_ppid` and the same start time give the parent and the
    identity.
  - `proc_pid_rusage(RUSAGE_INFO_V4)` gives the times, memory and I/O. It also works on a
    zombie.
- **The root** gets the same identity check as on Linux.

### Summing a tree across exits (Linux and macOS)

When a parent reaps a child, the kernel moves the child's whole CPU time into the parent's
rollup fields. On Linux it moves the child's I/O counts too. That moment is also when the
child stops being a live member. So the tree's total stays continuous if each live member
counts its own time plus its rollup:

```text
total(t) = Σ over live members (own + rollup) + lost(t)
```

- **A member whose parent is a live member disappears.** That parent reaped it, so its time
  is now in the parent's rollup. The scan is not atomic: the parent's `stat` may have been
  read just before the reap. So the departed member stays pending for one more tick. It
  moves to `lost` only if the parent's rollup still has not grown by its total. On Linux
  that happens when the child was reaped automatically because its parent ignores
  `SIGCHLD`; a child reaped that way is never folded into its parent.
- **A member with no live parent in the tree disappears.** This is the root, reaped by
  Zeal, or an orphan reaped by init. Its last-seen total moves to `lost`, and at most its
  last 5 s are missed.
- **macOS I/O.** macOS does not roll up I/O, so every departed member moves its last-seen
  bytes to `lost`.
- **Never decreasing.** The published total is `max(previous, total(t))`, so it never falls
  within a run. This hides the one-tick dip of a torn read.
- **Zombies** count as live until they are reaped. On Linux their `stat` keeps their final
  times. On macOS the list calls still return a zombie, but
  `proc_pidinfo(PROC_PIDTASKALLINFO)` fails on it with ESRCH. The reader then calls
  `proc_pidinfo(PROC_PIDTBSDINFO)` with `arg` 1, which XNU answers for zombies:
  `pbi_status` is `SZOMB`, and `pbi_ppid` and `pbi_start_tvsec`/`pbi_start_tvusec` give its
  parent and identity. Its final times come from `proc_pid_rusage`.

Example: a child uses 10 s before sample 1 and 1 s after it, and then its parent reaps it.
At sample 2 the child is gone and the parent's `cutime` has grown by 11 s, so the total
grows by 1 s, which is right. The rule in the first research note (sum each process's
growth, then add the growth of the rollups) gives 11 s.

Memory is the sum over live members. The peak is the largest of the previous peak, the sum
now and the largest member's own OS peak.

Windows needs none of this: the job keeps the totals.

### What the tree holds for each kind

| Kind | Tree |
| --- | --- |
| zebrad (bundled) | One process, unless `[notify] block_notify_command` is set, which Configure offers and `validate` allows. zebrad 7.0.0 then runs the command through `/bin/sh -c` (`cmd /C` on Windows) at each best-tip change once it is near the tip, and reaps it (`zebrad/src/components/notify.rs`). On Unix each run has a process group of its own. A run alive at a sample is found by its parent link on Unix and by the job on Windows. A shorter run is never listed; its CPU still counts, through zebrad's rollup on Unix and in the job's totals on Windows. Zeal forces `[zcashd_compat]` off (`config.rs`), so zebrad starts no zcashd. |
| Zebend (MULTI-NODE phase 4) | `python3 -u node/ops/supervise.py run …` → one Python process per enabled service (`sync`, `rpc`, `mempool` and `follow` in Zebend's own `zebend.json`; `live` is disabled), each started as `{python} -u <script>.py` in a new session, so the OS names each after the interpreter. They are found by their parent link and then kept as sticky members. The services run helper programs: `follow` runs Bend binaries from `node/follow/bin` (among them `zebend-p2p`, and `nfb` through `tools/heavy.sh`) from a thread pool, and `rpc` runs `blockdelta`. The supervisor runs `ps`, `lsof`, `osascript` and, on macOS, `memory_pressure -Q`. A helper alive at a sample is listed by its parent link; the CPU of the others is counted through the rollup when they are reaped. |
| Zakura, Zaino, Zallet, zcashd build | Expected to be one process each (unconfirmed). The same rules apply. |
| Generic | Whatever the program starts. The same rules apply. |

## Backend

### Module layout (`crates/launcher-core`)

| File | What it holds |
| --- | --- |
| `resources.rs` | `ResourceSampler` (one per node), `MachineSampler` (one per app), the `Reader` and `MachineReader` traits, `INTERVAL` (5 s), `VOLUME_INTERVAL` (30 s). |
| `resources/fold.rs` | Pure code: member selection, identities, the continuity fold, peaks. Tested on literal process tables. |
| `resources/procfs.rs` | Pure parsers of `/proc` files, read as bytes (`stat`, `status`, `io`, `/proc/stat`, `meminfo`). Compiled and tested on every OS. |
| `resources/linux.rs` | The Linux reader: `std::fs` and nix's `sysconf`, no `unsafe`. |
| `resources/macos.rs` | The macOS reader: libproc, `sysctlbyname`, host statistics. |
| `resources/windows.rs` | The Windows reader: calls per process and for the machine. Job queries go through `process::windows::Job`. |
| `process.rs` | `TreeProbe` and `ChildHandle::probe`. On Windows, `ChildHandle.job` becomes `Arc<Job>`. |
| `process/windows.rs` | `Job::accounting`, `Job::process_ids` and `Job::contains`, next to `Job::terminate`. |
| `supervisor.rs`, `supervisor/actor.rs` | `SupervisorHandle::watch_probe`. `install_run` sets the probe and `finalize` clears it. |
| `firstrun.rs` | `volume(path) -> io::Result<Volume>`. `free_space` becomes `volume(path)?.available`. |
| `diagnostics.rs` | `resources_section(node, machine) -> String`, for Copy diagnostics. |

Nothing moves: `ChildHandle`, `Job` and `firstrun` change in place. If the Windows reader
reuses the image-name code in `executable_of`, the extraction lands first, as a pure-move
commit of its own.

### The probe: from the supervisor to the sampler

`Status` cannot carry a handle, so the supervisor publishes a second watch:

- `Actor` gets `probe: watch::Sender<Option<TreeProbe>>`, and
  `SupervisorHandle::watch_probe()` returns a receiver.
- `install_run` (`actor.rs:448`) reads `SystemTime::now()` and `Instant::now()` once. It
  uses them for `Status.started_at` (l.491) and `Run.started_at` (l.485), and sends
  `Some(handle.probe(id, wall, mono))`.
- `finalize` (`actor.rs:657`) sends `None` right after `self.run.take()`.

**What is measured.** A node is measured from its spawn to the end of its exit drain:

- Starting: the database open, and the start-up scan of an `indexer` build (zebra#11090).
- Running and Stopping.
- The exit drain, when lingering descendants show.

This differs on purpose from the RPC monitor and the metrics poller. They wait for
`Phase::Running` and for zebrad's log lines. Resources need neither, so every node kind
gets them.

**Windows.** The strong `Arc<Job>` lives only in `Run.handle` and drops at the end of
`finalize`, as the job does today, so the kill-on-close timing does not change. A read in
flight can delay it by the length of that read (milliseconds), never by a tick.

### The node sampler

```rust
impl ResourceSampler {
    /// Samples the probe's tree every INTERVAL while the probe is `Some`. Must be called
    /// inside a Tokio runtime.
    pub fn spawn(probe: watch::Receiver<Option<TreeProbe>>) -> watch::Receiver<Option<NodeResources>>;
    /// Like `spawn`, with another interval or reader (tests).
    pub fn spawn_with(
        probe: watch::Receiver<Option<TreeProbe>>,
        interval: Duration,
        reader: Arc<dyn Reader>,
    ) -> watch::Receiver<Option<NodeResources>>;
}
```

Its lifecycle follows the metrics poller (`metrics/poller.rs`):

1. While the probe is `None`, the sampler publishes `None` and waits for a change.
2. When the probe becomes `Some` with a new run id, it starts a new `Fold`. It reads at
   once, then every 5 s (`tokio::time::interval`, `MissedTickBehavior::Delay`).
3. Each tick moves three things into `spawn_blocking`: a clone of the probe, the fold's
   sticky identities and the reader (an `Arc`). The blocking task returns an owned
   `TreeReading`. If the probe changed during the read, the reading is dropped, as the
   metrics poller drops a read that spans a status change. Otherwise the actor folds the
   reading and calls `send_replace(Some(sample))`.
4. Failures never end the task:
   - Malformed `/proc` text or an unexpected OS answer is an `Err` from `tree`, never a
     panic. The first one in a run logs one warning, and later ones log at debug, as the
     metrics poller does after `FAILURE_LIMIT`.
   - A reader panic is a bug. It arrives as a `JoinError`, and the launcher's panic hook
     (`logging.rs`) has already logged it as an ERROR, which reaches the node's log and
     rule 14 (errors in the log). So the sampler logs one warning, publishes `None` and
     reads nothing more until the next run.
   - EACCES, EPERM or ERROR_ACCESS_DENIED on a field makes that field `None`, with one
     debug line per node and field.
   - A process that has vanished simply drops out of the tick.
5. The task ends when the probe's sender (the supervisor) or every receiver is dropped.

**Why 5 s:**

- It is the RPC monitor's cadence, so the card moves with the statistics beside it.
- A 10 ms CPU tick is 0.2 % of an interval.
- The cost stays negligible.

### The machine sampler

```rust
pub struct DataDir {
    pub node: NodeId,
    pub path: PathBuf,
}

impl MachineSampler {
    /// The computer's CPU and memory every INTERVAL, and the volumes holding `data_dirs`
    /// every VOLUME_INTERVAL. Must be called inside a Tokio runtime.
    pub fn spawn(data_dirs: watch::Receiver<Vec<DataDir>>) -> watch::Receiver<Option<MachineResources>>;
    pub fn spawn_with(
        data_dirs: watch::Receiver<Vec<DataDir>>,
        interval: Duration,
        volume_interval: Duration,
        reader: Arc<dyn MachineReader>,
    ) -> watch::Receiver<Option<MachineResources>>;
}
```

- **One per app.** Each tick reads the CPU counters and memory in `spawn_blocking`. Busy
  cores are the growth of the counters over the interval.
- **Volumes.** A separate blocking task reads the volumes every 30 s. An in-flight flag
  owned by the actor means a hung network mount delays only its own result. Volumes are
  grouped by device, and each lists the nodes it holds.
- **macOS host port.** The reader takes the host port with `mach_host_self` once and keeps
  it, because each call adds a send right.
- **End.** The task ends when every receiver is dropped.

### Publishing to the UI

| | One node (now) | Several nodes (MULTI-NODE phase 2) |
| --- | --- | --- |
| Event `node_resources` | `Option<NodeResources>` | `NodeResourcesEvent { node, resources }` |
| Event `machine_resources` | `Option<MachineResources>` | unchanged |
| Snapshot command | `get_node_resources()` | `get_node_resources(node)` |
| Snapshot command | `get_machine_resources()` | unchanged |

- **Bridge.** `bridge::forward_node_resources` and `forward_machine_resources` are thin
  wrappers over `forward_watch` (`bridge.rs`). `forward_watch` emits once at start and then
  on every change. That makes one event per node and one machine event every 5 s. A zebrad
  sample is about 0.6 kB, a three-process tree about 1 kB.
- **Command lists.** The commands go into three lists: `COMMANDS` in `build.rs`,
  `generate_handler!` in `lib.rs`, and `capabilities/main.json` (`allow-get-node-resources`,
  `allow-get-machine-resources`). `check_command_lists` fails the build if they disagree.
- **App state.** `AppState` gains `node_resources` and `machine` receivers.
- **UI subscription.** The UI uses `subscribe_then_fetch` (`app.rs`), so an event that
  arrives while the snapshot is in flight wins.
- **Wiring in `start`**, next to `MetricsPoller::spawn` (`lib.rs:266`):

  ```rust
  let node_resources = ResourceSampler::spawn(supervisor.watch_probe());
  let machine = MachineSampler::spawn(data_dirs(settings.watch(), paths.clone()));
  ```

  `data_dirs` is a few lines in src-tauri. It maps the settings watch to
  `[DataDir { node, path: state_dir(..) }]`. `node` is
  `NodeId::suggest(NodeKind::Zebrad, settings.network, &[])`, the id
  `node_state::primary_info` gives the UI's primary node (`zebrad-mainnet` or
  `zebrad-testnet`), so the UI finds the node's volume. It is recomputed on every settings
  change, so it follows a change of network as the UI's does, and `choose_data_dir` needs
  no extra call. A unit test of `data_dirs` checks the id on Mainnet and after a change to
  Testnet.
- **Diagnostics.** `copy_diagnostics` appends `diagnostics::resources_section(..)`.

### Several nodes

- **Samplers.** Each `NodeRuntime` gets one `ResourceSampler`, next to its supervisor, RPC
  monitor and metrics poller. The sampler takes that runtime's probe watch and is dropped
  with it. The app has one `MachineSampler`, which takes the registry's data folders.
- **Summaries.** `NodeSummary`, planned in MULTI-NODE.md, gains `cpu_cores: Option<f64>`
  and `memory_bytes: Option<u64>` for the tray; the overview's card lines and table read
  the UI's histories.
- **Why one sampler per node** rather than one actor for all (where both alternatives started):
  - The sampler's lifecycle is the runtime's, so no add-and-remove protocol is needed.
  - A slow or failing read of one node never delays another.
  - Tests stay per node.

  The cost: on Linux, one `/proc` scan per node per tick, about a millisecond each. The
  samplers' ticks are not aligned; the overview absorbs that by using one-minute averages.

## Types

### launcher-types: `src/resources.rs`

The module is re-exported from `lib.rs` like `nodes.rs`. It holds plain serde types that
build to wasm. Optional fields carry `#[serde(default)]`, as in `NodeDetails`. Function
bodies are left out in this section.

```rust
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::NodeId;

/// The most processes one sample lists; `processes` still counts them all.
pub const MAX_MEMBERS: usize = 32;

/// A node's process tree, read every 5 s from the spawn until the run is finalized or a
/// reader panics (the node sampler, step 4); the `node_resources` event and
/// `get_node_resources` carry it. `None` on the wire while the node has no run, and after a
/// reader panic until the next run. Counters are cumulative since the spawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeResources {
    /// The supervisor's run id: counters restart when it changes.
    pub run: u64,
    pub at: SystemTime,
    /// Monotonic seconds since the spawn: the base of every rate.
    pub run_secs: f64,
    /// What the figures mean (the UI's labels).
    pub platform: Platform,
    /// User plus system CPU of the whole tree, exited processes included. Never decreases
    /// within a run. 0 while `cpu_cores` is `None` for the whole run.
    pub cpu_secs: f64,
    /// Over the last interval (the first sample: since the spawn); 1.0 = one logical CPU.
    /// `None` when CPU cannot be read (an x86-64 build under Rosetta).
    #[serde(default)]
    pub cpu_cores: Option<f64>,
    /// The live members' memory, summed (see `Platform::memory_label`).
    pub memory_bytes: u64,
    /// The highest this run: exact for one process, a lower bound for several.
    pub peak_memory_bytes: u64,
    /// Windows only: commit charge (`PrivateUsage`), summed.
    #[serde(default)]
    pub committed_bytes: Option<u64>,
    /// Since the spawn (see `Platform::io_label`); `None` when the OS refuses the read.
    #[serde(default)]
    pub read_bytes: Option<u64>,
    #[serde(default)]
    pub written_bytes: Option<u64>,
    /// Live processes in the tree, listed or not.
    pub processes: u32,
    /// `None` on Windows.
    #[serde(default)]
    pub threads: Option<u32>,
    /// Open files on Unix, handles on Windows.
    #[serde(default)]
    pub descriptors: Option<u32>,
    /// The process Zeal started first, then the others by start time; at most MAX_MEMBERS.
    pub members: Vec<ProcessUsage>,
}

/// One live process of a node's tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProcessUsage {
    pub pid: u32,
    /// Its parent, when the parent is a member too.
    #[serde(default)]
    pub parent: Option<u32>,
    /// As the OS reports it, cleaned to one line of plain text: `zebrad`, `python3`,
    /// `zebrad.exe`.
    pub name: String,
    pub started_at: SystemTime,
    /// Its own CPU since it started, not its children's.
    pub cpu_secs: f64,
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    #[serde(default)]
    pub read_bytes: Option<u64>,
    #[serde(default)]
    pub written_bytes: Option<u64>,
    #[serde(default)]
    pub threads: Option<u32>,
}

/// The OS the figures come from, so the UI can say what they mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

impl Platform {
    /// "Resident memory (RSS)", "Memory, as Activity Monitor shows it", "Working set".
    pub fn memory_label(self) -> &'static str;
    /// "Disk" on Linux and macOS; "I/O" on Windows, which also counts network and devices.
    pub fn io_label(self) -> &'static str;
    /// "open files" or "handles".
    pub fn descriptors_label(self) -> &'static str;
    /// True on Windows, where the job holds every process the node ever started. On Unix a
    /// process that leaves the group and is orphaned before it is first seen is missed.
    pub fn tree_is_exact(self) -> bool;
}

/// The computer, read every 5 s (the `machine_resources` event, `get_machine_resources`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineResources {
    pub at: SystemTime,
    pub logical_cpus: u32,
    /// Busy over the last interval, in logical CPUs (0 to `logical_cpus`); `None` on the
    /// first sample.
    #[serde(default)]
    pub cpu_busy_cores: Option<f64>,
    pub memory_total_bytes: u64,
    /// Usable without swapping: MemAvailable, PhysicalAvailable, an estimate on macOS.
    #[serde(default)]
    pub memory_available_bytes: Option<u64>,
    /// Linux and macOS.
    #[serde(default)]
    pub swap_total_bytes: Option<u64>,
    #[serde(default)]
    pub swap_used_bytes: Option<u64>,
    /// Windows: CommitTotal and CommitLimit.
    #[serde(default)]
    pub commit_bytes: Option<u64>,
    #[serde(default)]
    pub commit_limit_bytes: Option<u64>,
    /// macOS only.
    #[serde(default)]
    pub pressure: Option<MemoryPressure>,
    /// The volumes that hold a node's data folder, read every 30 s.
    #[serde(default)]
    pub volumes: Vec<VolumeUsage>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeUsage {
    /// For display: a mount point (`/mnt/zcash`) or a volume path (`D:\`).
    pub label: String,
    pub total_bytes: u64,
    /// Available to this user.
    pub free_bytes: u64,
    /// The nodes whose data folder is on it.
    pub nodes: Vec<NodeId>,
    pub measured_at: SystemTime,
}

/// The `node_resources` payload once several nodes run (MULTI-NODE phase 2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeResourcesEvent {
    pub node: NodeId,
    pub resources: Option<NodeResources>,
}
```

In the `events` module:

```rust
pub const NODE_RESOURCES: &str = "node_resources";
pub const MACHINE_RESOURCES: &str = "machine_resources";
```

### launcher-core

```rust
// process.rs
/// What a sampler needs to find a run's process tree. The supervisor publishes it from
/// `install_run` until `finalize`.
#[derive(Clone, Debug)]
pub struct TreeProbe {
    /// The supervisor's run id.
    pub run: u64,
    /// The process Zeal started; on Unix also its process group id.
    pub pid: u32,
    /// Checked against the root's start time at the run's first reading.
    pub spawned_at: SystemTime,
    /// The base of `run_secs`.
    pub spawned: Instant,
    /// Upgraded only for the length of one read.
    #[cfg(windows)]
    pub(crate) job: Weak<windows::Job>,
}

impl ChildHandle {
    /// The only place a probe is made.
    pub fn probe(&self, run: u64, spawned_at: SystemTime, spawned: Instant) -> TreeProbe;
}

// process/windows.rs: the raw job calls stay in this file.
pub struct JobAccounting {
    pub cpu_100ns: u64,
    pub read_bytes: u64,
    pub written_bytes: u64,
    pub total_processes: u32,
}

impl Job {
    /// CPU and I/O of every process ever in the job (JobObjectBasicAndIoAccountingInformation).
    pub fn accounting(&self) -> io::Result<JobAccounting>;
    /// The job's processes, nested jobs included (JobObjectBasicProcessIdList, grown and
    /// retried at most three times).
    pub fn process_ids(&self) -> io::Result<Vec<u32>>;
    /// Whether `process` is in this job (IsProcessInJob).
    pub fn contains(&self, process: BorrowedHandle<'_>) -> io::Result<bool>;
}

// resources.rs
/// A process as the OS has it: the pid and the start time in the OS's unit (Linux ticks
/// since boot, macOS microseconds, Windows 100 ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Identity {
    pub pid: u32,
    pub start: u64,
}

/// One member as a reader saw it: `id`, `ppid`, `name`, `started_at`, `cpu_own_secs`,
/// `cpu_rollup_secs` (Linux cutime + cstime, macOS ri_child_*, 0 on Windows), and the
/// optional memory, peak, committed, read, written, threads and descriptors.
pub struct MemberReading { /* the fields above */ }

/// One tick's reading of a tree, before the fold.
pub struct TreeReading {
    pub at: SystemTime,
    pub mono: Instant,
    pub members: Vec<MemberReading>,
    /// Windows: the job's totals, which replace the fold.
    pub job: Option<JobAccounting>,
}

pub trait Reader: Send + Sync + 'static {
    /// Reads the probe's tree. `sticky` holds the identities seen as members before.
    fn tree(&self, probe: &TreeProbe, sticky: &[Identity]) -> io::Result<TreeReading>;
}

pub trait MachineReader: Send + Sync + 'static {
    fn machine(&self) -> io::Result<MachineReading>; // CPU counters, memory, swap or commit, pressure
    fn volume(&self, dir: &Path) -> io::Result<firstrun::Volume>;
}

// resources/fold.rs (pure)
pub(crate) enum Rollup {
    /// Windows: the job keeps the totals.
    JobTotal,
    /// Linux CPU and I/O, macOS CPU: a reaped child's total moves into its parent.
    IntoParent,
    /// macOS I/O: a departed member keeps what it had when last seen.
    LostOnExit,
}

pub(crate) struct Fold {
    // run id, root identity, last seen per identity, departures pending for one tick,
    // lost CPU and bytes, published totals, peak, the previous (Instant, cpu) for the rate
}

impl Fold {
    pub(crate) fn new(probe: &TreeProbe, cpu: Rollup, io: Rollup) -> Self;
    pub(crate) fn sticky(&self) -> Vec<Identity>;
    pub(crate) fn fold(&mut self, reading: TreeReading) -> NodeResources;
}

// firstrun.rs
pub struct Volume {
    /// The device number on Unix, the volume path on Windows.
    pub key: String,
    pub label: String,
    pub available: u64,
    pub total: u64,
}
```

## UI

### What the UI shows, and what it leaves out

- **Smoothed values.** Tiles and tables show rates over the last minute, the average
  Resource Monitor uses. Charts plot every 5 s point in the range, as the existing charts
  do; the sparklines and the charts' Table view thin to `charts::MAX_POINTS` (60), as today.
- **Stable order.** The process table is in tree order (the root, then children by start
  time) and is never re-sorted by load.
- **Tones.** Only the Memory tile and the existing Disk tile take a tone, and only from the
  verdict's rules (see [Health](#health)). CPU, I/O and process figures never warn.
- **Labels.** The card's subtitle says once what each OS measures, instead of a note on
  every number.
- **No fake zeros.** The card stays hidden until a node has its first sample. A stopped
  node shows muted last-run values, never live-looking zeros.
- **Computer-wide facts appear once:** in the card's footer on one node, in the overview's
  strip with several.
- **Units.**
  - Memory uses binary units with their names: 16 GiB of RAM reads "16 GiB", which is the
    number the OS tools show.
  - Disk figures keep the decimal `format::bytes` ("212.4 GB"), so on one card "GB" never
    means two things.
  - Cores have one decimal ("2.4 cores"), and two below 0.1 ("0.05 cores").
  - Shares follow `format::percent`'s style ("30 %").

### Each node's Resources card

`ui/src/pages/dashboard/resources.rs`, `ResourcesCard(dash: Dash)`. It sits after the chart
grid (`dashboard.rs`, `chart-grid`) and before `<ActivityRow>`, so the dashboard's range
control scopes its charts. Every node kind gets it: it needs neither RPC nor metrics, and it
keeps working while a node's RPC is silent.

```text
  Resources                         zebrad and the processes it started · every 5 s
  Resident memory (RSS) · disk bytes at the storage layer

  +------------------+ +------------------+ +----------------------+ +----------------+
  | CPU              | | Memory           | | Disk                 | | Processes      |
  | 5.6 cores        | | 6.1 GiB          | | 35 MB/s read         | | 1 process      |
  | 70 % of 8 CPUs · | | 38 % of 16 GiB · | | 95 MB/s written      | | 68 threads     |
  | syncing uses     | | peak 6.4 GiB     | | 12.3 GB read,        | | 412 open files |
  | every CPU    ~~~ | |              ~~~ | | 40.1 GB written  ~~~ | |                |
  +------------------+ +------------------+ +----------------------+ +----------------+
  [ CPU: this node / whole computer ] [ Memory: this node / in use ] [ Disk: read / written ]

  Process   PID     CPU    Memory    Read      Written   Threads  Started
  zebrad    4242    5.6    6.1 GiB   35 MB/s   95 MB/s   68       10:42

  This computer: 8 CPUs, 81 % busy · 16 GiB, 4.7 GiB available · swap 0.3 of 2 GiB ·
  212.4 GB free on the data disk
```

- **Subtitle.** "zebrad and every process it started" on Windows, where the job is exact.
  On Unix it reads "zebrad and the processes it started". Other kinds show their root
  process's name. Below it, a basis line from `Platform`:
  - "Resident memory (RSS) · disk bytes at the storage layer";
  - "Memory as Activity Monitor shows it";
  - "Working set · I/O counts files, network and devices".
- **Tiles** (`StatTile` with a `Sparkline`):

  | Tile | Value | Detail |
  | --- | --- | --- |
  | CPU | `5.6 cores`, over the last minute | `70 % of 8 CPUs`, then `syncing uses every CPU` while the node syncs, otherwise `2.1 on average this run`. |
  | Memory | `6.1 GiB` | `38 % of 16 GiB · peak 6.4 GiB`; on Windows also `· 7.0 GiB committed`. |
  | Disk (I/O on Windows) | `35 MB/s read · 95 MB/s written`, over the last minute | `12.3 GB read, 40.1 GB written this run`; on Windows `files, network and devices`. |
  | Processes | `1 process · 68 threads` (Windows: `1 process`) | `412 open files` or `1,204 handles`. |

- **Charts.** Three `LineChart`s on the existing range control. Each starts from zero.

  | Chart | Series | Unit |
  | --- | --- | --- |
  | CPU | "This node" (series 1, area), "Whole computer" (muted) | cores |
  | Memory | "This node", "In use on this computer" (total − available, muted) | GiB |
  | Disk or I/O | "Read" (series 1), "Written" (series 2) | MB/s |

  A restart, or a gap of more than 15 s between two samples, leaves a gap in the line. The empty text
  reads "Measuring… every 5 s"; `chart_empty_text` hard-codes the RPC's wording.
- **Process table** (`chart-table`, captioned "Processes of this node").
  - Columns: Process (indented under its parent), PID, CPU (cores over the last minute),
    Memory, Read and Written (over the last minute), Threads, Started.
  - It is in tree order. zebrad is one row, or two while a block notify command runs; a
    wrapper shows its children. Past `MAX_MEMBERS` it ends with "and 12 more".
  - At narrow width only Process, CPU and Memory remain.
  - It is hidden while the node is stopped.
- **Stopped.**
  - The tiles go muted and show "—" with the last run: "last run: average 2.1 cores · peak
    6.8 GiB · read 1,234.5 GB, wrote 340.2 GB · 3 h 12 min".
  - The charts keep the history, with a gap at the stop.
- **Footer.** The machine summary for a single node, as in the sketch. On Windows it adds
  "committed 14.1 of 24 GiB"; on macOS "memory pressure normal".
- **Elsewhere on the dashboard.**
  - The existing Disk tile (`tiles.rs`) adds "· 212.4 GB free" to its detail, and takes
    its tone from `DISK_RULES`.
  - The node card's "State memory" row adds "(of 6.1 GiB in use)". This is the cross-check
    that zebrad's own figure is a lower bound.
- **Text helpers.** `cpu_detail`, `memory_detail`, `io_value` and `last_run_text` are pure
  and tested natively.

### Chart code: two additive changes

Every existing chart keeps its output byte for byte, because the default unit is `Count`.

1. **A unit.**
   - `charts::Unit` has four values: `Count` (the default, today's labels), `Cores`,
     `Memory` (binary units, ticks on whole GiB or MiB, so the axis reads "2 GiB, 4 GiB"
     and not "2.15B") and `BytesPerSec` (decimal).
   - It has three methods: `ticks(lo, hi)`, `short(v)` for the end label and `full(v)` for
     the tooltip, the table and the aria summary.
   - `geometry` and `summary` take the unit. `LineChart` gets `#[prop(optional)] unit:
     Unit`, and `line_chart::whole` becomes `unit.full`.
2. **Gaps.**
   - `line_path` starts a new subpath after a non-finite value instead of writing "NaN"
     into the SVG, and `area_path` closes each finite run on its own.
   - The end label uses the last finite value, and the tooltip shows "—".
   - This also fixes the NaN paths that any chart could write today.

New format helpers: `format::mem_bytes` ("6.1 GiB"), `format::cores` ("2.4 cores") and
`format::share` ("30 %").

### State and histories (pure Rust, tested natively)

- **`NodeState`** (`node_state.rs`) gains `resources: RwSignal<Option<NodeResources>>`,
  `resource_history: RwSignal<ResourceHistory>` and `record_resources`, which pushes and
  then sets, like `record_metrics`.
- **`AppState`** (`app.rs`) gains `machine: RwSignal<Option<MachineResources>>` and
  `machine_history: RwSignal<MachineHistory>`.
- **Subscriptions.** Two more `subscribe_then_fetch` blocks feed these signals, and
  `subscribe_extra` adds `on_extra_node_resources` for the mock's `?herd` and `?tree`
  nodes.
- **`ui/src/resource_history.rs`:**
  - **Size.** `MAX_SAMPLES = 2880`, which is 4 h of 5 s samples, like
    `stats_history::MAX_SAMPLES`. The last 12 samples (one minute) keep their member
    lists. Older ones shrink to `Point { at, run, run_secs, cpu_secs, memory, peak, read,
    written, processes }`, as `MetricsHistory` keeps only its newest snapshot whole.
  - **Rates.** `cpu_cores(span, now)` is ΣΔcpu ÷ ΣΔrun_secs within runs, and
    `io_rates(span, now)` gives bytes per second.
  - **Charts.** `chart(kind, span, now, &MachineHistory) -> ChartData` puts NaN at a run
    boundary or a gap of more than 15 s. `spark(kind, span, now)` covers the current run
    only.
  - **Table and summary.** `process_rates()` gives cores and bytes per second per
    identity over the kept minute. `last_run()` gives
    `RunSummary { started_at, ended_at, avg_cores, peak, read, written }`.
- **`ui/src/machine_history.rs`** follows the same pattern, with `busy_cores(span, now)`
  and the sustain helpers the rules need: `available_below_for`, `pressure_at_least_for`,
  `commit_above_for`, and an `Episode` type for the leave margins.

### Overview (`ui/src/pages/overview.rs`)

```text
  This computer · CPU 63 % of 16 · Memory 19.4 of 32 GiB in use (12.6 GiB available) · Swap 0.3 GiB
  /             212.4 GB free of 1,000.2 GB     Zebra · Mainnet
  /mnt/zcash  1,834.5 GB free of 4,000.8 GB     Zakura · Mainnet, Zaino · Mainnet, zcashd · Mainnet
                                                and 2 Testnet nodes

  Mainnet · 4 nodes · 3 running · 1 stopped  Show: [CPU] Memory  [Start all] [Stop all] [+ Add node]
  +-----------------------------+ +-----------------------------+ +---------------------+
  | * Zebra · Mainnet    zebrad | | * Zakura · Mainnet   zakura | | o zcashd · Mainnet  |
  | Syncing 94.5 %              | | Catching up                 | | Stopped 2 days ago  |
  | 5.6 cores · 6.1 GiB    ~~~~ | | 3.2 cores · 4.4 GiB    ~~~~ | |                     |
  +-----------------------------+ +-----------------------------+ +---------------------+
```

- **Network scope.** The sidebar's Mainnet and Testnet tabs scope the overview
  (`AppState.network`, `AppState::scoped_nodes`), as they scope its cards, `PortsTable` and
  `FoldersTable`, so the cards' resource lines and the `ResourcesTable` node rows cover only
  the shown network's nodes. The sketch shows the Mainnet tab.
- **`MachineStrip`** sits above the cards. It is about the computer, so it shows on both
  tabs. It holds one sentence about the computer, which counts every process, and then one
  line per volume. Each volume line names the shown network's nodes on it, stopped ones
  included, and counts the others ("and 2 Testnet nodes"). On Windows the sentence adds
  "Committed 14.1 of 24 GiB"; on macOS "Memory pressure: normal". It takes the warning tone
  while rule 20's condition holds, so that machine-wide fact shows once, here.
- **`NodeCard`** gains a line: "5.6 cores · 6.1 GiB" while running, or "last run: peak
  6.8 GiB" when stopped. A stopped node with no run since the launcher opened shows no
  resource line. It also gets a 120×32 sparkline of the last 15 minutes. A "Show:
  CPU | Memory" control in the overview head switches every card's sparkline; it reuses the
  `RangeControl` pattern.
- **`ResourcesTable`** sits beside `PortsTable` and `FoldersTable`.
  - Columns: Node, CPU, Memory, Disk I/O, Processes, Free on its disk.
  - It lists the shown network's nodes, then one row for the other network's nodes
    ("Testnet nodes (2)"). It ends with an "Other programs (Zeal included)" row and a "This
    computer" row, from one-minute averages, so its rows add up to the computer.
  - The pure
    `other_programs(machine: &MachineHistory, nodes: &[&ResourceHistory], now) -> (f64, u64)`
    takes CPU from `busy_cores` and each node's `cpu_cores` over the last minute, and memory
    from the newest samples. It is clamped at zero. It is given every node of both networks
    (`AppState.nodes`), not only the shown network's, so Zeal's own nodes never count as
    other programs.
- **No stacked per-node charts.** They need a colour per node, and only three series
  colours exist (`charts.css`); they are left for later.

### Copy summary and diagnostics

`summary_text` (`health.rs`) adds two lines after "State on disk":

```text
Resources: 5.6 cores (70 % of 8) · 6.1 GiB, peak 6.4 · 1 process · 35 MB/s read, 95 MB/s written
This computer: 8 CPUs · 16 GiB, 4.7 GiB available · 212.4 GB free on the data disk
```

`copy_diagnostics` adds the same facts through `diagnostics::resources_section`, with one
line per process.

### Mock data (`ui/src/mock/resources.rs`)

- **Shape.** The module is pure and answers through `api::mock_handler`, so every payload
  takes the real JSON round trip. Its jitter comes from a hash of the tick, not
  `Math.random`, so it runs under `cargo test`.
- **The computer.**
  - 8 logical CPUs and 16 GiB. With `?herd` it has 16 CPUs and 32 GiB, so the herd fits
    without a memory warning.
  - Other programs use 0.6–0.9 cores and 5.2 GiB. Swap is 0.3 of 2 GiB.
  - Volumes reuse the mock's constants:
    - `/` holds `DEFAULT_DATA_DIR`, with 212.4 GB free (`FREE_AT_DEFAULT`) of 1 TB;
    - `/mnt/storage` (`PICKED_DATA_DIR`) has 1,834.5 GB free of 4 TB;
    - `/mnt/zcash` holds the herd's folders.
- **The primary zebrad.**
  - It has its own 5 s interval. It publishes while `status.pid` is set, through Starting,
    Running and Stopping. It is not gated on Running like `tick_stats` (`mock.rs:606`).
  - Starting (the first 10 s): 1.1 cores and 180 MB/s read, for the database open.
  - Syncing: 5.6 ± 0.8 cores, 35 MB/s read and 95 MB/s written. Memory climbs from 2.4 to
    6.4 GiB over 30 minutes, then moves in a ±0.3 GiB sawtooth. It always stays above the
    mock's 1.62 GB of state memory (`mock/metrics.rs`).
  - At the tip: 0.25 cores, 3.2 GiB and 0.4 MB/s written.
  - Stopping: 0.6 cores.
  - Always 68 threads and 412 open files.
  - Under existing flags: `?stalled` gives 0.1 cores and almost no I/O; `?nopeers` gives
    0.1 cores; `?crashloop` restarts the run counters with each run.
- **`?herd`.**
  - Zebra · Testnet, at the tip: 0.15 cores, 1.9 GiB.
  - Zakura · Testnet, pruned, at the tip: 0.1 cores, 1.2 GiB.
  - Zakura · Mainnet, catching up: 3.2 cores, 4.4 GiB.
  - Zaino · Mainnet: 0.3 cores, 650 MiB.
  - zcashd · Mainnet, stopped: `None`.

New flags. Each goes into `Flags`, the module's flag table, `ui/README.md` and
`flags_are_read_from_the_query`.

| Flag | Effect |
| --- | --- |
| `?tree` | Adds a Zebend · Mainnet node: a `python3` supervisor (0.05 cores, 40 MiB) → four `python3` services (`sync`, `rpc`, `mempool`, `follow`), each in its own session, using 3.3 cores and 2.6 GiB together. |
| `?os=macos`, `?os=windows` | The platform's labels and fields (pressure on macOS; commit, handles and no threads on Windows). Linux is the default. |
| `?backfill` | Replays 40 minutes of resource and machine samples at load, with the run started 45 minutes ago, so charts and sustained rules show at once. Mock-only. |
| `?lowdisk` | The data volume has 1.4 GB free: rule 19, Problem. |
| `?lowmem` | Other programs leave 4 % available, implying `?backfill`: rule 20, Problem. |
| `?nofit` | 40 GB free while syncing Mainnet, beside the mock's 248.3 GB state (`SYNCED_STATE_BYTES`): rule 21, Attention. |

## Health

### Which facts may raise a verdict

Only three resource facts can raise a verdict, and each names something the user can act
on:

- the free space on the node's data volume;
- the computer's available memory, or the pressure level on macOS and the commit charge on
  Windows;
- on Mainnet zebrad while syncing, whether the chain will fit.

CPU, I/O, process counts, descriptors and the node's own memory size never raise one. They
appear only as context in a reason.

`HealthInput` gains one nested field, `resources: ResourceInput`. To fill it,
`with_health_input` (`dashboard.rs`) borrows four more signals: the node's `resources` and
`resource_history`, and the app's `machine` and `machine_history`.

```rust
pub struct ResourceInput<'a> {
    pub node: Option<&'a NodeResources>,
    pub history: &'a ResourceHistory,
    pub machine: Option<&'a MachineResources>,
    pub machine_history: &'a MachineHistory,
}
```

The node's volume is the entry in `machine.volumes` that lists its id. The new rules are
numbered 19 and up, so existing numbers and `PEER_RULES = [8, 12]` stay valid. They are
evaluated where a cause comes before its symptom, and the page specification's rule table
gains their rows.

### Rules

| Rule | Level | When | Clears when | Action |
| --- | --- | --- | --- | --- |
| 19 Disk almost full | Problem | Under `DISK_FULL_BYTES` (2 GB) free on the newest volume reading | Above 3 GB | Open data folder (new `Action::OpenDataFolder`, via the existing `OpenTarget::DataDir`) |
| 19 Disk space low | Attention | Under min(5 % of the volume, 20 GB) free for `DISK_LOW_FOR` (1 min) | 1 GB above the threshold | Open data folder |
| 20 Memory nearly exhausted | Problem | Linux, Windows: under 5 % available for 2 min. macOS: pressure Critical for 1 min. Windows also: commit above 95 % of the limit for 2 min. | Above 7 %; pressure below Critical; commit under 93 % | Restart, offered only when this node uses at least half the computer's memory |
| 20 Memory low | Attention | Linux, Windows: under 10 % available for 5 min. macOS: pressure Warning for 1 min. | Above 12 %; pressure Normal | The same |
| 21 The chain may not fit | Attention | Mainnet zebrad, not at the tip: free space + the state size (`zebra_state_rocksdb_total_disk_size_bytes`) under `LOW_SPACE_BYTES` (300 GB) × 1.05, for 1 min | Above × 1.10 | Open data folder |

- **Order.**
  - Rules 19 and 20 run first in the running chain, before rule 7 (RPC silent): a full
    disk or exhausted memory is what makes RPC go quiet and the sync stall.
  - Rule 21 is forward-looking, like the end-of-support warning, so it runs after rule 15
    and before rule 16.
- **Staleness.** Rule 20 needs a computer sample younger than `STALE_AFTER` (30 s).
- **Reasons** name the facts:
  - "1.4 GB free on the disk with this node's data (/mnt/zcash). zebrad stops when a write
    fails; free some space or move the data folder."
  - "This computer has 0.6 of 16 GiB available (4 %) for 3 min; Zebra · Mainnet uses
    9.8 GiB of it."
  - "Mainnet needs about 300 GB. This node holds 248.3 GB and its disk has 40.0 GB free."
- **The Restart condition** follows zebra#9699: zebrad used about 7 GB during a Mainnet
  sync on an 8 GB machine, and 1.3 GB after a restart.
- **Other kinds.** Rule 21 skips other kinds and networks until their adapters carry an
  expected size (open question 4).
- **Tile tones.** `DISK_RULES = [19, 21]` sets the existing Disk tile's tone, and
  `MEMORY_RULES = [20]` the Memory tile's. Both sit beside `PEER_RULES` (`tiles.rs`), so
  tiles and verdict cannot disagree.

### Exit context, without new exit classes

Rules 2 (failed), 3 (backoff) and 15 (restarted after a crash) gain one sentence each, from
data the UI already keeps:

- **Memory.** An unrequested exit "killed by signal 9" while the computer had under 5 %
  available, or Critical pressure, in the minute before `last_exit.at`. The added sentence:
  "The computer was almost out of memory just before; the system may have stopped it." The
  Linux OOM killer and macOS jetsam both send `SIGKILL`.
- **Commit (Windows).** A crash while commit was above 95 % of the limit adds: "Windows was
  almost out of memory to commit just before."
- **Disk.** A crash with under 2 GB free on the last volume reading adds: "Only 1.2 GB was
  free on its disk."

`ExitClass::DiskFull` and `ExitClass::OutOfMemory`, read from log lines, are open
question 3.

### Staying quiet during the initial sync

- **CPU and I/O never raise a verdict**, at any stage. While syncing, zebrad uses every
  core by design: the 7.0.0 fixture log shows a 32-thread rayon pool. Its I/O is heavy for
  the same reason. The CPU tile says so in its detail.
- **Disk and memory rules stay on during the sync**, because that is when they bite:
  zebra#9699 happened during a Mainnet sync. Each has a sustain time and a leave margin.
  Only `DISK_FULL_BYTES` fires on a single reading, because a free-space reading is exact
  and the next write may fail.
- **"The chain may not fit" never projects the sync's growth rate**, which is huge and
  temporary. It compares against the expected final size.
- **No "disk fills in N days" projection.** It needs at least 6 h of history at the tip
  (node-exporter's pattern), and the UI keeps only 4 h. It waits for persisted history
  (open question 1).
- **No "busy at the tip" rule.**
  - At the tip, a node that serves Zaino, Zallet or a wallet's rescan is busy for good
    reason. So in the multi-node setups this design is for, such a rule would be noise.
  - node-exporter rates sustained CPU as `info`.
  - A stuck syncer is already caught by rule 9 (stall) and rule 8 (no peers).
- **Machine-wide facts show once.**
  - The memory verdict goes only on the largest memory consumer among the running nodes
    of both networks; the overview strip carries it for the rest, on either tab.
  - Disk verdicts go only on the nodes whose data is on that volume.
- **No desktop notifications** in this design.

## Testing

All tests are native, and CI runs them on ubuntu-24.04, macos-15 (arm64, so the 125/3
timebase path runs) and windows-2025 (`ci.yml`).

### Backend unit tests (no processes)

- **`resources/procfs.rs`,** on fixture text (compiled and run on every OS):
  - `stat` with the comm `) weird (name` and with spaces;
  - a `stat` and a `status` whose comm holds a newline, a tab and a multi-byte character
    cut at 15 bytes;
  - a zombie's `status` without `VmRSS`;
  - `io`;
  - `/proc/stat` with its cpu line and btime;
  - `meminfo`.

  Captures from a Mainnet-syncing zebrad 7.0.0 go under
  `crates/launcher-core/tests/fixtures/proc/`, documented in the fixtures README like the
  metrics page.
- **`resources/fold.rs`, membership.**
  - Group members are found.
  - A child that left the group is found by its parent link.
  - A reused pid under a dead parent is rejected by start time.
  - A sticky member stays after it is orphaned.
  - Kernel threads (pgrp 0) are excluded.
- **`resources/fold.rs`, the fold.**
  - A reap by a member leaves the total unchanged (the worked example).
  - A torn read neither dips nor counts twice: the child is gone but the parent's rollup
    has not grown yet.
  - An orphan reaped outside the tree keeps its seen part in `lost`.
  - A Linux auto-reap is held for one tick and then moved to `lost`.
  - On macOS, a member's I/O goes to `lost` when it exits.
  - Windows job totals pass through.
  - A new run resets the counters.
  - An unreadable field keeps its last value in the totals.
  - The peak follows its rule.
- **`resources.rs`, the actors with a scripted `Reader`.**
  - A `None` probe publishes `None`.
  - Samples arrive every interval, and the first comes at once.
  - A run change resets the fold, and its first rate is the average since the spawn.
  - A slow reader does not pile up ticks.
  - A read that spans a probe change is dropped.
  - A reader panic stops sampling until the next run, and repeated reader errors log one
    warning per run.
  - Dropping the probe's sender ends the task.
  - The machine's busy cores come from two readings.
  - A hung volume read does not block the CPU and memory ticks.
  - Volumes are grouped by device and list their nodes.
- **Supervisor** (`tests/supervisor.rs`).
  - `watch_probe()` is `Some`, with the same pid as `Status.pid`, from Starting until
    `finalize`, and then `None`.
  - Run ids increase across restarts.
  - Windows: after `finalize` the `Weak` no longer upgrades, and the
    `FAKE_ZEBRAD_LINGER_MS` grandchild dies promptly (kill-on-close is not delayed).
- **launcher-types:** serde round trips, `Platform`'s wire names, and samples missing
  their optional fields still deserialize.
- **`diagnostics.rs`:** the text of the resources section.

### Live tests (`crates/launcher-core/tests/resources.rs`)

They use `support/helpers.rs`, a 200 ms interval and `watch::Receiver::wait_for`.

New fake-zebrad modes, documented in its mode table:

| Variable | Effect | Test |
| --- | --- | --- |
| `FAKE_ZEBRAD_BURN_MS=1500` | One thread spins after `Starting zebrad`. | `cpu_secs` ≥ 0.8; some sample has `cpu_cores` > 0.3. |
| `FAKE_ZEBRAD_ALLOC_MB=128` | Allocates, touches every page, holds until exit. | Memory ≥ 100 MiB; peak ≥ memory. |
| `FAKE_ZEBRAD_WRITE_MB=32` | Writes under `CARGO_TARGET_TMPDIR`, on a real disk rather than a tmpfs `/tmp`, then `sync_all`. | `written_bytes` ≥ 32 MB (lenient on Windows, where pipes count). |
| `FAKE_ZEBRAD_CHILD=busy` | A `fake-zebrad child` that spins 500 ms and lives 10 s, waited for. | 2 processes while it lives; `cpu_secs` does not drop after it exits (Linux `cutime`, macOS `ri_child_*`, Windows job). |
| `FAKE_ZEBRAD_CHILD=session` | The child calls `setsid()` first, like Zebend's services. | It is a member: by parent link on Unix, by the job on Windows. |
| `FAKE_ZEBRAD_CHILD=short` | A child that spins 100 ms and exits; this test samples every 1 s, so the child falls between two samples. | Its CPU is still counted; it is never listed. |
| `FAKE_ZEBRAD_CHILD=late` | A child that spins 500 ms and exits; fake-zebrad reaps it 15 s later. | `cpu_secs` counts the child's CPU once on every OS. |

The ignored real-zebrad contract job (`tests/real_zebrad.rs`) gains one check: on Regtest,
the pinned zebrad's memory is at least its own state memory, it is one process (its Regtest
config sets no `block_notify_command`), and its descriptor count is above zero.

### UI tests (native)

- **`resource_history`:**
  - rates over windows;
  - NaN at run boundaries and gaps;
  - `process_rates`;
  - `last_run`.
- **`machine_history`:** episodes with their leave margins, and a stale sample.
- **Charts:**
  - the `Unit` labels ("6.1 GiB", "2.4 cores", "45 MB/s") and whole-GiB ticks;
  - `line_path` with a NaN gives two subpaths;
  - the end label uses the last finite value;
  - charts with `Unit::Count` are byte-identical to today's.
- **Health,** for each new rule:
  - thresholds, sustain times and leave margins;
  - syncing compared with at the tip;
  - its place in the order (`disk_full_beats_rpc_silent`);
  - the Restart offer and the exit sentences.
- **Overview and text:**
  - `other_programs` clamping;
  - the tile tone sets;
  - the `summary_text` lines;
  - the card's text helpers.
- **Mock:**
  - the flags are read from the query;
  - memory stays above state memory;
  - `?tree` has five processes with the right parents;
  - `?lowdisk` is under `DISK_FULL_BYTES`.

## Dependencies and unsafe code

### What is added

Only cargo features, in `crates/launcher-core/Cargo.toml`. No crate is added, so
`Cargo.lock` does not change (it records no features). cargo-deny sees nothing new: no
licence, no advisory, no duplicate. No pinned tool changes.

```toml
[target.'cfg(unix)'.dependencies]
# "feature": sysconf (CLK_TCK and _NPROCESSORS_ONLN), for the Linux reader.
nix = { workspace = true, features = ["signal", "process", "fs", "feature"] }

[target.'cfg(windows)'.dependencies]
windows-sys = { workspace = true, features = [
    "Win32_Foundation",
    "Win32_System_JobObjects",
    "Win32_System_Threading",
    "Win32_System_Diagnostics_ToolHelp",
    "Win32_Storage_FileSystem",
    # K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX, K32GetPerformanceInfo.
    "Win32_System_ProcessStatus",
] }
```

- **nix 0.31.3** (MIT, already locked). `sysconf` sits behind `#![feature = "feature"]`
  (`unistd.rs:2575`), and that feature has no dependencies (`feature = []`).
- **windows-sys 0.61.2** (MIT OR Apache-2.0, already locked).
  - Everything else is in features already on:
    - JobObjects: `QueryInformationJobObject`, `IsProcessInJob`, both information classes;
    - Threading: `GetProcessTimes`, `GetProcessIoCounters`, `GetProcessHandleCount`,
      `GetSystemTimes`, `GetActiveProcessorCount`, `WaitForSingleObject`,
      `PROCESS_SYNCHRONIZE`, and `JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION`, which is
      gated on Threading;
    - Storage_FileSystem: `GetVolumePathNameW`, `GetDiskFreeSpaceExW`.
  - `Win32_System_SystemInformation` is not needed, because `GlobalMemoryStatusEx` is not
    used.
- **libc 0.2.191** (macOS, already a dependency for `proc_pidpath`). It binds everything
  needed:
  - `proc_listpgrppids`, `proc_listchildpids`, `proc_pidinfo` and `proc_pid_rusage`;
  - `rusage_info_v4`, `proc_taskallinfo` and `proc_bsdinfo`;
  - `PROC_PIDTASKALLINFO`, `PROC_PIDTBSDINFO`, `SZOMB` and `RUSAGE_INFO_V4`;
  - `sysctlbyname`, `xsw_usage`;
  - `sysconf` and `_SC_NPROCESSORS_ONLN`;
  - `host_statistics` and `host_statistics64`, which are not deprecated.

### Where `unsafe` sits and why

| File | Calls | Why |
| --- | --- | --- |
| `resources/linux.rs` | none | `std::fs` reads and nix's safe `sysconf`; `firstrun.rs` already uses nix's safe `statvfs`. |
| `resources/macos.rs` | `proc_listpgrppids`, `proc_listchildpids`, `proc_pidinfo` (`PROC_PIDTASKALLINFO`, `PROC_PIDTBSDINFO`, `PROC_PIDLISTFDS`), `proc_pid_rusage`, `sysctlbyname` (`hw.memsize`, `vm.swapusage`, `kern.memorystatus_vm_pressure_level`, `sysctl.proc_translated`), `sysconf` (`_SC_NPROCESSORS_ONLN`), `host_statistics`, `host_statistics64`, and two local declarations (below) | libproc and Mach have no safe binding in the tree, and nix's safe `sysconf` has no `_NPROCESSORS_ONLN` on macOS. |
| `resources/windows.rs` | `OpenProcess`, `WaitForSingleObject`, `GetProcessTimes`, `K32GetProcessMemoryInfo`, `GetProcessIoCounters`, `GetProcessHandleCount`, `QueryFullProcessImageNameW` (the existing pattern), `GetSystemTimes`, `GetActiveProcessorCount`, `K32GetPerformanceInfo` | Win32. |
| `process/windows.rs` | `QueryInformationJobObject` (two classes), `IsProcessInJob` | The job handle stays in this file. |
| `firstrun.rs` | `GetDiskFreeSpaceExW` (existing, now with its total), `GetVolumePathNameW` | Win32. |

Each call follows the house pattern:

- a `// SAFETY:` comment;
- buffers sized from the struct;
- the returned count or length checked;
- handles wrapped in `OwnedHandle` at once;
- variable-length lists grown and retried at most three times.

The two local declarations in `resources/macos.rs`:

```rust
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
    safe fn mach_host_self() -> libc::mach_port_t;
}
```

libc 0.2.191 binds both but marks them deprecated
(``#[deprecated(since = "0.2.55", note = "Use the `mach2` crate instead")]``,
`src/unix/bsd/apple/mod.rs:4755-4760`), and CI builds with `RUSTFLAGS: -D warnings`
(`ci.yml:17`). Declaring them locally avoids both an `#[allow(deprecated)]` and the mach2
crate for two symbols.

### No mutexes

- Each sampler is an actor that owns its state; the fold never leaves it.
- A blocking read takes its inputs by value and returns owned output.
- Outputs are `watch` channels.
- `Arc<Job>` and `Weak<Job>` (reference counts, not locks) are the only shared ownership,
  made in one place, `ChildHandle::probe`.
- The machine sampler's volume read is tracked by a flag the actor owns.
- clippy's `disallowed_types` (`clippy.toml`) stays as it is.

### Rejected

- **sysinfo 0.40.1.** It is MIT-licensed and maintained, its dependencies pass deny.toml,
  and it would need no `unsafe` in Zeal. But its figures differ from the OS monitors', it
  keeps no child rollups, it cannot use the job, and it lists every process on each
  refresh ([Alternatives considered](#alternatives-considered)). The `Reader` trait keeps a
  sysinfo-backed reader possible later, for an OS without a reader of its own.
- the `mach2` crate, for two symbols;
- the `procfs` and `libproc` crates, which have more surface than the parsers they would
  replace;
- a cgroup v2 boundary, because delegation varies across desktop sessions and AppImage
  (unconfirmed);
- `PR_SET_CHILD_SUBREAPER`, because Zeal would then have to reap adopted orphans with
  `waitpid(-1)`, which would steal tokio's child statuses.

## Phases

Each phase is one pull request that ships on its own, with screenshots from the mock
(headless Chromium, the usual workflow).

1. **The Resources card against the mock (UI only).**
   - The launcher-types module and event names.
   - `mock/resources.rs` for the primary node, with `?os=`.
   - The card's four tiles, the process table, the stopped state and the footer line.
   - A minimal `ResourceHistory`: `push`, `latest`, one-minute rates, `process_rates`,
     `spark`, `last_run`.

   Under Tauri the `api.rs` wrappers subscribe to nothing and return `None`, the way
   `extra_nodes()` returns nothing today, so the card stays hidden and nothing changes for
   users. Screenshots: syncing, at the tip, stopped, `?os=macos`, `?os=windows` and a
   narrow window.

   **Built.** As planned, with these differences:

   - `MachineResources` has no `volumes` field yet, and `VolumeUsage` and
     `NodeResourcesEvent` do not exist: they come with phases 5 and 7. The field defaults on
     the wire, so adding it later breaks nothing. The card's footer line has no free-space
     part until then.
   - `MemoryPressure::as_str` gives the footer's words; the macOS memory label reads "Memory
     as Activity Monitor shows it", the basis line's wording.
   - `format::mem_bytes` drops a trailing `.0`, so totals read "16 GiB" and a commit charge of
     exactly 7 GiB reads "7 GiB committed".
   - The Disk and Processes tiles show their value on two lines, as in the sketch. The Disk
     tile's sparkline plots the write rate. While stopped, the tiles keep the last run's
     sparklines.
   - "syncing uses every CPU" follows zebrad's own progress lines (syncing, slow or close to
     the tip), so a stalled node shows the run's average instead.
   - The footer's busy share is the newest computer sample's (its last 5 s); one-minute
     averages of the computer wait for `MachineHistory` (phase 4).
   - The mock has no flag for the tip: its sync reaches the tip after about 47 minutes, when
     the card shows the tip's figures.
   - The node card's "State memory (of 6.1 GiB in use)" cross-check is in no phase yet and is
     not built.
2. **Live on Linux.**
   - `TreeProbe`, `ChildHandle::probe` and `watch_probe` (`install_run`, `finalize`).
   - `resources/{procfs, fold, linux}.rs`.
   - `ResourceSampler`, and `MachineSampler` with CPU and memory only.
   - The Tauri wiring: `AppState`, the bridge, and the two commands in the three lists.
   - The Copy diagnostics section.
   - The fake-zebrad modes and the tests on ubuntu-24.04.
   - A debug log line with each tick's read time.

   The macOS and Windows readers return "unsupported", so the card stays hidden there.
   Ships: Linux users see their zebrad's CPU, memory, disk I/O and processes. Screenshot:
   the app under `cargo tauri dev` with `FAKE_ZEBRAD_BURN_MS`.
3. **macOS and Windows.**
   - `resources/macos.rs` with the local Mach declarations.
   - `Job::{accounting, process_ids, contains}` and `resources/windows.rs`.
   - `ChildHandle.job: Arc<Job>`, with the probe's `Weak`.
   - The machine readers for both OSes.
   - The live tests on macos-15 and windows-2025, including job retention and kill-on-close
     timing.
   - The image-name extraction, if needed, as its own pure-move commit first.

   Before release, test a notarized, hardened-runtime build by hand. Report CPU as "not
   available" under Rosetta until its timebase is checked. Screenshots from both OSes.
4. **History charts.**
   - `Unit` and gap support in the chart code; the existing charts are byte-identical.
   - The three charts.
   - `ui/src/machine_history.rs` with `busy_cores`, and `AppState.machine_history`.
   - The Copy summary lines.
   - `?backfill`.

   Ships: up to 4 h of resource history on the existing range control.
5. **Free space and verdicts.**
   - `firstrun::volume`, and volumes in `MachineResources` every 30 s.
   - Rules 19–21, `DISK_RULES` and `MEMORY_RULES`, and `Action::OpenDataFolder`.
   - The sustain helpers (`available_below_for`, `pressure_at_least_for`,
     `commit_above_for`) and `Episode` in `machine_history.rs`.
   - The exit sentences, and the Disk tile's free space.
   - `?lowdisk`, `?lowmem` and `?nofit`.

   Screenshots of each verdict. Ships: Zeal warns before the data disk or the memory runs
   out, and says so when a crash followed either.
6. **Overview.**
   - `MachineStrip` with its volume lines.
   - The card lines and sparklines.
   - `ResourcesTable` with "Other programs".
   - The `?herd` data.
   - `?tree`: a Zebend · Mainnet node in `mock::herd` and `on_extra_node_resources` in
     `subscribe_extra`, with a screenshot of the Zebend node's page.

   It ships in the mock at once; real users see it when MULTI-NODE phase 2 brings several
   nodes.
7. **Several nodes and wrapper trees** (with MULTI-NODE phases 2 and 4).
   - One sampler per `NodeRuntime`, and the node-scoped events and commands.
   - `NodeSummary`'s resource fields for the tray.
   - The machine sampler fed the registry's folders.
   - Generic and Zebend trees; the parent links and sticky members already cover
     `start_new_session`.
   - `dir_size` for kinds without a size metric.
   - Leftover processes reported after the root exits.

Through every phase:

- this document is kept current;
- ARCHITECTURE.md gains rows in "Process control per OS" and "Status and metrics";
- `ui/README.md` and the mock's flag table list the new flags.

## Open questions

1. **Disk history.** Should Zeal persist a coarse disk history (say every 10 minutes for
   30 days) so a "disk fills in N days" projection can work? That breaks "nothing is
   persisted".
2. **Zeal's own use.** Should Zeal and its webviews get a row of their own, or stay in
   "Other programs"?
3. **Exit classes.** Add `ExitClass::DiskFull` (from "No space left on device" or "There is
   not enough space on the disk") and `ExitClass::OutOfMemory` (from "memory allocation of
   N bytes failed")? Should a full disk stop the restarts? This design adds only reason
   sentences.
4. **Expected sizes.** Who keeps the expected chain size per kind and network that rule 21
   needs? Today there is only Mainnet zebrad's 300 GB (`LOW_SPACE_BYTES`).
5. **Windows threads.** Is a thread count worth a Toolhelp snapshot every 30 s while the
   process table is open?
6. **Escaped members.** Should the stop escalation also kill sticky Unix members outside
   the group (MULTI-NODE phase 4), using `pidfd_send_signal` on Linux against pid reuse?
   This design only reports them.
7. **Memory labels.** GiB, as proposed, or binary figures labelled "GB", as Activity
   Monitor and Task Manager print them?
8. **Linux PSI.** Where it is enabled, is `/proc/pressure/memory` a better memory signal
   than MemAvailable?
9. **Shared disks.** MULTI-NODE.md (Data and disk) has the overview sum the expected sizes
   of the nodes on one disk against its free space. Rule 21 checks each node alone, so two
   Mainnet zebrads on one disk can each pass while together they do not fit. Should rule 21
   count the shared free space once, or is the sum left to MULTI-NODE?

Unconfirmed, to be checked in the phase named:

- **Phase 3:**
  - libproc calls in a notarized, hardened-runtime build. No hardened-runtime entitlement
    concerns process information, and Zeal is not sandboxed, so they are expected to work.
  - The Mach timebase a Rosetta-translated process sees, and whether `hw.tbfrequency` gives
    the native rate instead.
  - The macOS available-memory formula, against Activity Monitor.
  - How much network traffic the Windows job's I/O counters include.
  - Whether `QueryInformationJobObject` fails with `ERROR_MORE_DATA` on a short process
    list (the loop handles both cases).
- **Phase 2:** the cost of the `/proc` scan with thousands of processes, from the per-tick
  log.
- **MULTI-NODE phases 3–5:** whether Zakura, Zaino, Zallet and the zcashd build start
  children or export process metrics of their own.
- **Estimates:** the 16,000-SST estimate, and a Rust `/proc` scan of a few hundred processes
  in under 5 ms.

## Alternatives considered

Two ways to measure were weighed:

- **Own readers:** Zeal's own per-OS readers and the job's accounting, which this design uses.
- **sysinfo:** sysinfo 0.40.1 with its default features off, behind a trait.

This design keeps the own readers' measurement and the restraint in the UI that the sysinfo
approach argued for. The comparison follows eight questions.

### OS API claims

**The own readers** are right where checked:

- job totals include terminated processes, and the process list must be grown and retried
  ([sdk-api][jobobject-list]);
- `PROCESS_QUERY_LIMITED_INFORMATION` is enough for `GetProcessMemoryInfo`
  ([sdk-api][getprocessmemoryinfo]);
- libproc's list calls return counts of pids;
- XNU's times are Mach ticks;
- Linux folds reaped children's CPU and I/O into the parent;
- its correction of the research's rate rule (the worked example above).

Their first version had two errors and one race, which this design corrects:

- It skips job members whose `GetProcessTimes` exit time is set. Microsoft: "If the process
  has not exited, the content of this structure is undefined." This design waits on the
  handle instead.
- It reads commit from `GlobalMemoryStatusEx`, whose `ullTotalPageFile` is "the current
  committed memory limit for the system or the current process, whichever is smaller"
  ([sdk-api][memorystatusex]). This design uses `K32GetPerformanceInfo`, which is
  system-wide ([sdk-api][performance-information]) and drops one windows-sys feature.
- Its fold counts twice when a reap lands between the reads of a parent and its child: the
  child goes to `lost` and then reappears in the parent's rollup. This design waits one
  tick before moving a departed child to `lost`.

**sysinfo** is described here as its 0.40.1 source behaves. `winapi` 0.3.9 is already in
`Cargo.lock`, through dlopen2, libloading and gio-sys, so sysinfo would add only itself,
ntapi on Windows and objc2-io-kit on macOS. What it gives up is more than it seems, though:

- CPU of short-lived children and of orphans is lost. Short-lived children are the pattern
  of Zebend's supervisor.
- macOS memory is RSS (`pti_resident_size`), and there are no OS peaks. RSS leaves out
  compressed pages, so it reads below Activity Monitor exactly under memory pressure.
- Without the job, Windows trees come from Toolhelp parent pids, which can be stale. Only
  start times in whole seconds guard them.

### The no-mutex rule

Both comply:

- an actor owns its state and publishes through `watch`;
- inputs move into `spawn_blocking` and back, so nothing is shared;
- the own readers' `Arc<Job>` and `Weak<Job>` count references and take no lock.

One actor for all nodes would need a registration protocol; one sampler per node needs
none.

### Dependencies and licences

- **Own readers:** adds two cargo features and no crate.
- **sysinfo:** adds sysinfo (MIT), plus ntapi 0.4.3 (Apache-2.0 OR MIT) on Windows and
  objc2-io-kit 0.3.2 (Zlib OR Apache-2.0 OR MIT) on macOS.
  - All of these are on deny.toml's list.
  - `[graph] all-features = true` turns on the features of workspace members only, so
    cargo-deny also sees sysinfo with its defaults off.
  - Its cost is churn, not licences: three breaking releases in 2026 (0.38, 0.39 and
    0.40, with the MSRV going from 1.88 to 1.95 in 0.39), and soundness fixes in 0.39.0,
    0.39.4 and 0.40.0.

### Cost on a syncing node

Both are negligible beside zebrad's five or more busy cores.

- **Own readers:** reads a few in-memory proc files or makes a few calls per member. It never
  reads `smaps`, which takes a busy process's memory-map lock, and never walks a running
  zebrad's data folder.
- **sysinfo:** enumerates every process on each refresh on macOS and Windows, even when it
  measures only the root: on Windows a full `CreateToolhelp32Snapshot`
  (`windows/system.rs:235-237`). Finding descendants needs `ProcessesToUpdate::All`, which
  on Windows opens a handle with `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ` to every
  process (`windows/process.rs:73`). Endpoint-security tools may flag that pattern
  (unconfirmed).

### PID reuse and short-lived children

- **Own readers:**
  - identities with fine start times: 10 ms on Linux, 1 µs on macOS, 100 ns on Windows;
  - `IsProcessInJob`;
  - POSIX's rule that a group id is not reused while the group lives;
  - kernel rollups, and exact totals on Windows.
- **sysinfo:** start times in whole seconds and no rollups. A child that starts and exits
  between two samples is lost, and so is a descendant whose parent has died.

### Windows job semantics

- **Own readers:** uses the job for both the boundary and the totals. It holds the job only
  through a `Weak`, so `KILL_ON_JOB_CLOSE` timing does not change; a duplicated handle
  would delay it by a tick.
- **sysinfo:** leaves the job unused. Its own fallback for Windows needs the job handle, and
  that is the supervisor change it set out to avoid.

### macOS hardened runtime

The same for both:

- Zeal is signed with a Developer ID, uses the hardened runtime and is not sandboxed.
- No hardened-runtime entitlement concerns process information.
- XNU first runs a MAC hook (`mac_proc_check_proc_info`), which is where the App Sandbox
  would step in. It then requires the same uid, unless the caller holds
  `PRIV_GLOBAL_PROC_INFO`.

This is unconfirmed by test, so phase 3 checks a notarized build. The Rosetta timebase
question also applies to both.

### Noise in the UI

- **The own readers' first rules were too loud for several nodes.**
  - "Busy at the tip" would fire on a zebrad that serves Zaino, Zallet or a rescan.
  - Its 10-minute sustain on the low-disk warning is long for a node that writes as fast
    as it syncs.
- **The sysinfo approach's two rules are the right scope**, but they have no leave margin,
  so its 20 GB warning would flap during compaction.
- **This design** keeps that scope, the own readers' leave margins and their "chain may not
  fit".
- **Charts.** A `Unit` enum gives readable whole-GiB ticks; pre-scaled values with a unit
  string do not.
- **Both** keep tables in tree order and show last-run values, not zeros, for a stopped
  node.

### What this design takes from each

- **From A:**
  - the per-OS readers, the job's accounting, the kernel rollups and the fold;
  - identities and sticky members;
  - the probe with a `Weak<Job>`, and measuring from the spawn to `finalize`;
  - volumes in the machine sample;
  - the local Mach declarations;
  - leave margins;
  - the mock backfill, and most of the tests.
- **From B:**
  - the reader trait, which keeps a sysinfo reader possible;
  - two disk and memory rules and no CPU verdict;
  - the Restart condition (the node uses half the memory);
  - the chart `Unit` enum;
  - per-process I/O;
  - `?os=`;
  - I/O tests on a real disk, and the real-zebrad memory check;
  - the per-tick timing log.
- **New here:**
  - one sampler per node;
  - the Windows liveness and commit fixes, and the torn-read grace;
  - no busy-at-the-tip rule;
  - no disk projection until history outlives 4 h.

## Sources

Zeal, branch `main` at 02482f7 (after #88, the network tabs): the paths cited above, with
[MULTI-NODE.md](MULTI-NODE.md) for the node kinds and the registry.

**Operating systems:**

- **Microsoft:**
  - [GetProcessTimes][getprocesstimes]
  - [GetProcessMemoryInfo][getprocessmemoryinfo]
  - [JOBOBJECT_BASIC_PROCESS_ID_LIST][jobobject-list]
  - [JOBOBJECT_BASIC_ACCOUNTING_INFORMATION][jobobject-accounting]
  - [PERFORMANCE_INFORMATION][performance-information]
  - [MEMORYSTATUSEX][memorystatusex]
  - [Nested jobs][nested-jobs]
  - [IO_COUNTERS](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-io_counters)
    and the scope of the I/O counters, "file, network, and device"
    ([WMI, archived](https://learn.microsoft.com/en-us/previous-versions/aa394323(v=vs.85)))
  - Raymond Chen on [pid reuse][chen-reuse] and [stale parent pids][chen-ppid]
- **POSIX:** [Process ID Reuse][posix-reuse].
- **Linux man pages:**
  - [proc_pid_stat(5)](https://man7.org/linux/man-pages/man5/proc_pid_stat.5.html)
  - [proc_pid_status(5)](https://man7.org/linux/man-pages/man5/proc_pid_status.5.html)
  - [proc_pid_statm(5)](https://man7.org/linux/man-pages/man5/proc_pid_statm.5.html)
  - [proc_pid_io(5)](https://man7.org/linux/man-pages/man5/proc_pid_io.5.html)
  - [proc_tid_children(5)](https://man7.org/linux/man-pages/man5/proc_tid_children.5.html)
- **Linux source:** `fs/proc/base.c`, `fs/proc/array.c` and `kernel/exit.c`.
- **XNU source** ([apple-oss-distributions/xnu](https://github.com/apple-oss-distributions/xnu)):
  - `bsd/kern/proc_info.c`
  - `bsd/kern/kern_exit.c`
  - `bsd/kern/kern_resource.c`
  - `bsd/kern/kern_memorystatus_notify.c`
  - `libsyscall/wrappers/libproc/libproc.c`
- **Apple:** [Hardened Runtime](https://developer.apple.com/documentation/security/hardened-runtime).

**Crates, as locked:**

- libc 0.2.191 (`src/unix/bsd/apple/mod.rs`);
- nix 0.31.3 (`src/unistd.rs`, `Cargo.toml`);
- windows-sys 0.61.2 (`System/{JobObjects,ProcessStatus,Threading}`);
- sysinfo 0.40.1 (`src/windows/{system,process}.rs`).

**Zebra:**

- [requirements](https://zebra.zfnd.org/user/requirements.html);
- issues [#9699](https://github.com/ZcashFoundation/zebra/issues/9699),
  [#11090](https://github.com/ZcashFoundation/zebra/issues/11090) and
  [#1634](https://github.com/ZcashFoundation/zebra/issues/1634);
- `zebra-state` at the pinned commit e8db87eb, for the RocksDB gauges.

**Monitoring practice:** node-exporter's alert rules
([node-mixin](https://github.com/prometheus/node_exporter/blob/master/docs/node-mixin/alerts/alerts.libsonnet)).

[getprocesstimes]: https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes
[getprocessmemoryinfo]: https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getprocessmemoryinfo
[jobobject-list]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_process_id_list
[jobobject-accounting]: https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_accounting_information
[performance-information]: https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-performance_information
[memorystatusex]: https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/ns-sysinfoapi-memorystatusex
[nested-jobs]: https://learn.microsoft.com/en-us/windows/win32/procthread/nested-jobs
[chen-reuse]: https://devblogs.microsoft.com/oldnewthing/20110107-00/?p=11803
[chen-ppid]: https://devblogs.microsoft.com/oldnewthing/20150403-00/?p=44313
[posix-reuse]: https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/V1_chap04.html
