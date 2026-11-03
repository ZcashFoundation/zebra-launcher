//! The Linux readers: `/proc` through `std::fs`, and nix's `sysconf` for the clock tick and
//! the CPU count. No `unsafe`.
//!
//! A tree reading scans every `/proc/<pid>/stat` for the pid, parent, group and start time,
//! picks the members ([`fold::select`]), then reads each member's `stat`, `status`, `io` and
//! `fd` directory in tree order. Reading a parent before its children means a reap between the
//! two reads can hide the child's time for one reading, which the fold absorbs, but never
//! counts it twice.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use nix::{
    errno::Errno,
    unistd::{SysconfVar, sysconf},
};

use super::{
    Denied, Identity, MachineReader, MachineReading, MemberReading, Reader, TreeReading,
    clean_name,
    fold::{self, ProcEntry},
    procfs,
};
use crate::process::TreeProbe;

/// How far the root's start may be from the spawn the supervisor saw: `btime` is in whole
/// seconds, and the supervisor reads its clock just after the spawn.
const ROOT_START_TOLERANCE: Duration = Duration::from_secs(5);

/// Reads process trees and the computer from `/proc`.
pub(super) struct LinuxReader {
    /// USER_HZ: the unit of the times in `stat`, 100 on the usual kernels.
    ticks_per_sec: u64,
}

impl LinuxReader {
    pub(super) fn new() -> Self {
        let ticks_per_sec = sysconf(SysconfVar::CLK_TCK)
            .ok()
            .flatten()
            .and_then(|ticks| u64::try_from(ticks).ok())
            .filter(|&ticks| ticks > 0)
            .unwrap_or(100);
        LinuxReader { ticks_per_sec }
    }

    fn secs(&self, ticks: u64) -> f64 {
        ticks as f64 / self.ticks_per_sec as f64
    }

    fn since_boot(&self, ticks: u64) -> Duration {
        let whole = ticks / self.ticks_per_sec;
        let part = ticks % self.ticks_per_sec;
        Duration::from_secs(whole) + Duration::from_nanos(part * 1_000_000_000 / self.ticks_per_sec)
    }

    /// The root on the run's first reading: the process with the probe's pid, if it started
    /// within [`ROOT_START_TOLERANCE`] of the spawn. A pid already reused by then fails it.
    fn root_at_spawn(
        &self,
        entries: &[ProcEntry],
        probe: &TreeProbe,
        btime: u64,
    ) -> Option<Identity> {
        let entry = entries.iter().find(|entry| entry.pid == probe.pid)?;
        let started = UNIX_EPOCH + Duration::from_secs(btime) + self.since_boot(entry.start);
        let skew = match started.duration_since(probe.spawned_at) {
            Ok(after) => after,
            Err(before) => before.duration(),
        };
        (skew <= ROOT_START_TOLERANCE).then_some(Identity {
            pid: entry.pid,
            start: entry.start,
        })
    }

