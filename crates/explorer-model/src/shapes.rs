//! zebrad 7.0.0's RPC answers, reduced to the fields the explorer reads (zebra-rpc's
//! `BlockObject`, `TransactionObject`, `GetBlockSubsidyResponse` and friends). Unknown fields
//! are skipped, so the Equihash solution and every proof and ciphertext are never kept, and
//! shielded arrays are only counted ([`Count`]); of a script only its length is kept
//! ([`HexLen`]), which ZIP 317 counts. Amounts zebrad gives in ZEC alone (a spent output's
//! value, a fee) are read as zatoshis ([`zec_amount`]). Optional fields default, so a zebrad
//! that leaves one out still decodes.
//!
//! The shapes also serialize, in the same field names, so the synthetic chain can stand in for
//! zebrad: fields that are `None` are left out, as zebrad leaves them out, and the genesis
//! block's parent is written as zebrad writes it.

use std::fmt;

use launcher_types::explorer::{BlockHash, Pool, TxId};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{IgnoredAny, SeqAccess, Visitor},
    ser::SerializeSeq,
};

/// The length of an array whose elements the explorer does not read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count(pub u32);

impl<'de> Deserialize<'de> for Count {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CountVisitor;
        impl<'de> Visitor<'de> for CountVisitor {
            type Value = Count;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Count, A::Error> {
                let mut count = 0u32;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    count = count.saturating_add(1);
                }
                Ok(Count(count))
            }
        }
        deserializer.deserialize_seq(CountVisitor)
    }
}

impl Serialize for Count {
    /// As many empty objects: zebrad's elements are objects whose fields nobody reads here.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Empty {}
        let mut seq = serializer.serialize_seq(Some(self.0 as usize))?;
        for _ in 0..self.0 {
            seq.serialize_element(&Empty {})?;
        }
        seq.end()
    }
}

fn is_zero(count: &Count) -> bool {
    count.0 == 0
}

/// The length in bytes of a hex string the explorer does not read, such as a script: counted
/// as it is decoded, never kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HexLen(pub u32);

impl<'de> Deserialize<'de> for HexLen {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HexVisitor;
        impl Visitor<'_> for HexVisitor {
            type Value = HexLen;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a hex string")
            }
            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<HexLen, E> {
                Ok(HexLen(u32::try_from(text.len() / 2).unwrap_or(u32::MAX)))
            }
        }
        deserializer.deserialize_str(HexVisitor)
    }
}

impl Serialize for HexLen {
    /// As many zero bytes, in hex: what is read back is only the length.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&"00".repeat(self.0 as usize))
    }
}

/// Amounts zebrad gives in ZEC only, as zatoshis: exact for every amount it prints
/// ([`crate::zec_to_zat`]), and written back in ZEC.
pub mod zec_amount {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        f64::deserialize(deserializer).map(crate::zec_to_zat)
    }

    pub fn serialize<S: Serializer>(zat: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        (*zat as f64 / crate::ZAT_PER_ZEC as f64).serialize(serializer)
    }

    /// The same for an optional amount.
    pub mod option {
        use serde::{Deserialize, Deserializer, Serialize, Serializer};

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<i64>, D::Error> {
            Option::<f64>::deserialize(deserializer).map(|zec| zec.map(crate::zec_to_zat))
        }

        pub fn serialize<S: Serializer>(
            zat: &Option<i64>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            zat.map(|zat| zat as f64 / crate::ZAT_PER_ZEC as f64)
                .serialize(serializer)
        }
    }
}

