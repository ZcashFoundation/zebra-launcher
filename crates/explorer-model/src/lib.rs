//! The block explorer's pure model, shared by the explorer backend (launcher-core), the UI and
//! its browser mock. No I/O and no async runtime; it builds natively and to WebAssembly. Byte
//! parsing (serde_json) stays with the callers: this crate only declares zebrad's shapes.
//!
//! - [`shapes`]: zebrad's RPC answers, reduced to the fields the explorer reads.
//! - [`upgrades`]: network upgrade heights and target spacings.
//! - [`ledger`]: what a block did to the value pools, and whether it adds up.
//! - [`views`]: the pages of `launcher_types::explorer` built from the shapes.
//! - [`classify`]: what a search query is, refused secrets first.
//! - [`synth`]: a deterministic chain in zebrad's shapes, for the mock and the tests.
//!
//! See docs/EXPLORER.md.

#![forbid(unsafe_code)]

pub mod classify;
pub mod ledger;
pub mod shapes;
pub mod synth;
pub mod upgrades;
pub mod views;

/// Zatoshis in one ZEC.
pub const ZAT_PER_ZEC: i64 = 100_000_000;

/// An amount zebrad reports in ZEC as zatoshis. Exact for every amount zebrad reports: it
/// prints at most eight decimals and the supply fits in 51 bits.
pub fn zec_to_zat(zec: f64) -> i64 {
    (zec * ZAT_PER_ZEC as f64).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zec_converts_exactly() {
        assert_eq!(zec_to_zat(0.0), 0);
        assert_eq!(zec_to_zat(0.000_000_01), 1);
        assert_eq!(zec_to_zat(1.5625), 156_250_000);
        assert_eq!(zec_to_zat(0.1 + 0.2), 30_000_000);
        assert_eq!(zec_to_zat(21_000_000.0), 2_100_000_000_000_000);
        assert_eq!(zec_to_zat(-0.0001), -10_000);
    }
}
