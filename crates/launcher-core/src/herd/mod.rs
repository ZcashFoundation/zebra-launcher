//! The nodes Zeal runs together (docs/HERD.md): the agreement actors that compare the nodes'
//! chains ([`agreement`]), the probes they ask ([`tips`]) and each node's own peer files
//! ([`peers`]).

pub mod agreement;
pub mod peers;
pub mod tips;

#[cfg(test)]
mod tests;

pub use agreement::{HerdConfig, HerdHandle, MemberInput, NETWORKS, members_of, spawn};
