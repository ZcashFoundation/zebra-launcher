//! The chain fake-zebrad and fake-zingod agree on: a tip, and a hash for each height up to
//! it. Included by both with `#[path]`, and by tests that compare their answers.

#![allow(dead_code)]

/// fake-zebrad's height (`getblockchaininfo`'s `blocks`).
pub const TIP: u64 = 2_500_000;

/// The hash of the block at `height`, in zebrad's display order (`getblockhash`): 32 bytes as
/// 64 hex digits, from a mix of the height so neighbours differ everywhere.
pub fn block_hash(height: u64) -> String {
    (0..4u64)
        .map(|word| {
            let mixed = (height ^ word.wrapping_mul(0x9E37_79B9_7F4A_7C15))
                .wrapping_mul(0xBF58_476D_1CE4_E5B9)
                .rotate_left(31)
                .wrapping_mul(0x94D0_49BB_1331_11EB);
            format!("{mixed:016x}")
        })
        .collect()
}

/// The same hash in internal byte order, as a light wallet server sends it in a `BlockID`.
pub fn block_hash_bytes(height: u64) -> Vec<u8> {
    let hex = block_hash(height);
    let mut bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or_default())
        .collect();
    bytes.reverse();
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_differ_and_reverse() {
        assert_eq!(block_hash(7).len(), 64);
        assert_ne!(block_hash(7), block_hash(8));
        assert_eq!(block_hash(7), block_hash(7));
        let bytes = block_hash_bytes(7);
        assert_eq!(bytes.len(), 32);
        assert_eq!(format!("{:02x}", bytes[31]), block_hash(7)[..2]);
    }
}
