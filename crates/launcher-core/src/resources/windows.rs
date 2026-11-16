//! The Windows readers. A node's tree is its job (docs/RESOURCES.md, "Windows: the job"): the
//! job's process list names the members, and its accounting gives the CPU and I/O totals of
//! every process ever in it, the exited ones included.
//!
//! Each listed pid is read through one handle. `IsProcessInJob` rejects a pid that now names
//! another process, and a wait of zero skips a process that has exited but is still listed
//! while handles to it are open. Then `GetProcessTimes` (the creation time is the identity),
//! `K32GetProcessMemoryInfo`, `GetProcessIoCounters`, `GetProcessHandleCount` and
//! `QueryFullProcessImageNameW` read it. Parent pids are not read: the job's list is
//! authoritative, and Toolhelp's parents can be stale. Threads are not read either, since that
//! needs a snapshot of the whole computer.
//!
//! The computer comes from `GetSystemTimes`, `GetActiveProcessorCount` and
//! `K32GetPerformanceInfo`. `unsafe` is only around the Win32 calls; the job's handle stays in
//! `process/windows.rs`.

use std::{
    io,
    mem::size_of,
    os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::{
    Foundation::{FILETIME, WAIT_TIMEOUT},
    System::{
        ProcessStatus::{
            K32GetPerformanceInfo, K32GetProcessMemoryInfo, PERFORMANCE_INFORMATION,
            PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::{
            ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetProcessHandleCount,
            GetProcessIoCounters, GetProcessTimes, GetSystemTimes, IO_COUNTERS, OpenProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        },
    },
};

use super::{
    Denied, Identity, JobTotals, MachineReader, MachineReading, MemberReading, Reader, TreeReading,
    clean_name,
};
use crate::process::{
    TreeProbe,
    windows::{Job, image_path},
};

/// How far the root's creation may be from the spawn the supervisor saw: the supervisor reads
/// its clock once the suspended process is in its job and resumed.
const ROOT_START_TOLERANCE: Duration = Duration::from_secs(5);

/// FILETIME's epoch, 1601-01-01, in 100 ns units before the Unix epoch.
const UNIX_EPOCH_100NS: u64 = 116_444_736_000_000_000;

/// 100 ns units in a second.
const UNITS_PER_SEC: f64 = 10_000_000.0;

/// Reads a node's job and the computer.
pub(super) struct WindowsReader;

impl Reader for WindowsReader {
    fn tree(&self, probe: &TreeProbe, sticky: &[Identity]) -> io::Result<TreeReading> {
        // Held for this read only. The run's handle keeps the job; when the run is finalized,
        // dropping the handle kills what is left in it even while this read holds it.
        let job = probe
            .job
            .upgrade()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the run's job is closed"))?;
        let pids = job.process_ids()?;
        let at = SystemTime::now();
        let mono = Instant::now();
        let mut denied = Denied::default();
        let mut members = Vec::with_capacity(pids.len());
        for pid in pids {
            if let Some(member) = read_member(&job, pid, &mut denied)? {
                members.push(member);
            }
        }
        // After the members, so the totals hold at least what they read.
        let accounting = job.accounting()?;
        drop(job);
        let root = sticky
            .iter()
            .find(|id| id.pid == probe.pid)
            .copied()
            .or_else(|| root_at_spawn(&members, probe));
        members.sort_by_key(|member| (Some(member.id) != root, member.id.start, member.id.pid));
        Ok(TreeReading {
            at,
            mono,
            members,
            denied,
            job: Some(JobTotals {
                cpu: accounting.cpu_100ns as f64 / UNITS_PER_SEC,
                read: accounting.read_bytes,
                written: accounting.written_bytes,
            }),
        })
    }
}

/// Reads the job's process `pid`. `None` when it has exited, when its pid names a process
/// outside the job now, or when it cannot be opened; the job's totals count it all the same.
fn read_member(job: &Job, pid: u32, denied: &mut Denied) -> io::Result<Option<MemberReading>> {
    // SAFETY: OpenProcess has no preconditions; a null result is checked below.
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if raw.is_null() {
        return Ok(None);
    }
    // SAFETY: the handle was just returned by OpenProcess and is owned by us.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    // A pid is not reused while a handle to its process is open, so this answer holds for
    // every read below.
    if !job.contains(process.as_handle())? {
        return Ok(None);
    }
    if exited(&process) {
        return Ok(None);
    }
    let fields = (|| {
        Ok((
            times(&process)?,
            optional(memory(&process), &mut denied.memory)?,
            optional(io_counters(&process), &mut denied.io)?,
            optional(handle_count(&process), &mut denied.descriptors)?,
        ))
    })();
    let ((created, cpu), memory, io, handles) = match fields {
        Ok(fields) => fields,
        // It exited during the reads.
        Err(_) if exited(&process) => return Ok(None),
        Err(err) => return Err(err),
    };
    let name = image_path(process.as_handle())
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    Ok(Some(MemberReading {
        id: Identity {
            pid,
            start: created,
        },
        ppid: 0,
        name: clean_name(name.as_bytes()),
        started_at: system_time(created),
        cpu_own: cpu as f64 / UNITS_PER_SEC,
        cpu_rollup: 0.0,
        memory: memory.map(|counters| bytes(counters.WorkingSetSize)),
        peak: memory.map(|counters| bytes(counters.PeakWorkingSetSize)),
        committed: memory.map(|counters| bytes(counters.PrivateUsage)),
        read: io.map(|io| io.ReadTransferCount),
        written: io.map(|io| io.WriteTransferCount),
        threads: None,
        descriptors: handles,
    }))
}

/// Whether the process has exited, though it may still be listed while handles to it are open.
fn exited(process: &OwnedHandle) -> bool {
    // SAFETY: the handle is valid and has SYNCHRONIZE access; a zero timeout only polls.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) != WAIT_TIMEOUT }
}

/// A field's value, or `None` with `denied` set when the OS refused it.
fn optional<T>(read: io::Result<T>, denied: &mut bool) -> io::Result<Option<T>> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
            *denied = true;
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// The root on the run's first reading: the job's process with the probe's pid, if it was
/// created within [`ROOT_START_TOLERANCE`] of the spawn.
fn root_at_spawn(members: &[MemberReading], probe: &TreeProbe) -> Option<Identity> {
    let member = members.iter().find(|member| member.id.pid == probe.pid)?;
    let skew = match member.started_at.duration_since(probe.spawned_at) {
        Ok(after) => after,
        Err(before) => before.duration(),
    };
    (skew <= ROOT_START_TOLERANCE).then_some(member.id)
}

/// The creation time, and the kernel plus user time, in 100 ns units.
fn times(process: &OwnedHandle) -> io::Result<(u64, u64)> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: the handle is valid with PROCESS_QUERY_LIMITED_INFORMATION access, and the four
    // FILETIMEs are writable for the duration of the call.
    let ok = unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((units(creation), units(kernel).saturating_add(units(user))))
}

/// The working set, its peak and the commit charge (`PrivateUsage`).
fn memory(process: &OwnedHandle) -> io::Result<PROCESS_MEMORY_COUNTERS_EX> {
    let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>())
        .expect("the memory counters are a few dozen bytes");
    let mut counters = PROCESS_MEMORY_COUNTERS_EX {
        cb: size,
        ..PROCESS_MEMORY_COUNTERS_EX::default()
    };
    // SAFETY: the handle is valid with PROCESS_QUERY_LIMITED_INFORMATION access; `counters` is
    // a writable PROCESS_MEMORY_COUNTERS_EX of `size` bytes, which the call takes in place of a
    // PROCESS_MEMORY_COUNTERS when `size` says so.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            process.as_raw_handle(),
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(counters)
}

