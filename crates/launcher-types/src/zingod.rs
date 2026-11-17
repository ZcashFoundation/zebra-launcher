//! The zingod build Zeal bundles on Linux, read from the root `ZINGOD_*` pins when this crate
//! is built.

/// The pinned zingod version, without a leading `v`.
pub fn version() -> &'static str {
    include_str!("../../../ZINGOD_VERSION").trim_ascii()
}

/// The full zingod commit that the version's tag names.
pub fn commit() -> &'static str {
    include_str!("../../../ZINGOD_COMMIT").trim_ascii()
}

/// The pinned commit's date in UTC, as `YYYY-MM-DD`.
pub fn date() -> &'static str {
    include_str!("../../../ZINGOD_DATE").trim_ascii()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_build_comes_from_the_pins() {
        assert_eq!(super::version(), "1.1.0");
        assert_eq!(super::commit(), "bfce8b2b433578054dec82928d2707e523ec75b7");
        assert_eq!(super::date(), "2026-11-10");
    }
}
