//! The macOS readers: libproc for a node's process tree, and Mach's host statistics and
//! `sysctlbyname` for the computer, through libc.
//!
//! A tree reading follows Linux's rules (docs/RESOURCES.md, "macOS: the same rules through
//! libproc"). The candidates are the root, its process group (`proc_listpgrppids`) and the
//! members seen before; a breadth-first walk of `proc_listchildpids` adds the children that
//! left the group. `proc_pidinfo` gives each candidate's parent, group and start time,
//! [`fold::select`] picks the members, and `proc_pid_rusage` reads each one in tree order: its
//! CPU time, the CPU time of the children it reaped, its memory as Activity Monitor shows it
//! (the physical footprint) and its disk I/O.
//!
//! `unsafe` is only around the libc and Mach calls. libc marks `mach_timebase_info` and
//! `mach_host_self` deprecated in favour of the mach2 crate, and does not bind
//! `host_page_size`, so the three are declared here.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::{CStr, c_int},
    io,
    mem::{MaybeUninit, size_of},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use launcher_types::MemoryPressure;

use super::{
    Denied, Identity, MachineReader, MachineReading, MemberReading, Reader, TreeReading,
    clean_name,
    fold::{self, ProcEntry},
};
use crate::process::TreeProbe;

/// How far the root's start may be from the spawn the supervisor saw: the supervisor reads its
/// clock just after the spawn.
const ROOT_START_TOLERANCE: Duration = Duration::from_secs(5);

/// How many more times a list that filled its buffer is grown and asked for again.
const LIST_RETRIES: usize = 3;

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::kern_return_t;
    safe fn mach_host_self() -> libc::mach_port_t;
    fn host_page_size(
        host: libc::mach_port_t,
        out_page_size: *mut libc::vm_size_t,
    ) -> libc::kern_return_t;
}

/// Reads process trees through libproc.
pub(super) struct MacReader {
    /// Mach ticks to nanoseconds, as a fraction: 125/3 on Apple silicon, 1/1 on Intel.
    numer: u64,
    denom: u64,
    /// An x86-64 build under Rosetta, where the timebase of the kernel's CPU times is unchecked:
    /// CPU is not read (docs/RESOURCES.md).
    translated: bool,
}

impl MacReader {
    pub(super) fn new() -> Self {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: `info` is a writable MachTimebaseInfo for the duration of the call.
        let kr = unsafe { mach_timebase_info(&raw mut info) };
        let (numer, denom) = if kr == libc::KERN_SUCCESS && info.numer > 0 && info.denom > 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        };
        MacReader {
            numer,
            denom,
            translated: sysctl::<c_int>(c"sysctl.proc_translated") == Some(1),
        }
    }

    /// Mach ticks in seconds; 0 when CPU is not read.
    fn secs(&self, ticks: u64) -> f64 {
        if self.translated {
            return 0.0;
        }
        ticks as f64 * self.numer as f64 / self.denom as f64 / 1e9
    }

    /// Reads one member. `None` when it is gone.
    fn read_member(&self, info: &Info, denied: &mut Denied) -> Option<MemberReading> {
        let pid = c_int::try_from(info.entry.pid).ok()?;
        let usage = match rusage(pid) {
            Ok(usage) => Some(usage),
            Err(err) if err.raw_os_error() == Some(libc::ESRCH) => return None,
            Err(_) => {
                denied.memory = true;
                denied.io = true;
                None
            }
        };
        // A zombie holds no memory, threads or files. A process that exited since its usage was
        // read stays a member of this reading, without a count.
        let descriptors = if info.zombie {
            Some(0)
        } else {
            match descriptors(pid) {
                Ok(count) => Some(count),
                Err(err) if err.raw_os_error() == Some(libc::ESRCH) => None,
                Err(_) => {
                    denied.descriptors = true;
                    None
                }
            }
        };
        let times = |user: u64, system: u64| self.secs(user.saturating_add(system));
        Some(MemberReading {
            id: info.id(),
            ppid: info.entry.ppid,
            name: info.name.clone(),
            started_at: info.started_at,
            cpu_own: usage.map_or(0.0, |u| times(u.ri_user_time, u.ri_system_time)),
            cpu_rollup: usage.map_or(0.0, |u| times(u.ri_child_user_time, u.ri_child_system_time)),
            memory: usage.map(|u| if info.zombie { 0 } else { u.ri_phys_footprint }),
            peak: usage.map(|u| u.ri_lifetime_max_phys_footprint),
            committed: None,
            read: usage.map(|u| u.ri_diskio_bytesread),
            written: usage.map(|u| u.ri_diskio_byteswritten),
            threads: info.threads,
            descriptors,
        })
    }
}

