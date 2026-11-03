//! What a node's processes and the computer use: CPU, memory, disk I/O and process counts,
//! read every 5 s by the backend's samplers. See docs/RESOURCES.md.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// The most processes one sample lists; [`NodeResources::processes`] still counts them all.
pub const MAX_MEMBERS: usize = 32;

/// A node's process tree, read every 5 s from the spawn until the supervisor finalizes the
/// run, or until a reader panics: the `node_resources` event and `get_node_resources`.
/// `None` on the wire while the node has no run, and after a reader panic until the next run.
/// Counters are cumulative since the spawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeResources {
    /// The supervisor's run id: counters restart when it changes.
    pub run: u64,
    /// When the tree was read.
    pub at: SystemTime,
    /// Monotonic seconds since the spawn: the base of every rate.
    pub run_secs: f64,
    /// What the figures mean (the UI's labels).
    pub platform: Platform,
    /// User plus system CPU of the whole tree, exited processes included. Never decreases
    /// within a run. 0 while `cpu_cores` is `None` for the whole run.
    pub cpu_secs: f64,
    /// Over the last interval (the first sample: since the spawn); 1.0 is one logical CPU.
    /// `None` when CPU cannot be read.
    #[serde(default)]
    pub cpu_cores: Option<f64>,
    /// The live processes' memory, summed (see [`Platform::memory_label`]).
    pub memory_bytes: u64,
    /// The highest this run: exact for one process, a lower bound for several.
    pub peak_memory_bytes: u64,
    /// Windows only: the commit charge (`PrivateUsage`), summed.
    #[serde(default)]
    pub committed_bytes: Option<u64>,
    /// Read since the spawn (see [`Platform::io_label`]); `None` when the OS refuses the read.
    #[serde(default)]
    pub read_bytes: Option<u64>,
    /// Written since the spawn, like `read_bytes`.
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
    /// The process Zeal started first, then the others in tree order (each process before
    /// its children, children by start time); at most [`MAX_MEMBERS`].
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
    /// What the memory figure is: the number the OS's own monitor shows.
    pub fn memory_label(self) -> &'static str {
        match self {
            Platform::Linux => "Resident memory (RSS)",
            Platform::Macos => "Memory as Activity Monitor shows it",
            Platform::Windows => "Working set",
        }
    }

    /// "Disk" on Linux and macOS; "I/O" on Windows, which also counts network and devices.
    pub fn io_label(self) -> &'static str {
        match self {
            Platform::Linux | Platform::Macos => "Disk",
            Platform::Windows => "I/O",
        }
    }

    /// What [`NodeResources::descriptors`] counts.
    pub fn descriptors_label(self) -> &'static str {
        match self {
            Platform::Linux | Platform::Macos => "open files",
            Platform::Windows => "handles",
        }
    }

    /// True on Windows, where the job holds every process the node ever started. On Unix a
    /// process that leaves the group and is orphaned before it is first seen is missed.
    pub fn tree_is_exact(self) -> bool {
        self == Platform::Windows
    }
}

/// The computer, read every 5 s: the `machine_resources` event and `get_machine_resources`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MachineResources {
    /// When it was read.
    pub at: SystemTime,
    pub logical_cpus: u32,
    /// Busy over the last interval, in logical CPUs (0 to `logical_cpus`); `None` on the
    /// first sample.
    #[serde(default)]
    pub cpu_busy_cores: Option<f64>,
    pub memory_total_bytes: u64,
    /// Usable without swapping: MemAvailable on Linux.
    #[serde(default)]
    pub memory_available_bytes: Option<u64>,
    /// Linux and macOS.
    #[serde(default)]
    pub swap_total_bytes: Option<u64>,
    #[serde(default)]
    pub swap_used_bytes: Option<u64>,
    /// Windows: CommitTotal.
    #[serde(default)]
    pub commit_bytes: Option<u64>,
    /// Windows: CommitLimit.
    #[serde(default)]
    pub commit_limit_bytes: Option<u64>,
    /// macOS only.
    #[serde(default)]
    pub pressure: Option<MemoryPressure>,
}

/// macOS's memory pressure level (`kern.memorystatus_vm_pressure_level`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
}

