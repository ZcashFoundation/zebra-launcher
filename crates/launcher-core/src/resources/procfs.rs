//! Parsers of the Linux `/proc` files the resource reader uses. Every file is read as bytes,
//! so a process name with any bytes in it (a name cut mid-character at 15 bytes, a tab, a
//! newline) never fails a read. Pure, compiled and tested on every OS.

/// The fields of `/proc/<pid>/stat` the reader uses. Times are in clock ticks (USER_HZ).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stat {
    pub pid: u32,
    /// The name between the parentheses, raw: up to 15 bytes, any of them.
    pub comm: Vec<u8>,
    /// The state letter: `R`, `S`, `Z` for a zombie, and so on.
    pub state: u8,
    pub ppid: u32,
    pub pgrp: u32,
    /// User and system time of the process's threads, live and exited (fields 14 and 15).
    pub utime: u64,
    pub stime: u64,
    /// User and system time of the children it waited for (fields 16 and 17).
    pub cutime: u64,
    pub cstime: u64,
    /// Field 20.
    pub threads: u32,
    /// Ticks since boot (field 22).
    pub starttime: u64,
}

/// Parses `/proc/<pid>/stat`. The fields after the name are read after the last `)`, since
/// the name itself may hold spaces and parentheses. `None` for text that is not a stat line.
pub fn parse_stat(bytes: &[u8]) -> Option<Stat> {
    let open = bytes.iter().position(|&b| b == b'(')?;
    let close = bytes.iter().rposition(|&b| b == b')')?;
    if close < open {
        return None;
    }
    let pid = std::str::from_utf8(&bytes[..open])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let comm = bytes[open + 1..close].to_vec();
    let rest = std::str::from_utf8(&bytes[close + 1..]).ok()?;
    // Field n (1-based) of the line is field n - 3 here: the state is field 3.
    let fields: Vec<&str> = rest.split_ascii_whitespace().collect();
    let field = |n: usize| fields.get(n - 3).copied();
    let number = |n: usize| field(n)?.parse::<u64>().ok();
    // cutime and cstime are signed in the kernel (`%ld`); never negative in practice.
    let signed = |n: usize| {
        field(n)?
            .parse::<i64>()
            .ok()
            .map(|value| value.max(0).unsigned_abs())
    };
    let state = *field(3)?.as_bytes().first()?;
    Some(Stat {
        pid,
        comm,
        state,
        ppid: u32::try_from(number(4)?).ok()?,
        pgrp: u32::try_from(number(5)?).ok()?,
        utime: number(14)?,
        stime: number(15)?,
        cutime: signed(16)?,
        cstime: signed(17)?,
        threads: u32::try_from(number(20)?).ok()?,
        starttime: number(22)?,
    })
}

/// What `/proc/<pid>/status` says about a process's name and memory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// The `Name:` line, raw. The kernel escapes it, so it is one line.
    pub name: Option<Vec<u8>>,
    /// `VmRSS`, in bytes. Absent for a zombie and a kernel thread.
    pub rss: Option<u64>,
    /// `VmHWM`, the peak of `VmRSS`, in bytes.
    pub hwm: Option<u64>,
}

/// A `key:   1234 kB` value, in bytes.
fn kilobytes(value: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(value).ok()?.trim();
    let number = text.strip_suffix("kB").unwrap_or(text).trim();
    number.parse::<u64>().ok()?.checked_mul(1024)
}

/// Splits a `key: value` line.
fn key_value(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let colon = line.iter().position(|&b| b == b':')?;
    Some((&line[..colon], &line[colon + 1..]))
}

/// Parses `/proc/<pid>/status`; lines it does not know are skipped.
pub fn parse_status(bytes: &[u8]) -> Status {
    let mut status = Status::default();
    for line in bytes.split(|&b| b == b'\n') {
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        match key {
            b"Name" => {
                let value = value.strip_prefix(b"\t").unwrap_or(value);
                status.name = Some(value.to_vec());
            }
            b"VmRSS" => status.rss = kilobytes(value),
            b"VmHWM" => status.hwm = kilobytes(value),
            _ => {}
        }
    }
    status
}

/// What `/proc/<pid>/io` says the process read from and wrote to storage, its children
/// that it waited for included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Io {
    pub read_bytes: u64,
    pub write_bytes: u64,
}

/// Parses `/proc/<pid>/io`: its `read_bytes` and `write_bytes`. `rchar` and `wchar` count
/// every read and write call, sockets and pipes included, so they are not used.
pub fn parse_io(bytes: &[u8]) -> Option<Io> {
    let mut read = None;
    let mut write = None;
    for line in bytes.split(|&b| b == b'\n') {
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        let number = || std::str::from_utf8(value).ok()?.trim().parse::<u64>().ok();
        match key {
            b"read_bytes" => read = number(),
            b"write_bytes" => write = number(),
            _ => {}
        }
    }
    Some(Io {
        read_bytes: read?,
        write_bytes: write?,
    })
}