    /// Reads one member. `None` when it is gone, or its pid names another process now.
    fn read_member(
        &self,
        id: Identity,
        btime: u64,
        buf: &mut Vec<u8>,
        denied: &mut Denied,
    ) -> io::Result<Option<MemberReading>> {
        let dir = PathBuf::from(format!("/proc/{}", id.pid));
        match read_into(&dir.join("stat"), buf) {
            Ok(()) => {}
            Err(err) if gone(&err) => return Ok(None),
            Err(err) => return Err(err),
        }
        let stat = procfs::parse_stat(buf).ok_or_else(|| malformed(&dir, "stat"))?;
        if stat.starttime != id.start {
            return Ok(None);
        }
        let (name, memory, peak) = match read_into(&dir.join("status"), buf) {
            // A zombie or a process that is exiting has no memory lines: it holds none.
            Ok(()) => {
                let status = procfs::parse_status(buf);
                (status.name, Some(status.rss.unwrap_or(0)), status.hwm)
            }
            Err(err) if gone(&err) => return Ok(None),
            Err(err) if refused(&err) => {
                denied.memory = true;
                (None, None, None)
            }
            Err(err) => return Err(err),
        };
        // Allowed only for the same user's dumpable processes.
        let io = match read_into(&dir.join("io"), buf) {
            Ok(()) => Some(procfs::parse_io(buf).ok_or_else(|| malformed(&dir, "io"))?),
            Err(err) if gone(&err) => return Ok(None),
            Err(err) if refused(&err) => {
                denied.io = true;
                None
            }
            Err(err) => return Err(err),
        };
        let descriptors = match fs::read_dir(dir.join("fd")) {
            Ok(entries) => Some(u32::try_from(entries.count()).unwrap_or(u32::MAX)),
            Err(err) if gone(&err) => return Ok(None),
            Err(err) if refused(&err) => {
                denied.descriptors = true;
                None
            }
            Err(err) => return Err(err),
        };
        let name = clean_name(name.as_deref().unwrap_or(&stat.comm));
        Ok(Some(MemberReading {
            id,
            ppid: stat.ppid,
            name,
            started_at: UNIX_EPOCH + Duration::from_secs(btime) + self.since_boot(stat.starttime),
            cpu_own: self.secs(stat.utime + stat.stime),
            cpu_rollup: self.secs(stat.cutime + stat.cstime),
            memory,
            peak,
            read: io.map(|io| io.read_bytes),
            written: io.map(|io| io.write_bytes),
            threads: Some(stat.threads),
            descriptors,
        }))
    }
}

/// Reads `path` into `buf`, replacing what it held.
fn read_into(path: &Path, buf: &mut Vec<u8>) -> io::Result<()> {
    buf.clear();
    fs::File::open(path)?.read_to_end(buf)?;
    Ok(())
}

/// The process exited between the scan and the read.
fn gone(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::NotFound || err.raw_os_error() == Some(Errno::ESRCH as i32)
}

/// EACCES or EPERM: the field is not ours to read.
fn refused(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::PermissionDenied
}

fn malformed(dir: &Path, file: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}/{file} is not in the expected format", dir.display()),
    )
}

/// The boot time, from `/proc/stat`.
fn btime(buf: &mut Vec<u8>) -> io::Result<u64> {
    read_into(Path::new("/proc/stat"), buf)?;
    procfs::parse_btime(buf).ok_or_else(|| malformed(Path::new("/proc"), "stat"))
}

/// Every process's pid, parent, group and start time. A process that exits during the scan,
/// or whose `stat` does not parse, is left out.
fn scan(buf: &mut Vec<u8>) -> io::Result<Vec<ProcEntry>> {
    let mut entries = Vec::new();
    for dirent in fs::read_dir("/proc")? {
        let Ok(dirent) = dirent else {
            continue;
        };
        let is_pid = dirent
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()));
        if !is_pid || read_into(&dirent.path().join("stat"), buf).is_err() {
            continue;
        }
        if let Some(stat) = procfs::parse_stat(buf) {
            entries.push(ProcEntry {
                pid: stat.pid,
                ppid: stat.ppid,
                pgrp: stat.pgrp,
                start: stat.starttime,
            });
        }
    }
    Ok(entries)
}

impl Reader for LinuxReader {
    fn tree(&self, probe: &TreeProbe, sticky: &[Identity]) -> io::Result<TreeReading> {
        let mut buf = Vec::with_capacity(4096);
        let entries = scan(&mut buf)?;
        let btime = btime(&mut buf)?;
        let root = sticky
            .iter()
            .find(|id| id.pid == probe.pid)
            .copied()
            .or_else(|| self.root_at_spawn(&entries, probe, btime));
        let ids = fold::select(&entries, probe.pid, root, sticky);
        let at = SystemTime::now();
        let mono = Instant::now();
        let mut denied = Denied::default();
        let mut members = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(member) = self.read_member(id, btime, &mut buf, &mut denied)? {
                members.push(member);
            }
        }
        Ok(TreeReading {
            at,
            mono,
            members,
            denied,
        })
    }
}

