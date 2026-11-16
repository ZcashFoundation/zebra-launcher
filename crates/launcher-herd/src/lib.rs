//! Pure rules for the nodes Zeal runs together (docs/HERD.md): the public services' rules
//! ([`public`], docs/PUBLIC-SERVICES.md) and the herd's agreement ([`herd`]).
//!
//! Everything here is a function of its inputs: no I/O, no clock and no async runtime, so the
//! backend and the browser mock run the same rules. Functions that need the time take `now`,
//! since `SystemTime::now` panics on `wasm32-unknown-unknown`.

#![forbid(unsafe_code)]

pub mod herd;
pub mod public;
