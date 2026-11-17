//! zingod, the light client indexer the light client server runs on Linux
//! (docs/PUBLIC-SERVICES.md, "zingod as the backend"): the configuration Zeal
//! writes ([`config`]), its command line and environment ([`command`]), and the
//! reader of its output ([`logs`]). The supervisor runs it through
//! `Launch::Zingod`.

pub mod command;
pub mod config;
pub mod logs;

pub use command::ZingodCommand;
pub use config::{ZingodConfig, ZingodNetwork};