/// The computer's CPU time from the first line of `/proc/stat`, in clock ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuTimes {
    /// user + nice + system + irq + softirq + steal. Guest time is already in user and nice.
    pub busy: u64,
    /// idle + iowait.
    pub idle: u64,
}

/// Parses the `cpu` line of `/proc/stat`. Fields an older kernel does not print count as 0.
pub fn parse_cpu_times(bytes: &[u8]) -> Option<CpuTimes> {
    let line = bytes.split(|&b| b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut fields = line.split_ascii_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields
        .map(|field| field.parse().ok())
        .collect::<Option<_>>()?;
    if values.len() < 4 {
        return None;
    }
    let at = |i: usize| values.get(i).copied().unwrap_or(0);
    let (user, nice, system, idle, iowait, irq, softirq, steal) =
        (at(0), at(1), at(2), at(3), at(4), at(5), at(6), at(7));
    Some(CpuTimes {
        busy: user + nice + system + irq + softirq + steal,
        idle: idle + iowait,
    })
}

/// The boot time from `/proc/stat`'s `btime` line, in seconds since the epoch.
pub fn parse_btime(bytes: &[u8]) -> Option<u64> {
    bytes.split(|&b| b == b'\n').find_map(|line| {
        let rest = line.strip_prefix(b"btime ")?;
        std::str::from_utf8(rest).ok()?.trim().parse().ok()
    })
}

/// What `/proc/meminfo` says, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemInfo {
    pub total: u64,
    /// `MemAvailable`, since Linux 3.14.
    pub available: Option<u64>,
    pub swap_total: Option<u64>,
    pub swap_free: Option<u64>,
}