/// A process as `proc_pidinfo` describes it.
struct Info {
    entry: ProcEntry,
    name: String,
    started_at: SystemTime,
    /// `None` when the kernel gave no task information, and 0 for a zombie.
    threads: Option<u32>,
    zombie: bool,
}

impl Info {
    fn new(bsd: &libc::proc_bsdinfo, threads: Option<u32>) -> Self {
        let zombie = bsd.pbi_status == libc::SZOMB;
        // `pbi_name` is the longer name, empty for some processes; `pbi_comm` has 16 bytes.
        let name = if bsd.pbi_name[0] != 0 {
            c_chars(&bsd.pbi_name)
        } else {
            c_chars(&bsd.pbi_comm)
        };
        let start = bsd
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(bsd.pbi_start_tvusec);
        Info {
            entry: ProcEntry {
                pid: bsd.pbi_pid,
                ppid: bsd.pbi_ppid,
                pgrp: bsd.pbi_pgid,
                start,
            },
            name: clean_name(&name),
            started_at: UNIX_EPOCH + Duration::from_micros(start),
            threads: if zombie { Some(0) } else { threads },
            zombie,
        }
    }

    fn id(&self) -> Identity {
        Identity {
            pid: self.entry.pid,
            start: self.entry.start,
        }
    }
}

/// The bytes of a C string in a fixed array, up to its first NUL.
fn c_chars(chars: &[libc::c_char]) -> Vec<u8> {
    chars
        .iter()
        .take_while(|&&c| c != 0)
        .flat_map(|c| c.to_ne_bytes())
        .collect()
}

/// What `proc_pidinfo` says of `pid`: `PROC_PIDTASKALLINFO`, or for a zombie, which that call
/// refuses with ESRCH, `PROC_PIDTBSDINFO` with `arg` 1, which XNU answers for zombies. `None`
/// when the process is gone or is not ours to read.
fn info(pid: u32) -> Option<Info> {
    let c_pid = c_int::try_from(pid).ok()?;
    let size = c_int::try_from(size_of::<libc::proc_taskallinfo>()).ok()?;
    let mut all = MaybeUninit::<libc::proc_taskallinfo>::zeroed();
    // SAFETY: `all` is writable for `size` bytes for the duration of the call.
    let filled = unsafe {
        libc::proc_pidinfo(
            c_pid,
            libc::PROC_PIDTASKALLINFO,
            0,
            all.as_mut_ptr().cast(),
            size,
        )
    };
    if filled == size {
        // SAFETY: the call filled the whole struct, which holds only integers and arrays of
        // them, so any bytes are valid.
        let all = unsafe { all.assume_init() };
        return Some(Info::new(
            &all.pbsd,
            u32::try_from(all.ptinfo.pti_threadnum).ok(),
        ));
    }
    let size = c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut bsd = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `bsd` is writable for `size` bytes for the duration of the call; `arg` 1 asks for
    // a zombie's information too.
    let filled = unsafe {
        libc::proc_pidinfo(
            c_pid,
            libc::PROC_PIDTBSDINFO,
            1,
            bsd.as_mut_ptr().cast(),
            size,
        )
    };
    // SAFETY: the call filled the whole struct, which holds only integers and arrays of them.
    (filled == size).then(|| Info::new(&unsafe { bsd.assume_init() }, None))
}

