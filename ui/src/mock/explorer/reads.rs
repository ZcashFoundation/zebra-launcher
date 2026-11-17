//! The mock explorer's reads of the synthetic chain, named as the backend names them over
//! zebrad's RPC (method and parameters), so Show RPC lists what the backend would ask. Each
//! read is remembered as the backend's cache would keep it, so the same read again is "from
//! memory". Times and sizes are made up, but steady: they follow from the read, not from a
//! random number, so the native tests see the same ones.

use std::collections::HashSet;

use explorer_model::{
    shapes::{BlockV1, BlockV3, Header, Subsidy, Tx},
    synth::SynthChain,
};
use launcher_types::explorer::{BlockHash, BlockId, ReadOutcome, SourceCall, TxId};

/// Reads remembered before the memory starts again, as the backend's cache evicts.
const REMEMBERED: usize = 50_000;

pub(super) struct Reads<'a> {
    pub(super) chain: &'a SynthChain,
    seen: &'a mut HashSet<String>,
    sources: Vec<SourceCall>,
}

impl<'a> Reads<'a> {
    pub(super) fn new(chain: &'a SynthChain, seen: &'a mut HashSet<String>) -> Self {
        if seen.len() > REMEMBERED {
            seen.clear();
        }
        Self {
            chain,
            seen,
            sources: Vec::new(),
        }
    }

    /// The reads so far, in order.
    pub(super) fn sources(self) -> Vec<SourceCall> {
        self.sources
    }

    /// Notes a read of `method` with `params` that brought `bytes`, or found nothing.
    fn note(&mut self, method: &str, params: String, bytes: Option<u32>) {
        let key = format!("{method}{params}");
        let found = bytes.is_some();
        let cached = found && !self.seen.insert(key.clone());
        self.sources.push(SourceCall {
            method: method.to_owned(),
            ms: if cached { 0 } else { made_up_ms(&key) },
            params,
            bytes: if cached { None } else { bytes },
            cached,
            outcome: if found {
                ReadOutcome::Answered
            } else {
                ReadOutcome::NotFound
            },
        });
    }

    fn known(&self, method: &str, params: &str) -> bool {
        self.seen.contains(&format!("{method}{params}"))
    }

    /// `getblock <id> 1`.
    pub(super) fn block(&mut self, id: BlockId) -> Option<BlockV1> {
        let block = self.chain.block(id);
        let bytes = block
            .as_ref()
            .map(|block| 3_700 + 66 * block.tx.len() as u32);
        self.note("getblock", format!(r#"["{id}",1]"#), bytes);
        block
    }

    /// `getblock <hash> 3`.
    pub(super) fn block_v3(&mut self, hash: BlockHash) -> Option<BlockV3> {
        let block = self.chain.block_v3(BlockId::Hash(hash));
        let bytes = block.as_ref().map(|block| {
            block
                .tx
                .iter()
                .map(|tx| 1_200 + 4 * tx.size.unwrap_or(0) as u32)
                .sum::<u32>()
        });
        self.note("getblock", format!(r#"["{hash}",3]"#), bytes);
        block
    }

    /// Whether the backend's cache would hold block `hash` at verbosity 3.
    pub(super) fn has_v3(&self, hash: BlockHash) -> bool {
        self.known("getblock", &format!(r#"["{hash}",3]"#))
    }

    /// `getrawtransaction <txid> 1`.
    pub(super) fn tx(&mut self, txid: &TxId) -> Option<Tx> {
        let tx = self.chain.tx(txid);
        let bytes = tx.as_ref().map(|tx| 600 + 2 * tx.size.unwrap_or(0) as u32);
        self.note("getrawtransaction", format!(r#"["{txid}",1]"#), bytes);
        tx
    }

    /// Whether the backend's cache would hold transaction `txid`.
    pub(super) fn has_tx(&self, txid: &TxId) -> bool {
        self.known("getrawtransaction", &format!(r#"["{txid}",1]"#))
    }

    /// Transaction `txid` read in the background, as the backend fills its cache: not one of
    /// the answer's reads.
    pub(super) fn fill_tx(&mut self, txid: &TxId) {
        self.seen
            .insert(format!(r#"getrawtransaction["{txid}",1]"#));
    }

    /// The reads the Block page makes of block `id`, in the background.
    pub(super) fn fill_block_page(&mut self, id: BlockId) {
        let Some(block) = self.chain.block(id) else {
            return;
        };
        self.seen.insert(format!(r#"getblock["{id}",1]"#));
        if let Some(parent) = block.previous {
            self.seen.insert(format!(r#"getblock["{parent}",1]"#));
        }
        if let Some(coinbase) = block.tx.first() {
            self.fill_tx(coinbase);
        }
        let height = block.height.unwrap_or(0);
        self.seen.insert(format!("getblocksubsidy[{height}]"));
    }

    /// `getblocksubsidy <height>`.
    pub(super) fn subsidy(&mut self, height: u32) -> Subsidy {
        self.note("getblocksubsidy", format!("[{height}]"), Some(640));
        self.chain.subsidy(height)
    }

    /// `getblockheader <id> true`.
    pub(super) fn header(&mut self, id: BlockId) -> Option<Header> {
        let header = self.chain.header(id);
        let bytes = header.as_ref().map(|_| 430);
        self.note("getblockheader", format!(r#"["{id}",true]"#), bytes);
        header
    }

    /// `getbestblockheightandhash`: the mock's tip is always fresh.
    pub(super) fn best(&mut self) {
        self.note("getbestblockheightandhash", "[]".to_owned(), Some(110));
    }
}

/// A steady, made-up time for a read: 3 to 40 ms.
fn made_up_ms(key: &str) -> u32 {
    let hash = key.bytes().fold(0x811c_9dc5u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    3 + hash % 38
}

#[cfg(test)]
mod tests {
    use launcher_types::Network;

    use super::*;

    #[test]
    fn a_read_again_is_from_memory() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, 1_800_000_000);
        let mut seen = HashSet::new();
        let mut reads = Reads::new(&chain, &mut seen);
        let block = reads.block(BlockId::Height(3_550_000)).expect("a block");
        reads.tx(&block.tx[0]);
        reads.block(BlockId::Height(3_550_000));
        assert!(
            reads
                .tx(&launcher_types::explorer::Hash32([7; 32]))
                .is_none()
        );
        let sources = reads.sources();
        assert_eq!(sources.len(), 4);
        assert_eq!(sources[0].params, r#"["3550000",1]"#);
        assert!(!sources[0].cached && sources[0].bytes.is_some() && sources[0].ms > 0);
        assert!(sources[2].cached && sources[2].ms == 0 && sources[2].bytes.is_none());
        assert_eq!(sources[3].outcome, ReadOutcome::NotFound);
        // The memory outlives one answer.
        let mut reads = Reads::new(&chain, &mut seen);
        assert!(reads.has_tx(&block.tx[0]));
        reads.fill_tx(&block.tx[1]);
        assert!(reads.has_tx(&block.tx[1]));
        assert!(
            reads.sources().is_empty(),
            "a background read is not listed"
        );
    }
}
