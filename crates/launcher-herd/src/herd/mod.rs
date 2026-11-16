//! The herd's agreement (docs/HERD.md, Agreement): per network, from each witness node's best
//! tip, whether the nodes are on one chain, which are behind, stuck or on another branch,
//! and where a node split ([`search::fork_search`]). [`Engine`] runs the rounds; it is
//! sans-IO, so the backend's agreement actor and the browser mock run the same code, and
//! every transition is a native test with a fake clock.
//!
//! In this slice the inputs are the tips and phases only: peer diversity, `getchaintips`
//! corroboration, validators and alerts come in later phases.

pub mod engine;
pub mod roles;
pub mod search;
#[cfg(any(test, feature = "sim"))]
pub mod sim;
pub mod spacing;
pub mod verdict;

#[cfg(test)]
mod tests;

pub use engine::{Engine, MAX_PROBES, MemberFacts, Now};
pub use search::{Probe, ProbeResult};