/// `proc_pid_rusage(RUSAGE_INFO_V4)`, which also answers for a zombie.
fn rusage(pid: c_int) -> io::Result<libc::rusage_info_v4> {
    let mut usage = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `usage` is a writable rusage_info_v4, the struct RUSAGE_INFO_V4 fills.
    let rc = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, usage.as_mut_ptr().cast()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call filled the struct, which holds only integers and arrays of them.
    Ok(unsafe { usage.assume_init() })
}

/// The open files of `pid`: the bytes `PROC_PIDLISTFDS` fills ÷ the size of one entry.
fn descriptors(pid: c_int) -> io::Result<u32> {
    let entry = size_of::<libc::proc_fdinfo>();
    // SAFETY: a null buffer asks for the size the list needs now; nothing is written.
    let need =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    let need = usize::try_from(need)
        .ok()
        .filter(|&need| need > 0)
        .ok_or_else(io::Error::last_os_error)?;
    let mut capacity = need / entry + 16;
    for _ in 0..=LIST_RETRIES {
        let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(capacity);
        let bytes = c_int::try_from(capacity * entry).map_err(io::Error::other)?;
        // SAFETY: `fds` has room for `bytes` bytes for the duration of the call; what the call
        // writes is never read.
        let filled = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                bytes,
            )
        };
        // 0 is also the answer for a process with no open files, which the size check above
        // found readable.
        let count = usize::try_from(filled).unwrap_or(0) / entry;
        if count < capacity {
            return Ok(u32::try_from(count).unwrap_or(u32::MAX));
        }
        capacity *= 2;
    }
    Err(io::Error::other("the list of open files kept growing"))
}

/// A libproc call that lists pids: `proc_listpgrppids` or `proc_listchildpids`.
type ListPids = unsafe extern "C" fn(libc::pid_t, *mut libc::c_void, c_int) -> c_int;

/// The pids `list` gives for `of`. Both list calls return a count of pids while taking the
/// buffer's size in bytes (`libproc.c`), so the buffer grows and the call repeats while the
/// count fills it.
fn list_pids(list: ListPids, of: u32) -> io::Result<Vec<u32>> {
    let of = libc::pid_t::try_from(of).map_err(io::Error::other)?;
    let mut capacity = 64;
    for _ in 0..=LIST_RETRIES {
        let mut pids: Vec<libc::pid_t> = vec![0; capacity];
        let bytes =
            c_int::try_from(capacity * size_of::<libc::pid_t>()).map_err(io::Error::other)?;
        // SAFETY: `pids` is writable for `bytes` bytes for the duration of the call.
        let count = unsafe { list(of, pids.as_mut_ptr().cast(), bytes) };
        let count = usize::try_from(count).map_err(|_| io::Error::last_os_error())?;
        if count < capacity {
            pids.truncate(count);
            return Ok(pids
                .into_iter()
                .filter_map(|pid| u32::try_from(pid).ok())
                .filter(|&pid| pid != 0)
                .collect());
        }
        capacity *= 4;
    }
    Err(io::Error::other("the process list kept growing"))
}

/// The root on the run's first reading: the process with the probe's pid, if it started
/// within [`ROOT_START_TOLERANCE`] of the spawn. A pid already reused by then fails it.
fn root_at_spawn(infos: &HashMap<u32, Info>, probe: &TreeProbe) -> Option<Identity> {
    let info = infos.get(&probe.pid)?;
    let skew = match info.started_at.duration_since(probe.spawned_at) {
        Ok(after) => after,
        Err(before) => before.duration(),
    };
    (skew <= ROOT_START_TOLERANCE).then(|| info.id())
}