/// `getblock <hash or height> 1`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockV1 {
    pub hash: BlockHash,
    /// Confirmations in the best chain, or -1 outside it.
    #[serde(default)]
    pub confirmations: i64,
    /// Present only with zebrad's value-pool row for the block (`BlockInfo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
    #[serde(
        default,
        rename = "merkleroot",
        skip_serializing_if = "Option::is_none"
    )]
    pub merkle_root: Option<String>,
    #[serde(
        default,
        rename = "blockcommitments",
        skip_serializing_if = "Option::is_none"
    )]
    pub block_commitments: Option<String>,
    #[serde(
        default,
        rename = "finalsaplingroot",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_sapling_root: Option<String>,
    #[serde(
        default,
        rename = "finalorchardroot",
        skip_serializing_if = "Option::is_none"
    )]
    pub final_orchard_root: Option<String>,
    /// The transaction ids, in block order.
    #[serde(default)]
    pub tx: Vec<TxId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<f64>,
    /// Present only with the block's `BlockInfo` row.
    #[serde(
        default,
        rename = "chainSupply",
        skip_serializing_if = "Option::is_none"
    )]
    pub chain_supply: Option<PoolValue>,
    /// Present only with the block's `BlockInfo` row.
    #[serde(
        default,
        rename = "valuePools",
        skip_serializing_if = "Option::is_none"
    )]
    pub value_pools: Option<Vec<PoolValue>>,
    #[serde(default)]
    pub trees: Trees,
    /// The parent's hash; `None` for the genesis block ([`parent_hash`]).
    #[serde(default, rename = "previousblockhash", with = "parent_hash")]
    pub previous: Option<BlockHash>,
    #[serde(
        default,
        rename = "nextblockhash",
        skip_serializing_if = "Option::is_none"
    )]
    pub next: Option<BlockHash>,
}

/// `previousblockhash`. zebrad writes the genesis block's as 64 zeros (zebra-chain's
/// `GENESIS_PREVIOUS_BLOCK_HASH`), which is no block's hash: it reads as no parent, and no
/// parent is written back the same way, as zebrad writes it.
mod parent_hash {
    use launcher_types::explorer::{BlockHash, Hash32};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    const GENESIS_PREVIOUS: BlockHash = Hash32([0; 32]);

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<BlockHash>, D::Error> {
        let hash = Option::<BlockHash>::deserialize(deserializer)?;
        Ok(hash.filter(|hash| *hash != GENESIS_PREVIOUS))
    }

    pub(super) fn serialize<S: Serializer>(
        previous: &Option<BlockHash>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        previous.unwrap_or(GENESIS_PREVIOUS).serialize(serializer)
    }
}

impl BlockV1 {
    /// The pool balances after the block, when zebrad has its row. A pool zebrad does not
    /// list (an older release without Ironwood) reads as zero.
    pub fn pools(&self) -> Option<[u64; 6]> {
        let pools = self.value_pools.as_ref()?;
        let mut out = [0u64; 6];
        for value in pools {
            if let Some(pool) = Pool::from_id(&value.id) {
                out[pool.index()] = value.chain_value_zat;
            }
        }
        Some(out)
    }
}

/// `getblock <hash> 3`: the block's transactions in full, each input with the output it
/// spends and each transaction with its fee. Only the fields the explorer reads; the rest of
/// the block comes from verbosity 1.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockV3 {
    pub hash: BlockHash,
    #[serde(default)]
    pub confirmations: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default)]
    pub tx: Vec<Tx>,
}

/// One entry of `valuePools`, or `chainSupply` (whose `id` is empty).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolValue {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(rename = "chainValueZat")]
    pub chain_value_zat: u64,
    /// zebrad's change for the block. It diffs against zero when only the parent lacks a row
    /// (methods.rs:1797-1806), so the ledger computes changes from both blocks instead.
    #[serde(
        default,
        rename = "valueDeltaZat",
        skip_serializing_if = "Option::is_none"
    )]
    pub value_delta_zat: Option<i64>,
}

/// `trees`: the note commitment trees' sizes after the block. zebrad leaves an empty tree out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trees {
    #[serde(default, skip_serializing_if = "TreeSize::is_empty")]
    pub sapling: TreeSize,
    #[serde(default, skip_serializing_if = "TreeSize::is_empty")]
    pub orchard: TreeSize,
    #[serde(default, skip_serializing_if = "TreeSize::is_empty")]
    pub ironwood: TreeSize,
}

impl Trees {
    /// Sapling, Orchard and Ironwood.
    pub fn sizes(&self) -> [u64; 3] {
        [self.sapling.size, self.orchard.size, self.ironwood.size]
    }
}

/// One tree's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeSize {
    pub size: u64,
}

impl TreeSize {
    fn is_empty(&self) -> bool {
        self.size == 0
    }
}