impl MemoryPressure {
    /// The level in words: `normal`, `warning`, `critical`.
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryPressure::Normal => "normal",
            MemoryPressure::Warning => "warning",
            MemoryPressure::Critical => "critical",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn sample() -> NodeResources {
        NodeResources {
            run: 3,
            at: at(1_800_000_000),
            run_secs: 125.5,
            platform: Platform::Linux,
            cpu_secs: 420.25,
            cpu_cores: Some(5.6),
            memory_bytes: 6_549_000_000,
            peak_memory_bytes: 6_871_000_000,
            committed_bytes: None,
            read_bytes: Some(12_300_000_000),
            written_bytes: Some(40_100_000_000),
            processes: 2,
            threads: Some(70),
            descriptors: Some(415),
            members: vec![
                ProcessUsage {
                    pid: 4242,
                    parent: None,
                    name: "zebrad".into(),
                    started_at: at(1_799_999_874),
                    cpu_secs: 420.0,
                    memory_bytes: Some(6_548_000_000),
                    read_bytes: Some(12_300_000_000),
                    written_bytes: Some(40_100_000_000),
                    threads: Some(68),
                },
                ProcessUsage {
                    pid: 4300,
                    parent: Some(4242),
                    name: "sh".into(),
                    started_at: at(1_799_999_999),
                    cpu_secs: 0.25,
                    memory_bytes: Some(1_000_000),
                    read_bytes: None,
                    written_bytes: None,
                    threads: Some(1),
                },
            ],
        }
    }

    #[test]
    fn samples_round_trip() {
        let sample = sample();
        let json = serde_json::to_value(&sample).unwrap();
        assert_eq!(json["platform"], "linux");
        let back: NodeResources = serde_json::from_value(json).unwrap();
        assert_eq!(back, sample);

        let machine = MachineResources {
            at: at(1_800_000_000),
            logical_cpus: 8,
            cpu_busy_cores: Some(6.5),
            memory_total_bytes: 17_179_869_184,
            memory_available_bytes: Some(5_046_586_573),
            swap_total_bytes: Some(2_147_483_648),
            swap_used_bytes: Some(322_122_547),
            commit_bytes: None,
            commit_limit_bytes: None,
            pressure: Some(MemoryPressure::Warning),
        };
        let json = serde_json::to_value(&machine).unwrap();
        assert_eq!(json["pressure"], "warning");
        let back: MachineResources = serde_json::from_value(json).unwrap();
        assert_eq!(back, machine);
    }

    #[test]
    fn platforms_and_pressures_have_snake_case_wire_names() {
        for (platform, name) in [
            (Platform::Linux, "linux"),
            (Platform::Macos, "macos"),
            (Platform::Windows, "windows"),
        ] {
            assert_eq!(serde_json::to_value(platform).unwrap(), name);
            let parsed: Platform = serde_json::from_value(serde_json::json!(name)).unwrap();
            assert_eq!(parsed, platform);
        }
        for (pressure, name) in [
            (MemoryPressure::Normal, "normal"),
            (MemoryPressure::Warning, "warning"),
            (MemoryPressure::Critical, "critical"),
        ] {
            assert_eq!(serde_json::to_value(pressure).unwrap(), name);
            assert_eq!(pressure.as_str(), name);
        }
        assert!(MemoryPressure::Critical > MemoryPressure::Warning);
    }

    #[test]
    fn samples_without_their_optional_fields_still_deserialize() {
        let node: NodeResources = serde_json::from_str(
            r#"{"run":1,"at":{"secs_since_epoch":1800000000,"nanos_since_epoch":0},"run_secs":5.0,
                "platform":"windows","cpu_secs":1.5,"memory_bytes":100,"peak_memory_bytes":120,
                "processes":1,"members":[{"pid":7,"name":"zebrad.exe",
                "started_at":{"secs_since_epoch":1799999995,"nanos_since_epoch":0},"cpu_secs":1.5}]}"#,
        )
        .expect("a sample without its optional fields");
        assert_eq!(node.cpu_cores, None);
        assert_eq!(node.read_bytes, None);
        assert_eq!(node.threads, None);
        assert_eq!(node.members[0].parent, None);
        assert_eq!(node.members[0].memory_bytes, None);

        let machine: MachineResources = serde_json::from_str(
            r#"{"at":{"secs_since_epoch":1800000000,"nanos_since_epoch":0},"logical_cpus":4,
                "memory_total_bytes":8000000000}"#,
        )
        .expect("a machine sample without its optional fields");
        assert_eq!(machine.cpu_busy_cores, None);
        assert_eq!(machine.memory_available_bytes, None);
        assert_eq!(machine.pressure, None);
    }

    #[test]
    fn labels_say_what_each_platform_measures() {
        assert_eq!(Platform::Linux.memory_label(), "Resident memory (RSS)");
        assert_eq!(Platform::Windows.io_label(), "I/O");
        assert_eq!(Platform::Macos.io_label(), "Disk");
        assert_eq!(Platform::Linux.descriptors_label(), "open files");
        assert_eq!(Platform::Windows.descriptors_label(), "handles");
        assert!(Platform::Windows.tree_is_exact());
        assert!(!Platform::Linux.tree_is_exact());
    }
}
