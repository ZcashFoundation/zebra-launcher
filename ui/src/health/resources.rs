//! What the node's processes and the computer use, in the verdict's input and in the copied
//! summary (docs/RESOURCES.md). The sentences come from the Resources card's own helpers, so
//! the summary cannot disagree with the card.

use std::time::SystemTime;

use launcher_types::{MachineResources, NodeResources};

use crate::{
    charts::byte_rate,
    format::{cores, mem_bytes, share, thousands},
    machine_history::MachineHistory,
    pages::dashboard::resources::{last_run_text, machine_text},
    resource_history::{RATE_WINDOW, ResourceHistory},
};

/// The resource facts the verdict and the summary read, borrowed.
#[derive(Clone, Copy)]
pub struct ResourceInput<'a> {
    /// The node's newest sample, `None` while it has no run.
    pub node: Option<&'a NodeResources>,
    /// The node's samples since the launcher opened.
    pub history: &'a ResourceHistory,
    /// The computer's newest sample.
    pub machine: Option<&'a MachineResources>,
    /// The computer's samples since the launcher opened.
    pub machine_history: &'a MachineHistory,
}

impl ResourceInput<'_> {
    /// The copied summary's lines, after the state on disk: the node's processes over the last
    /// minute (its last run once it stopped), and the computer as the card's footer has it.
    ///
    /// `Resources: 5.6 cores (70 % of 8) · 6.1 GiB, peak 6.4 GiB · 1 process · Disk 35.0 MB/s
    /// read, 95.0 MB/s written`
    pub fn summary_lines(&self, now: SystemTime) -> Vec<String> {
        let mut lines = Vec::with_capacity(2);
        match self.node {
            Some(sample) => lines.push(format!("Resources: {}", self.node_text(sample, now))),
            None => {
                if let Some(summary) = self.history.last_run() {
                    lines.push(format!("Resources: {}", last_run_text(&summary)));
                }
            }
        }
        if let Some(machine) = self.machine {
            let busy = self.machine_history.busy_cores(RATE_WINDOW, now);
            lines.push(machine_text(machine, busy));
        }
        lines
    }

    /// The live sample's figures, with the one-minute rates.
    fn node_text(&self, sample: &NodeResources, now: SystemTime) -> String {
        let mut parts = Vec::with_capacity(4);
        let cpus = self
            .machine
            .map(|machine| machine.logical_cpus)
            .filter(|&cpus| cpus > 0);
        parts.push(match (self.history.cpu_cores(RATE_WINDOW, now), cpus) {
            (Some(busy), Some(cpus)) => format!(
                "{} ({} of {cpus})",
                cores(busy),
                share(busy / f64::from(cpus))
            ),
            (Some(busy), None) => cores(busy),
            (None, _) => "CPU not available".to_owned(),
        });
        let mut memory = format!(
            "{}, peak {}",
            mem_bytes(sample.memory_bytes),
            mem_bytes(sample.peak_memory_bytes)
        );
        if let Some(committed) = sample.committed_bytes {
            memory.push_str(&format!(", {} committed", mem_bytes(committed)));
        }
        parts.push(memory);
        parts.push(match sample.processes {
            1 => "1 process".to_owned(),
            n => format!("{} processes", thousands(u64::from(n))),
        });
        let rate = |rate: Option<f64>| rate.map_or_else(|| "—".to_owned(), byte_rate);
        match self.history.io_rates(RATE_WINDOW, now) {
            (None, None) => {}
            (read, written) => parts.push(format!(
                "{} {} read, {} written",
                sample.platform.io_label(),
                rate(read),
                rate(written)
            )),
        }
        parts.join(" · ")
    }
}