fn io_counters(process: &OwnedHandle) -> io::Result<IO_COUNTERS> {
    let mut io = IO_COUNTERS::default();
    // SAFETY: the handle is valid with PROCESS_QUERY_LIMITED_INFORMATION access, and `io` is
    // writable for the duration of the call.
    if unsafe { GetProcessIoCounters(process.as_raw_handle(), &raw mut io) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(io)
}

fn handle_count(process: &OwnedHandle) -> io::Result<u32> {
    let mut count = 0;
    // SAFETY: the handle is valid with PROCESS_QUERY_LIMITED_INFORMATION access, and `count` is
    // writable for the duration of the call.
    if unsafe { GetProcessHandleCount(process.as_raw_handle(), &raw mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(count)
}

/// A FILETIME as one count of 100 ns units.
fn units(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// A FILETIME count, 100 ns units since 1601, as a SystemTime.
fn system_time(units: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(units.saturating_sub(UNIX_EPOCH_100NS).saturating_mul(100))
}

fn bytes(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

impl MachineReader for WindowsReader {
    fn machine(&self) -> io::Result<MachineReading> {
        let at = SystemTime::now();
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: the three FILETIMEs are writable for the duration of the call.
        if unsafe { GetSystemTimes(&raw mut idle, &raw mut kernel, &raw mut user) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let idle = units(idle);
        // The kernel time includes the idle time.
        let busy = units(kernel)
            .saturating_sub(idle)
            .saturating_add(units(user));
        // SAFETY: GetActiveProcessorCount takes no pointers.
        let cpus = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
        let size = u32::try_from(size_of::<PERFORMANCE_INFORMATION>())
            .expect("the performance information is a few dozen bytes");
        let mut perf = PERFORMANCE_INFORMATION {
            cb: size,
            ..PERFORMANCE_INFORMATION::default()
        };
        // SAFETY: `perf` is a writable PERFORMANCE_INFORMATION of `size` bytes.
        if unsafe { K32GetPerformanceInfo(&raw mut perf, size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // The memory figures are in pages.
        let pages = |count: usize| bytes(count).saturating_mul(bytes(perf.PageSize));
        Ok(MachineReading {
            at,
            logical_cpus: cpus.max(1),
            cpu_busy: busy,
            cpu_idle: idle,
            memory_total: pages(perf.PhysicalTotal),
            memory_available: Some(pages(perf.PhysicalAvailable)),
            swap_total: None,
            swap_used: None,
            commit: Some(pages(perf.CommitTotal)),
            commit_limit: Some(pages(perf.CommitLimit)),
            pressure: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        sync::Arc,
    };

    use super::*;

    #[test]
    fn filetimes_count_from_1601() {
        assert_eq!(system_time(UNIX_EPOCH_100NS), UNIX_EPOCH);
        assert_eq!(
            system_time(UNIX_EPOCH_100NS + 15_000_000),
            UNIX_EPOCH + Duration::from_millis(1500)
        );
        let time = FILETIME {
            dwLowDateTime: 7,
            dwHighDateTime: 1,
        };
        assert_eq!(units(time), (1 << 32) + 7);
    }

    #[test]
    fn reads_a_process_in_its_job_until_the_job_closes() {
        let mut child = Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .expect("ping starts");
        let job = Arc::new(Job::new(true).expect("a job"));
        job.assign(child.as_raw_handle())
            .expect("ping joins the job");
        let probe = TreeProbe {
            run: 1,
            pid: child.id(),
            spawned_at: SystemTime::now(),
            spawned: Instant::now(),
            job: Arc::downgrade(&job),
        };
        let reading = WindowsReader.tree(&probe, &[]).expect("a reading");
        assert_eq!(reading.members.len(), 1, "{reading:?}");
        let member = &reading.members[0];
        assert_eq!(member.id.pid, child.id());
        assert!(member.name.eq_ignore_ascii_case("ping.exe"), "{member:?}");
        assert_eq!((member.ppid, member.threads), (0, None));
        assert!(member.memory.unwrap_or(0) > 0, "{member:?}");
        assert!(member.peak >= member.memory, "{member:?}");
        assert!(member.committed.unwrap_or(0) > 0, "{member:?}");
        assert!(member.descriptors.unwrap_or(0) > 0, "{member:?}");
        assert!(member.read.is_some(), "{member:?}");
        assert_eq!(reading.denied, Denied::default());
        let totals = reading.job.expect("the job's totals");
        assert!(totals.cpu >= member.cpu_own, "{totals:?}, {member:?}");

        // Closing the job's only handle kills ping (KILL_ON_JOB_CLOSE), and the probe reads
        // nothing more.
        drop(job);
        let err = WindowsReader
            .tree(&probe, &[])
            .expect_err("the job is closed");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let closed = Instant::now();
        child.wait().expect("ping ends");
        assert!(
            closed.elapsed() < Duration::from_secs(10),
            "ping was killed, not left to finish"
        );
    }

    #[test]
    fn reads_the_computer() {
        let machine = WindowsReader.machine().expect("a reading");
        assert!(machine.logical_cpus >= 1);
        assert!(machine.memory_total > 0);
        assert!(machine.memory_available.unwrap_or(0) <= machine.memory_total);
        assert!(machine.cpu_busy + machine.cpu_idle > 0);
        let (commit, limit) = (machine.commit.unwrap(), machine.commit_limit.unwrap());
        assert!(commit > 0 && commit <= limit, "{commit} of {limit}");
        assert_eq!((machine.swap_total, machine.pressure), (None, None));
    }
}