impl Reader for MacReader {
    fn tree(&self, probe: &TreeProbe, sticky: &[Identity]) -> io::Result<TreeReading> {
        // The candidates, then a walk down their parent links. A child that started before
        // the process it hangs off is under a reused pid, so its children are not walked.
        let mut queued = HashSet::new();
        let mut queue: VecDeque<(u32, Option<u64>)> = VecDeque::new();
        let group = list_pids(libc::proc_listpgrppids, probe.pid)?;
        for pid in std::iter::once(probe.pid)
            .chain(group)
            .chain(sticky.iter().map(|id| id.pid))
        {
            if queued.insert(pid) {
                queue.push_back((pid, None));
            }
        }
        let mut infos = HashMap::new();
        while let Some((pid, parent_start)) = queue.pop_front() {
            let Some(info) = info(pid) else {
                continue;
            };
            if parent_start.is_none_or(|start| info.entry.start >= start) {
                // A process that has exited since it was listed has no children to list.
                for child in list_pids(libc::proc_listchildpids, pid).unwrap_or_default() {
                    if queued.insert(child) {
                        queue.push_back((child, Some(info.entry.start)));
                    }
                }
            }
            infos.insert(pid, info);
        }
        let entries: Vec<ProcEntry> = infos.values().map(|info| info.entry).collect();
        let root = sticky
            .iter()
            .find(|id| id.pid == probe.pid)
            .copied()
            .or_else(|| root_at_spawn(&infos, probe));
        let ids = fold::select(&entries, probe.pid, root, sticky);
        let at = SystemTime::now();
        let mono = Instant::now();
        let mut denied = Denied {
            cpu: self.translated,
            ..Denied::default()
        };
        let members = ids
            .iter()
            .filter_map(|id| infos.get(&id.pid))
            .filter_map(|info| self.read_member(info, &mut denied))
            .collect();
        Ok(TreeReading {
            at,
            mono,
            members,
            denied,
            job: None,
        })
    }
}

/// Reads the computer through Mach's host statistics and sysctls.
pub(super) struct MacMachineReader {
    /// The host port, taken once: each `mach_host_self` adds a send right.
    host: libc::mach_port_t,
    /// The kernel's page size, the unit of the VM statistics: 16 KiB on Apple silicon.
    page_size: Option<u64>,
}

impl MacMachineReader {
    pub(super) fn new() -> Self {
        let host = mach_host_self();
        let mut page_size: libc::vm_size_t = 0;
        // SAFETY: `host` is a send right to the host port, and `page_size` is writable for the
        // duration of the call.
        let kr = unsafe { host_page_size(host, &raw mut page_size) };
        MacMachineReader {
            host,
            page_size: (kr == libc::KERN_SUCCESS)
                .then(|| u64::try_from(page_size).ok())
                .flatten()
                .filter(|&size| size > 0),
        }
    }

