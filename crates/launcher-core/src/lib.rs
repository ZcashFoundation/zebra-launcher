//! Zebra Launcher logic that does not depend on Tauri.
//!
//! The Tauri app is a thin bridge over this crate: it maps commands and events to
//! the handles exposed here, so everything in this crate is testable with plain
//! `cargo test` against the `fake-zebrad` binary.

pub mod config;
mod fsutil;
pub mod logfile;
pub mod logs;
pub mod monitor;
pub mod paths;
pub mod pidfile;
pub mod policy;
pub mod process;
pub mod rpc;
pub mod settings;
pub mod supervisor;
pub mod updates;