/// `getrawtransaction <txid> 1`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tx {
    pub txid: TxId,
    /// The height in the best chain, -1 in a side chain, absent in the mempool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmations: Option<i64>,
    /// The fee in zatoshis, in `getblock` at verbosity 3 only, and only when zebrad found every
    /// output the transaction spends. Never in a coinbase.
    #[serde(
        default,
        with = "zec_amount::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub fee: Option<i64>,
    #[serde(default)]
    pub vin: Vec<TxIn>,
    #[serde(default)]
    pub vout: Vec<TxOut>,
    #[serde(default, rename = "vShieldedSpend")]
    pub sapling_spends: Count,
    #[serde(default, rename = "vShieldedOutput")]
    pub sapling_outputs: Count,
    #[serde(default, rename = "vjoinsplit")]
    pub joinsplits: Vec<JoinSplit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchard: Option<Actions>,
    /// zebra-rpc reuses the Orchard object for Ironwood (types/transaction.rs:386-390).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ironwood: Option<Actions>,
    /// Sapling's value balance: the value that leaves the pool, negative when value enters.
    #[serde(
        default,
        rename = "valueBalanceZat",
        skip_serializing_if = "Option::is_none"
    )]
    pub sapling_value_balance: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<i64>,
    #[serde(default)]
    pub version: u32,
    #[serde(default, rename = "locktime")]
    pub lock_time: u32,
    #[serde(
        default,
        rename = "expiryheight",
        skip_serializing_if = "Option::is_none"
    )]
    pub expiry_height: Option<u32>,
    #[serde(default, rename = "blockhash", skip_serializing_if = "Option::is_none")]
    pub block_hash: Option<BlockHash>,
    #[serde(default, rename = "blocktime", skip_serializing_if = "Option::is_none")]
    pub block_time: Option<i64>,
}

impl Tx {
    /// A block's first transaction.
    pub fn is_coinbase(&self) -> bool {
        self.vin
            .first()
            .is_some_and(|input| input.coinbase.is_some())
    }

    /// The value that left each shielded pool: Sprout, Sapling, Orchard, Ironwood (the
    /// order of [`Pool::SHIELDED`]). Negative when value entered the pool.
    pub fn net_out(&self) -> [i64; 4] {
        let sprout = self
            .joinsplits
            .iter()
            .map(|joinsplit| joinsplit.vpub_new - joinsplit.vpub_old)
            .sum();
        [
            sprout,
            self.sapling_value_balance.unwrap_or(0),
            self.orchard
                .as_ref()
                .map_or(0, |bundle| bundle.value_balance),
            self.ironwood
                .as_ref()
                .map_or(0, |bundle| bundle.value_balance),
        ]
    }

    /// Whether the transaction has a part in each shielded pool, in [`Pool::SHIELDED`]'s
    /// order.
    pub fn touches(&self) -> [bool; 4] {
        [
            !self.joinsplits.is_empty(),
            self.sapling_spends.0 > 0 || self.sapling_outputs.0 > 0,
            self.orchard
                .as_ref()
                .is_some_and(|bundle| bundle.actions.0 > 0),
            self.ironwood
                .as_ref()
                .is_some_and(|bundle| bundle.actions.0 > 0),
        ]
    }

    /// The sum of the transparent outputs.
    pub fn transparent_out(&self) -> i64 {
        self.vout.iter().map(|output| output.value_zat).sum()
    }
}

/// A transparent input: a coinbase script, or the output it spends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxIn {
    /// The coinbase script in hex, for a coinbase input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coinbase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub txid: Option<TxId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vout: Option<u32>,
    /// The unlocking script, for its length.
    #[serde(default, rename = "scriptSig", skip_serializing_if = "Option::is_none")]
    pub script_sig: Option<ScriptSig>,
    /// The output it spends, in `getblock` at verbosity 3 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prevout: Option<Prevout>,
}