    /// The memory in use as Activity Monitor counts it, in pages: app memory (internal pages
    /// that are not purgeable), wired memory and the compressor's pages.
    fn used_pages(&self) -> Option<u64> {
        let mut vm = MaybeUninit::<libc::vm_statistics64>::zeroed();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `host` is a send right to the host port; `vm` is writable for `count`
        // integers, the size HOST_VM_INFO64 fills, and `count` is writable.
        let kr = unsafe {
            libc::host_statistics64(
                self.host,
                libc::HOST_VM_INFO64,
                vm.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: the struct holds only integers, so the zeroed bytes the call did not fill
        // are valid too.
        let vm = unsafe { vm.assume_init() };
        let (internal, purgeable) = (vm.internal_page_count, vm.purgeable_count);
        let (wired, compressor) = (vm.wire_count, vm.compressor_page_count);
        Some(
            u64::from(internal)
                .saturating_sub(u64::from(purgeable))
                .saturating_add(u64::from(wired))
                .saturating_add(u64::from(compressor)),
        )
    }
}

impl MachineReader for MacMachineReader {
    fn machine(&self) -> io::Result<MachineReading> {
        let at = SystemTime::now();
        let mut load = MaybeUninit::<libc::host_cpu_load_info>::zeroed();
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        // SAFETY: `host` is a send right to the host port; `load` is writable for `count`
        // integers, the size HOST_CPU_LOAD_INFO fills, and `count` is writable.
        let kr = unsafe {
            libc::host_statistics(
                self.host,
                libc::HOST_CPU_LOAD_INFO,
                load.as_mut_ptr().cast(),
                &raw mut count,
            )
        };
        if kr != libc::KERN_SUCCESS {
            return Err(io::Error::other(format!(
                "host_statistics(HOST_CPU_LOAD_INFO) failed with {kr}"
            )));
        }
        // SAFETY: the call filled the struct, an array of integers.
        let load = unsafe { load.assume_init() };
        // Ticks of all CPUs, in CPU_STATE_* order. They are 32-bit and wrap after weeks; the
        // sampler then gives no busy figure for one interval.
        let [user, system, idle, nice] = load.cpu_ticks.map(u64::from);
        let memory_total = sysctl::<u64>(c"hw.memsize")
            .ok_or_else(|| io::Error::other("hw.memsize cannot be read"))?;
        let memory_available = self
            .used_pages()
            .zip(self.page_size)
            .map(|(pages, size)| memory_total.saturating_sub(pages.saturating_mul(size)));
        let swap = sysctl::<libc::xsw_usage>(c"vm.swapusage");
        // SAFETY: sysconf takes no pointers.
        let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
        Ok(MachineReading {
            at,
            logical_cpus: u32::try_from(cpus)
                .ok()
                .filter(|&cpus| cpus > 0)
                .unwrap_or(1),
            cpu_busy: user + system + nice,
            cpu_idle: idle,
            memory_total,
            memory_available,
            swap_total: swap.map(|swap| swap.xsu_total),
            swap_used: swap.map(|swap| swap.xsu_used),
            commit: None,
            commit_limit: None,
            pressure: sysctl::<c_int>(c"kern.memorystatus_vm_pressure_level")
                .and_then(pressure_level),
        })
    }
}

/// `kern.memorystatus_vm_pressure_level`: 1 normal, 2 warning, 4 critical.
fn pressure_level(level: c_int) -> Option<MemoryPressure> {
    match level {
        1 => Some(MemoryPressure::Normal),
        2 => Some(MemoryPressure::Warning),
        4 => Some(MemoryPressure::Critical),
        _ => None,
    }
}

/// A C value that any bytes make valid: the integers and structs of integers [`sysctl`] reads.
trait Plain: Copy {}

impl Plain for c_int {}
impl Plain for u64 {}
impl Plain for libc::xsw_usage {}

/// The sysctl `name`. `None` when it does not exist or has another size.
fn sysctl<T: Plain>(name: &CStr) -> Option<T> {
    let mut value = MaybeUninit::<T>::zeroed();
    let mut len = size_of::<T>();
    // SAFETY: `name` is NUL-terminated, `value` is writable for `len` bytes and `len` is
    // writable; no new value is set.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // SAFETY: the call filled `value`, and any bytes are a valid `T` (`Plain`).
    (rc == 0 && len == size_of::<T>()).then(|| unsafe { value.assume_init() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Info {
        info(std::process::id()).expect("this process")
    }

    fn probe(pid: u32, spawned_at: SystemTime) -> TreeProbe {
        TreeProbe {
            run: 1,
            pid,
            spawned_at,
            spawned: Instant::now(),
        }
    }

    #[test]
    fn reads_this_process() {
        let reader = MacReader::new();
        let me = me().id();
        let reading = reader
            .tree(&probe(me.pid, SystemTime::now()), &[me])
            .expect("a reading");
        let member = reading
            .members
            .iter()
            .find(|member| member.id == me)
            .expect("this process is a member");
        assert!(member.threads.unwrap_or(0) >= 1, "{member:?}");
        assert!(member.memory.unwrap_or(0) > 0, "{member:?}");
        assert!(member.peak >= member.memory, "{member:?}");
        assert!(member.descriptors.unwrap_or(0) > 0, "{member:?}");
        assert!(member.read.is_some(), "{member:?}");
        assert!(!member.name.is_empty());
        assert_eq!(
            reading.denied,
            Denied {
                cpu: reader.translated,
                ..Denied::default()
            }
        );
        if !reader.translated {
            assert!(member.cpu_own > 0.0, "{member:?}");
        }
        let age = SystemTime::now()
            .duration_since(member.started_at)
            .expect("started in the past");
        assert!(age < Duration::from_secs(24 * 3600), "{age:?}");
    }

    #[test]
    fn a_child_is_found_by_its_parent_link_also_as_a_zombie() {
        let reader = MacReader::new();
        let me = me().id();
        let probe = probe(me.pid, SystemTime::now());
        let member_of = |pid: u32| {
            reader
                .tree(&probe, &[me])
                .expect("a reading")
                .members
                .into_iter()
                .find(|member| member.id.pid == pid)
        };
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("sleep starts");
        let pid = child.id();
        let live = member_of(pid).expect("the child is a member");
        assert_eq!(live.ppid, me.pid);
        assert!(live.threads.unwrap_or(0) >= 1, "{live:?}");

        // Killed and not yet reaped: a zombie, still a member, with nothing left to hold.
        child.kill().expect("kill the child");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !info(pid).is_some_and(|info| info.zombie) {
            assert!(Instant::now() < deadline, "the child became a zombie");
            std::thread::sleep(Duration::from_millis(10));
        }
        let zombie = member_of(pid).expect("the zombie is still a member");
        assert_eq!(zombie.id, live.id, "the same identity");
        assert_eq!(zombie.memory, Some(0));
        assert_eq!((zombie.threads, zombie.descriptors), (Some(0), Some(0)));

        child.wait().expect("reap the child");
        assert_eq!(member_of(pid), None, "reaped");
    }

    #[test]
    fn the_root_must_have_started_at_the_spawn() {
        let me = me();
        let id = me.id();
        let started_at = me.started_at;
        let infos = HashMap::from([(id.pid, me)]);
        assert_eq!(
            root_at_spawn(&infos, &probe(id.pid, started_at + Duration::from_secs(1))),
            Some(id)
        );
        // A spawn a day later: the process with that pid now is not the one Zeal started.
        assert_eq!(
            root_at_spawn(
                &infos,
                &probe(id.pid, started_at + Duration::from_secs(86_400))
            ),
            None
        );
        assert_eq!(
            root_at_spawn(&HashMap::new(), &probe(id.pid, started_at)),
            None
        );
    }

    #[test]
    fn ticks_become_seconds_by_the_timebase() {
        let apple_silicon = MacReader {
            numer: 125,
            denom: 3,
            translated: false,
        };
        assert!((apple_silicon.secs(24_000_000) - 1.0).abs() < 1e-9);
        let rosetta = MacReader {
            translated: true,
            ..apple_silicon
        };
        assert_eq!(rosetta.secs(24_000_000), 0.0);
    }

    #[test]
    fn reads_the_computer() {
        let machine = MacMachineReader::new().machine().expect("a reading");
        assert!(machine.logical_cpus >= 1);
        assert!(machine.memory_total > 0);
        let available = machine.memory_available.expect("an estimate");
        assert!(available <= machine.memory_total);
        assert!(machine.cpu_busy + machine.cpu_idle > 0);
        assert!(machine.swap_total.is_some());
        assert!(machine.pressure.is_some(), "any process may read the level");
        assert_eq!((machine.commit, machine.commit_limit), (None, None));
    }

    #[test]
    fn pressure_levels_are_the_kernels() {
        assert_eq!(pressure_level(1), Some(MemoryPressure::Normal));
        assert_eq!(pressure_level(2), Some(MemoryPressure::Warning));
        assert_eq!(pressure_level(4), Some(MemoryPressure::Critical));
        assert_eq!(pressure_level(0), None);
    }
}