impl MachineReader for LinuxReader {
    fn machine(&self) -> io::Result<MachineReading> {
        let mut buf = Vec::with_capacity(8192);
        read_into(Path::new("/proc/stat"), &mut buf)?;
        let cpu =
            procfs::parse_cpu_times(&buf).ok_or_else(|| malformed(Path::new("/proc"), "stat"))?;
        read_into(Path::new("/proc/meminfo"), &mut buf)?;
        let memory =
            procfs::parse_meminfo(&buf).ok_or_else(|| malformed(Path::new("/proc"), "meminfo"))?;
        let logical_cpus = sysconf(SysconfVar::_NPROCESSORS_ONLN)
            .ok()
            .flatten()
            .and_then(|cpus| u32::try_from(cpus).ok())
            .filter(|&cpus| cpus > 0)
            .unwrap_or(1);
        Ok(MachineReading {
            at: SystemTime::now(),
            logical_cpus,
            cpu_busy: cpu.busy,
            cpu_idle: cpu.idle,
            memory_total: memory.total,
            memory_available: memory.available,
            swap_total: memory.swap_total,
            swap_used: memory
                .swap_total
                .zip(memory.swap_free)
                .map(|(total, free)| total.saturating_sub(free)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test process's identity.
    fn me() -> Identity {
        let mut buf = Vec::new();
        read_into(Path::new("/proc/self/stat"), &mut buf).expect("own stat");
        let stat = procfs::parse_stat(&buf).expect("parsed");
        Identity {
            pid: stat.pid,
            start: stat.starttime,
        }
    }

    #[test]
    fn reads_this_process() {
        let reader = LinuxReader::new();
        let me = me();
        let probe = TreeProbe {
            run: 1,
            pid: me.pid,
            spawned_at: SystemTime::now(),
            spawned: Instant::now(),
        };
        let reading = reader.tree(&probe, &[me]).expect("a reading");
        let member = reading
            .members
            .iter()
            .find(|member| member.id == me)
            .expect("this process is a member");
        assert!(member.threads.unwrap_or(0) >= 1, "{member:?}");
        assert!(member.memory.unwrap_or(0) > 0, "{member:?}");
        assert!(member.peak >= member.memory, "{member:?}");
        assert!(member.descriptors.unwrap_or(0) > 0, "{member:?}");
        assert!(member.read.is_some(), "our own io is readable: {member:?}");
        assert!(!member.name.is_empty());
        assert_eq!(reading.denied, Denied::default());
        let age = SystemTime::now()
            .duration_since(member.started_at)
            .expect("started in the past");
        assert!(age < Duration::from_secs(24 * 3600), "{age:?}");
    }

    #[test]
    fn the_root_must_have_started_at_the_spawn() {
        let reader = LinuxReader::new();
        let me = me();
        let btime = btime(&mut Vec::new()).expect("btime");
        let entries = [ProcEntry {
            pid: me.pid,
            ppid: 1,
            pgrp: 1,
            start: me.start,
        }];
        let spawned_at = UNIX_EPOCH + Duration::from_secs(btime) + reader.since_boot(me.start);
        let probe = |spawned_at| TreeProbe {
            run: 1,
            pid: me.pid,
            spawned_at,
            spawned: Instant::now(),
        };
        assert_eq!(
            reader.root_at_spawn(&entries, &probe(spawned_at + Duration::from_secs(1)), btime),
            Some(me)
        );
        // A spawn a day later: the process with that pid now is not the one Zeal started.
        assert_eq!(
            reader.root_at_spawn(
                &entries,
                &probe(spawned_at + Duration::from_secs(86_400)),
                btime
            ),
            None
        );
        assert_eq!(reader.root_at_spawn(&[], &probe(spawned_at), btime), None);
    }

    #[test]
    fn reads_the_computer() {
        let machine = LinuxReader::new().machine().expect("a reading");
        assert!(machine.logical_cpus >= 1);
        assert!(machine.memory_total > 0);
        assert!(machine.memory_available.unwrap_or(0) <= machine.memory_total);
        assert!(machine.cpu_busy + machine.cpu_idle > 0);
    }
}