impl TxIn {
    /// The input's size as ZIP 317 counts it: the outpoint (36 bytes), the script with its
    /// length prefix, and the sequence (4 bytes). A coinbase input's script is its
    /// `coinbase` field.
    pub fn serialized_size(&self) -> u32 {
        let script = match (&self.coinbase, &self.script_sig) {
            (Some(coinbase), _) => u32::try_from(coinbase.len() / 2).unwrap_or(u32::MAX),
            (None, Some(sig)) => sig.hex.0,
            (None, None) => 0,
        };
        36 + compact_size_len(script) + script + 4
    }
}

/// An unlocking script.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptSig {
    #[serde(default)]
    pub hex: HexLen,
}

/// The output an input spends, as `getblock` at verbosity 3 gives it (Bitcoin Core's
/// `prevout`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prevout {
    /// Whether a coinbase made it.
    #[serde(default)]
    pub generated: bool,
    /// The height of the block that made it.
    #[serde(default)]
    pub height: u32,
    /// Its value in zatoshis; zebrad gives it in ZEC only.
    #[serde(rename = "value", with = "zec_amount")]
    pub value_zat: i64,
    #[serde(rename = "scriptPubKey")]
    pub script: ScriptPubKey,
}

/// The bytes of a script's length prefix (Bitcoin's compact size).
fn compact_size_len(len: u32) -> u32 {
    match len {
        0..=252 => 1,
        253..=0xffff => 3,
        _ => 5,
    }
}

/// A transparent output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOut {
    #[serde(rename = "valueZat")]
    pub value_zat: i64,
    pub n: u32,
    #[serde(rename = "scriptPubKey")]
    pub script: ScriptPubKey,
}

impl TxOut {
    /// The output's size as ZIP 317 counts it: the value (8 bytes) and the script with its
    /// length prefix.
    pub fn serialized_size(&self) -> u32 {
        8 + compact_size_len(self.script.hex.0) + self.script.hex.0
    }
}

/// The parts of `scriptPubKey` the explorer shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptPubKey {
    /// `pubkeyhash`, `scripthash`, `nulldata`, `nonstandard` and so on.
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addresses: Option<Vec<String>>,
    /// The locking script, for its length.
    #[serde(default)]
    pub hex: HexLen,
}

/// A Sprout JoinSplit's public values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSplit {
    /// Value taken into Sprout from the transaction's transparent value.
    #[serde(rename = "vpub_oldZat")]
    pub vpub_old: i64,
    /// Value taken out of Sprout.
    #[serde(rename = "vpub_newZat")]
    pub vpub_new: i64,
}

/// An Orchard or Ironwood bundle: its actions, counted, and its value balance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actions {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub actions: Count,
    /// The value that leaves the pool, negative when value enters.
    #[serde(rename = "valueBalanceZat")]
    pub value_balance: i64,
}

/// `getblocksubsidy <height>`. Amounts are in ZEC except the streams' `valueZat`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Subsidy {
    #[serde(
        default,
        rename = "fundingstreams",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub funding_streams: Vec<Stream>,
    #[serde(
        default,
        rename = "lockboxstreams",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub lockbox_streams: Vec<Stream>,
    /// The miner's part of the subsidy, fees not included.
    pub miner: f64,
    #[serde(default)]
    pub founders: f64,
    /// The block subsidy, fees not included.
    #[serde(rename = "totalblocksubsidy")]
    pub total: f64,
}

/// A funding or lockbox stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stream {
    pub recipient: String,
    #[serde(rename = "valueZat")]
    pub value_zat: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// `getbestblockheightandhash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BestBlock {
    pub height: u32,
    pub hash: BlockHash,
}