/// Parses `/proc/meminfo`. `None` without a `MemTotal` line.
pub fn parse_meminfo(bytes: &[u8]) -> Option<MemInfo> {
    let mut total = None;
    let mut info = MemInfo {
        total: 0,
        available: None,
        swap_total: None,
        swap_free: None,
    };
    for line in bytes.split(|&b| b == b'\n') {
        let Some((key, value)) = key_value(line) else {
            continue;
        };
        match key {
            b"MemTotal" => total = kilobytes(value),
            b"MemAvailable" => info.available = kilobytes(value),
            b"SwapTotal" => info.swap_total = kilobytes(value),
            b"SwapFree" => info.swap_free = kilobytes(value),
            _ => {}
        }
    }
    info.total = total?;
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stat line in the kernel's format, with a zebrad's name and thread count.
    const ZEBRAD_STAT: &[u8] = b"4242 (zebrad) S 4100 4242 4100 0 -1 4194560 152340 0 12 0 \
        8123 911 37 4 20 0 68 0 1234567 3305209856 1598234 18446744073709551615 1 1 0 0 0 0 0 \
        4096 17000 0 0 0 17 5 0 0 0 0 0\n";

    #[test]
    fn stat_fields_come_after_the_last_parenthesis() {
        let stat = parse_stat(ZEBRAD_STAT).expect("a stat line");
        assert_eq!(stat.pid, 4242);
        assert_eq!(stat.comm, b"zebrad");
        assert_eq!(stat.state, b'S');
        assert_eq!(stat.ppid, 4100);
        assert_eq!(stat.pgrp, 4242);
        assert_eq!((stat.utime, stat.stime), (8123, 911));
        assert_eq!((stat.cutime, stat.cstime), (37, 4));
        assert_eq!(stat.threads, 68);
        assert_eq!(stat.starttime, 1_234_567);

        // A name with spaces and parentheses of its own.
        let weird = b"77 () weird (name) Z 1 77 77 0 -1 4194560 0 0 0 0 1 2 3 4 20 0 1 0 900 0 0\n";
        let stat = parse_stat(weird).expect("a stat line with a weird name");
        assert_eq!(stat.comm, b") weird (name");
        assert_eq!(stat.state, b'Z');
        assert_eq!(stat.ppid, 1);
        assert_eq!(
            (stat.utime, stat.stime, stat.cutime, stat.cstime),
            (1, 2, 3, 4)
        );
        assert_eq!(stat.starttime, 900);
    }

    #[test]
    fn names_with_any_bytes_never_fail_a_read() {
        // A newline, a tab and a three-byte character cut after its first byte.
        let mut line =
            b"88 (a\nb\tc\xe2 d) R 2 88 88 0 -1 0 0 0 0 0 5 6 0 0 20 0 3 0 4000 0 0".to_vec();
        line.push(b'\n');
        let stat = parse_stat(&line).expect("parsed");
        assert_eq!(stat.comm, b"a\nb\tc\xe2 d");
        assert_eq!(stat.threads, 3);
        // status escapes the name, as the kernel does (`\n` and `\t` become two characters).
        let status = parse_status(b"Name:\ta\\nb\\tc\xe2 d\nUmask:\t0022\nState:\tR (running)\n");
        assert_eq!(status.name.as_deref(), Some(&b"a\\nb\\tc\xe2 d"[..]));
    }

    #[test]
    fn malformed_stat_lines_are_rejected() {
        for bad in [
            &b""[..],
            b"4242 zebrad S 1",
            b"4242 (zebrad) S 1 2",
            b"x (zebrad) S 1 4242 4100 0 -1 0 0 0 0 0 1 2 3 4 20 0 68 0 1234567",
            b"4242 (zebrad) S 1 4242 4100 0 -1 0 0 0 0 0 one 2 3 4 20 0 68 0 1234567",
        ] {
            assert_eq!(parse_stat(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn status_gives_the_name_and_memory() {
        let status = parse_status(
            b"Name:\tzebrad\nUmask:\t0022\nState:\tS (sleeping)\nVmPeak:\t 3305209 kB\n\
              VmHWM:\t 6871948 kB\nVmRSS:\t 6395592 kB\nThreads:\t68\n",
        );
        assert_eq!(status.name.as_deref(), Some(&b"zebrad"[..]));
        assert_eq!(status.rss, Some(6_395_592 * 1024));
        assert_eq!(status.hwm, Some(6_871_948 * 1024));
        // A zombie's status has no memory lines.
        let zombie = parse_status(b"Name:\tsh\nState:\tZ (zombie)\nThreads:\t1\n");
        assert_eq!(zombie.name.as_deref(), Some(&b"sh"[..]));
        assert_eq!(zombie.rss, None);
        assert_eq!(zombie.hwm, None);
    }

    #[test]
    fn io_gives_the_storage_bytes() {
        let io = parse_io(
            b"rchar: 5000000000\nwchar: 7000000000\nsyscr: 1\nsyscw: 2\n\
              read_bytes: 12300000000\nwrite_bytes: 40100000000\ncancelled_write_bytes: 0\n",
        )
        .expect("an io file");
        assert_eq!(io.read_bytes, 12_300_000_000);
        assert_eq!(io.write_bytes, 40_100_000_000);
        assert_eq!(parse_io(b"rchar: 1\nwchar: 2\n"), None);
    }

    #[test]
    fn proc_stat_gives_cpu_times_and_the_boot_time() {
        let text = b"cpu  10132153 290696 3084719 46828483 16683 0 25195 0 175628 0\n\
            cpu0 1393280 32966 572056 13343292 6130 0 17875 0 23933 0\n\
            intr 199292224 30 9 0 0 0\nctxt 4143934051\nbtime 1792000000\n\
            processes 26442\nprocs_running 2\n";
        let times = parse_cpu_times(text).expect("a cpu line");
        assert_eq!(times.busy, 10_132_153 + 290_696 + 3_084_719 + 25_195);
        assert_eq!(times.idle, 46_828_483 + 16_683);
        assert_eq!(parse_btime(text), Some(1_792_000_000));
        // An old kernel's four fields.
        let old = parse_cpu_times(b"cpu 1 2 3 4\n").expect("four fields");
        assert_eq!(old, CpuTimes { busy: 6, idle: 4 });
        assert_eq!(parse_cpu_times(b"cpu0 1 2 3 4\n"), None);
        assert_eq!(parse_cpu_times(b"cpu 1 2\n"), None);
        assert_eq!(parse_btime(b"cpu 1 2 3 4\n"), None);
    }

    #[test]
    fn meminfo_gives_memory_and_swap() {
        let info = parse_meminfo(
            b"MemTotal:       16303856 kB\nMemFree:         1052004 kB\n\
              MemAvailable:    4928320 kB\nBuffers:          123456 kB\n\
              SwapCached:            0 kB\nSwapTotal:       2097148 kB\nSwapFree:        1782524 kB\n",
        )
        .expect("meminfo");
        assert_eq!(info.total, 16_303_856 * 1024);
        assert_eq!(info.available, Some(4_928_320 * 1024));
        assert_eq!(info.swap_total, Some(2_097_148 * 1024));
        assert_eq!(info.swap_free, Some(1_782_524 * 1024));
        let old = parse_meminfo(b"MemTotal: 1000 kB\nMemFree: 10 kB\n").expect("no MemAvailable");
        assert_eq!(old.available, None);
        assert_eq!(parse_meminfo(b"MemFree: 10 kB\n"), None);
    }
}