/// `getblockheader <hash or height> true`, the fields the explorer reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    pub hash: BlockHash,
    pub height: u32,
    pub time: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block at verbosity 1 as zebrad 7.0.0 writes it (fields from zebra-rpc's
    /// `BlockObject`), solution shortened.
    const BLOCK: &str = r#"{
        "hash": "0000000000d8a9ac4e30b7a7ce2bd2b3b1b9c67b6ec23f5a3e7ba7ca1d4ab501",
        "confirmations": 12, "size": 4521, "height": 3120000, "version": 4,
        "merkleroot": "aa", "blockcommitments": "bb", "finalsaplingroot": "cc",
        "finalorchardroot": "dd", "nTx": 2,
        "tx": ["1111111111111111111111111111111111111111111111111111111111111111",
               "2222222222222222222222222222222222222222222222222222222222222222"],
        "time": 1761200000, "nonce": "00ff", "solution": "0123456789abcdef", "bits": "1c01ad18",
        "difficulty": 118462377.5,
        "chainSupply": {"chainValue": 16.0, "chainValueZat": 1600000000, "monitored": true},
        "valuePools": [
            {"id": "transparent", "chainValue": 10.0, "chainValueZat": 1000000000, "monitored": true, "valueDelta": 1.0, "valueDeltaZat": 100000000},
            {"id": "sprout", "chainValue": 0.0, "chainValueZat": 0, "monitored": false},
            {"id": "sapling", "chainValue": 2.0, "chainValueZat": 200000000, "monitored": true},
            {"id": "orchard", "chainValue": 3.0, "chainValueZat": 300000000, "monitored": true},
            {"id": "lockbox", "chainValue": 1.0, "chainValueZat": 100000000, "monitored": true}
        ],
        "trees": {"sapling": {"size": 40}, "orchard": {"size": 70}},
        "previousblockhash": "0000000001c2a8f4b4a2a1e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3",
        "nextblockhash": "0000000002c2a8f4b4a2a1e3d2c1b0a9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3"
    }"#;

    #[test]
    fn a_block_decodes_from_zebrad_json() {
        let block: BlockV1 = serde_json::from_str(BLOCK).expect("block");
        assert_eq!(block.height, Some(3_120_000));
        assert_eq!(block.size, Some(4_521));
        assert_eq!(block.tx.len(), 2);
        assert_eq!(
            block.trees.sizes(),
            [40, 70, 0],
            "an empty tree is left out"
        );
        assert_eq!(
            block.pools(),
            Some([1_000_000_000, 0, 200_000_000, 300_000_000, 100_000_000, 0]),
            "a pool zebrad does not list reads as zero"
        );
        assert_eq!(
            block.chain_supply.map(|supply| supply.chain_value_zat),
            Some(1_600_000_000)
        );
        assert!(block.previous.is_some() && block.next.is_some());
        // Without the row: no size, no pools, no supply.
        let bare: BlockV1 = serde_json::from_str(
            r#"{"hash":"0000000000d8a9ac4e30b7a7ce2bd2b3b1b9c67b6ec23f5a3e7ba7ca1d4ab501",
                "confirmations":1,"height":5,"tx":[],"trees":{}}"#,
        )
        .expect("bare block");
        assert_eq!(bare.pools(), None);
        assert_eq!(bare.size, None);
        assert_eq!(bare.previous, None);
    }

    #[test]
    fn the_genesis_block_has_no_parent() {
        // zebrad 7.0.0 gives the genesis block a `previousblockhash` of 64 zeros.
        let zeros = "0".repeat(64);
        let genesis: BlockV1 = serde_json::from_str(&format!(
            r#"{{"hash":"0000000000d8a9ac4e30b7a7ce2bd2b3b1b9c67b6ec23f5a3e7ba7ca1d4ab501",
                "confirmations":9,"height":0,"tx":[],"trees":{{}},"previousblockhash":"{zeros}"}}"#
        ))
        .expect("genesis");
        assert_eq!(genesis.previous, None);
        let json = serde_json::to_value(&genesis).unwrap();
        assert_eq!(
            json["previousblockhash"], zeros,
            "written back as zebrad writes it"
        );
    }

    /// A transaction as zebrad writes it (zebra-rpc's `TransactionObject`), hex shortened.
    const TX: &str = r#"{
        "hex": "0500", "height": 3120000, "confirmations": 12,
        "vin": [{"txid": "1111111111111111111111111111111111111111111111111111111111111111",
                 "vout": 1, "scriptSig": {"asm": "", "hex": "00"}, "sequence": 4294967295}],
        "vout": [{"value": 0.5, "valueZat": 50000000, "n": 0,
                  "scriptPubKey": {"asm": "OP_DUP", "hex": "76a9", "reqSigs": 1,
                                   "type": "pubkeyhash", "addresses": ["t1abc"]}}],
        "vShieldedSpend": [{"cv": "aa", "anchor": "bb", "nullifier": "cc", "rk": "dd",
                            "proof": "ee", "spendAuthSig": "ff"}],
        "vShieldedOutput": [{"cv": "aa", "cmu": "bb", "ephemeralKey": "cc", "encCiphertext": "dd",
                             "outCiphertext": "ee", "proof": "ff"},
                            {"cv": "aa", "cmu": "bb", "ephemeralKey": "cc", "encCiphertext": "dd",
                             "outCiphertext": "ee", "proof": "ff"}],
        "vjoinsplit": [],
        "orchard": {"actions": [{"cv": "aa"}, {"cv": "bb"}], "valueBalance": -1.0,
                    "valueBalanceZat": -100000000},
        "valueBalance": 0.25, "valueBalanceZat": 25000000,
        "size": 9165, "time": 1761200000,
        "txid": "3333333333333333333333333333333333333333333333333333333333333333",
        "authdigest": "44", "overwintered": true, "version": 5, "versiongroupid": "26a7270a",
        "locktime": 0, "expiryheight": 3120040,
        "blockhash": "0000000000d8a9ac4e30b7a7ce2bd2b3b1b9c67b6ec23f5a3e7ba7ca1d4ab501",
        "blocktime": 1761200000
    }"#;

    #[test]
    fn a_transaction_decodes_from_zebrad_json() {
        let tx: Tx = serde_json::from_str(TX).expect("tx");
        assert!(!tx.is_coinbase());
        assert_eq!(tx.sapling_spends, Count(1));
        assert_eq!(tx.sapling_outputs, Count(2));
        assert_eq!(tx.orchard.map(|bundle| bundle.actions), Some(Count(2)));
        assert_eq!(tx.net_out(), [0, 25_000_000, -100_000_000, 0]);
        assert_eq!(tx.touches(), [false, true, true, false]);
        assert_eq!(tx.transparent_out(), 50_000_000);
        assert_eq!(tx.vin[0].vout, Some(1));
        assert_eq!(tx.vout[0].script.kind, "pubkeyhash");
        assert_eq!(tx.expiry_height, Some(3_120_040));
        // Scripts are kept as their lengths: one byte of scriptSig, two of scriptPubKey.
        assert_eq!(tx.vin[0].script_sig.map(|sig| sig.hex), Some(HexLen(1)));
        assert_eq!(tx.vin[0].serialized_size(), 36 + 1 + 1 + 4);
        assert_eq!(tx.vout[0].script.hex, HexLen(2));
        assert_eq!(tx.vout[0].serialized_size(), 8 + 1 + 2);
        assert_eq!(tx.fee, None, "getrawtransaction gives no fee");
        assert_eq!(tx.vin[0].prevout, None);
        // A coinbase input and a mempool transaction: no height, no block.
        let coinbase: Tx = serde_json::from_str(
            r#"{"txid":"3333333333333333333333333333333333333333333333333333333333333333",
                "vin":[{"coinbase":"03aabbcc","sequence":0}],"vout":[],"vShieldedSpend":[],
                "vShieldedOutput":[],"vjoinsplit":[],"version":5,"locktime":0}"#,
        )
        .expect("coinbase");
        assert!(coinbase.is_coinbase());
        assert_eq!(coinbase.height, None);
        assert_eq!(coinbase.block_hash, None);
    }

    #[test]
    fn shapes_survive_their_own_json() {
        let tx: Tx = serde_json::from_str(TX).expect("tx");
        let back: Tx = serde_json::from_value(serde_json::to_value(&tx).unwrap()).unwrap();
        assert_eq!(back, tx);
        let block: BlockV1 = serde_json::from_str(BLOCK).expect("block");
        let back: BlockV1 = serde_json::from_value(serde_json::to_value(&block).unwrap()).unwrap();
        assert_eq!(back, block);
        let json = serde_json::to_value(Count(2)).unwrap();
        assert_eq!(json, serde_json::json!([{}, {}]));
    }

    /// A block at verbosity 3 as zebrad 7.0.0 writes it: every transaction in full, an input
    /// with the output it spends (`prevout`, value in ZEC only), and the fee.
    const BLOCK_V3: &str = r#"{
        "hash": "0000000000d8a9ac4e30b7a7ce2bd2b3b1b9c67b6ec23f5a3e7ba7ca1d4ab501",
        "confirmations": 3, "size": 1200, "height": 3120000, "version": 4,
        "tx": [
            {"hex": "0500", "height": 3120000, "confirmations": 3,
             "txid": "1111111111111111111111111111111111111111111111111111111111111111",
             "vin": [{"coinbase": "03807b2f00", "sequence": 0}],
             "vout": [{"value": 1.25, "valueZat": 125000000, "n": 0,
                       "scriptPubKey": {"asm": "", "hex": "76a914", "type": "pubkeyhash",
                                        "addresses": ["t1abc"]}}],
             "vShieldedSpend": [], "vShieldedOutput": [], "vjoinsplit": [],
             "version": 5, "locktime": 0},
            {"hex": "0500", "height": 3120000, "confirmations": 3, "fee": 0.0001,
             "txid": "2222222222222222222222222222222222222222222222222222222222222222",
             "vin": [{"txid": "3333333333333333333333333333333333333333333333333333333333333333",
                      "vout": 0, "scriptSig": {"asm": "", "hex": "0000"}, "sequence": 1,
                      "prevout": {"generated": true, "height": 3119000, "value": 2.5,
                                  "scriptPubKey": {"asm": "", "hex": "76a9", "type": "pubkeyhash",
                                                   "addresses": ["t1def"]}}}],
             "vout": [{"value": 2.4999, "valueZat": 249990000, "n": 0,
                       "scriptPubKey": {"asm": "", "hex": "76a9", "type": "pubkeyhash"}}],
             "vShieldedSpend": [], "vShieldedOutput": [], "vjoinsplit": [],
             "version": 5, "locktime": 0}
        ],
        "time": 1761200000, "nonce": "00ff", "solution": "0123", "bits": "1c01ad18",
        "difficulty": 1.0, "trees": {}
    }"#;

    #[test]
    fn a_block_at_verbosity_3_decodes_with_prevouts_and_fees() {
        let block: BlockV3 = serde_json::from_str(BLOCK_V3).expect("block v3");
        assert_eq!(block.height, Some(3_120_000));
        assert_eq!(block.tx.len(), 2);
        let coinbase = &block.tx[0];
        assert!(coinbase.is_coinbase());
        assert_eq!(coinbase.fee, None);
        assert_eq!(coinbase.vin[0].serialized_size(), 36 + 1 + 5 + 4);
        let spending = &block.tx[1];
        assert_eq!(spending.fee, Some(10_000), "0.0001 ZEC");
        let prevout = spending.vin[0].prevout.as_ref().expect("a prevout");
        assert!(prevout.generated);
        assert_eq!(prevout.height, 3_119_000);
        assert_eq!(prevout.value_zat, 250_000_000);
        assert_eq!(
            prevout.script.addresses.as_deref(),
            Some(&["t1def".to_owned()][..])
        );
        // And back through its own JSON, as the synthetic chain writes it.
        let back: BlockV3 = serde_json::from_value(serde_json::to_value(&block).unwrap()).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn a_subsidy_decodes_from_zebrad_json() {
        let subsidy: Subsidy = serde_json::from_str(
            r#"{"fundingstreams":[{"recipient":"Zcash Community Grants","specification":"https://zips.z.cash/zip-1015",
                 "value":0.125,"valueZat":12500000,"address":"t3cFfPt1Bcvgez9ZbMBFWeZsskxTkPzGCow"}],
                "lockboxstreams":[{"recipient":"Lockbox","specification":"https://zips.z.cash/zip-1015",
                 "value":0.1875,"valueZat":18750000}],
                "miner":1.25,"founders":0.0,"fundingstreamstotal":0.125,"lockboxtotal":0.1875,
                "totalblocksubsidy":1.5625}"#,
        )
        .expect("subsidy");
        assert_eq!(subsidy.funding_streams[0].value_zat, 12_500_000);
        assert_eq!(subsidy.lockbox_streams[0].address, None);
        assert_eq!(crate::zec_to_zat(subsidy.total), 156_250_000);
    }
}
