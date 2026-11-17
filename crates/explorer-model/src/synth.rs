//! A synthetic chain in zebrad's shapes, for the browser mock and the explorer's tests. It is
//! not Zcash's history: it is a chain that follows Zcash's rules closely enough that the
//! explorer's own builders run over it unchanged.
//!
//! Every block is a pure function of the network, its height and the branch it is on, and is
//! built in constant time. Pool balances and note-tree sizes follow trends by era with bounded
//! seeded noise; a block's transactions are then built to realise exactly the change from its
//! parent: its coinbase claims the subsidy and the fees (less the burned share from NU7), and
//! its other transactions move the rest. So every block passes [`crate::ledger::ledger_check`],
//! and these hold at every height:
//!
//! - no pool is negative; Ironwood is empty before NU6.3, Orchard before NU5;
//! - Sprout takes no new value from Canopy and has no JoinSplits from NU7;
//! - from NU6.3 no Orchard value balance is negative and no coinbase pays into Orchard;
//! - the lockbox is funded from NU6, and 78,750 ZEC leave it at NU6.1 (Mainnet);
//! - from NU7 ⌊6 F / 10⌋ of a block's fees F burn, and the shielded budget stays under 330.
//!
//! Every transparent input spends an output the chain has, worth what the transaction's fee
//! needs: a root, which is an output of a coinbase or of a transaction without transparent
//! inputs, in an older block (100 blocks back, where coinbase outputs mature; near the chain's
//! start, the block's own coinbase). Roots are found without looking further back, so a
//! block stays cheap to build. Nothing checks that an output is spent only once.
//!
//! Every transaction pays for its logical actions as ZIP 317 counts them, inputs and leftover
//! notes included: what a wallet pays an action in its era (5,000 zatoshis on Mainnet until
//! 3,590,000 and on Testnet until NU7, 1,000 after) when the block's fees cover that, else
//! zebrad's conventional 1,000; the transparent payments pay the rest of the block's fees. The
//! first payment of every hundredth block pays half of zebrad's conventional fee, as a block
//! may hold what zebrad's mempool would not, so the explorer's "paid less" shows.
//!
//! With [`SynthChain::with_big_blocks`], every so many blocks hold 4,600 more transparent
//! payments, nearly 2 MB in all and under zebrad's limit of 2,000,000 bytes.
//!
//! Block hashes and transaction ids encode their height, branch and position, so lookups need
//! no index. Addresses are made-up strings in the right shape: never real ones.

use launcher_types::{
    Network, NetworkUpgrade, UpgradeStatus,
    explorer::{BlockHash, BlockId, Hash32, LogicalActions, Pool, TxId},
};

use crate::{
    ledger::burned_share,
    shapes::{
        Actions, BestBlock, BlockV1, BlockV3, Count, Header, HexLen, JoinSplit, PoolValue, Prevout,
        ScriptPubKey, ScriptSig, Stream, Subsidy, TreeSize, Trees, Tx, TxIn, TxOut,
    },
    upgrades::{Upgrade, UpgradeHeights},
    zip317,
};

const ZEC: i128 = 100_000_000;

/// The subsidy before Blossom, in zatoshis.
const FIRST_SUBSIDY: u64 = 1_250_000_000;
/// Blocks between halvings after Blossom.
const HALVING_INTERVAL: u32 = 1_680_000;
/// The lockbox disbursement at NU6.1 on Mainnet (subsidy/constants/mainnet.rs:19-34).
const NU6_1_DISBURSEMENT: u64 = 7_875_000_000_000;

/// The burned fees per block from NU7, on average, and their spread.
const BURN_BASE: u64 = 270_000;
const BURN_SPREAD: u64 = 100_000;

const KIND_BLOCK: u64 = 1;
const KIND_TX: u64 = 2;
const KIND_PLAN: u64 = 3;
const KIND_TIME: u64 = 4;
const KIND_FEES: u64 = 5;
const KIND_BURN: u64 = 6;
const KIND_FUNDS: u64 = 7;
const KIND_POOL: u64 = 10;
const KIND_TREE: u64 = 20;
const KIND_FIELD: u64 = 30;

const BLOCK_HEIGHT_KEY: u32 = 0x5a3c_96e1;
const BRANCH_KEY: u16 = 0x9e37;
const TX_HEIGHT_KEY: u32 = 0xc3a5_5a3c;
const TX_INDEX_KEY: u16 = 0x7f4a;

/// The header and solution of a block, in bytes, before its transactions.
const HEADER_BYTES: i64 = 1_487;

/// A pay-to-public-key-hash input's unlocking script, in bytes: a signature and a public key.
const P2PKH_SCRIPT_SIG: u32 = 107;
/// The inputs a shielding transaction is taken to have before it is funded, when the block's
/// fees are shared out.
const SHIELD_INPUTS_GUESS: usize = 4;

/// The transactions a big block holds beyond its plan ([`SynthChain::with_big_blocks`]): with
/// the largest plans, about 200 KB, the block stays under 2,000,000 bytes.
const FILLERS: u32 = 4_600;
/// What each of them pays.
const FILLER_FEE: i64 = 10_000;
/// And its size: two inputs and two outputs, all pay-to-public-key-hash.
const FILLER_BYTES: i64 = 10 + 2 * 148 + 2 * 34;

/// A synthetic chain: its tip, the tip's time, and the reorganisations it went through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SynthChain {
    network: Network,
    heights: UpgradeHeights,
    tip: u32,
    tip_time: i64,
    /// Reorganisations in order: the first height of each new branch. Branch `n` is the
    /// `n`-th entry; branch 0 is the original chain.
    forks: Vec<u32>,
    /// Every block at a height `h` with `h % n == n - 1` lacks zebrad's value-pool row.
    missing_rows: Option<u32>,
    /// Every block at a non-zero height `h` with `h % n == 0` holds [`FILLERS`] more
    /// transactions.
    big_blocks: Option<u32>,
}

impl SynthChain {
    /// A chain on `network` whose tip is `tip`, mined at `tip_time` (seconds since the Unix
    /// epoch), with zebrad 7.0.0's upgrade heights.
    pub fn new(network: Network, tip: u32, tip_time: i64) -> Self {
        Self {
            network,
            heights: UpgradeHeights::defaults(network),
            tip,
            tip_time,
            forks: Vec::new(),
            missing_rows: None,
            big_blocks: None,
        }
    }

    /// The same chain with every `every`-th block lacking zebrad's value-pool row
    /// (`BlockInfo`), so its parent's successor shows the zero-diff case.
    pub fn with_missing_rows(mut self, every: u32) -> Self {
        self.missing_rows = (every > 1).then_some(every);
        self
    }

    /// The same chain with every `every`-th block holding 4,600 more transparent payments,
    /// nearly 2 MB in all: a block too big to read whole at once.
    pub fn with_big_blocks(mut self, every: u32) -> Self {
        self.big_blocks = (every > 0).then_some(every);
        self
    }

    /// Whether the block at `height` is one of the big ones.
    pub fn is_big(&self, height: u32) -> bool {
        self.fillers(height) > 0
    }

    pub fn network(&self) -> Network {
        self.network
    }

    pub fn heights(&self) -> &UpgradeHeights {
        &self.heights
    }

    pub fn tip(&self) -> u32 {
        self.tip
    }

    pub fn tip_time(&self) -> i64 {
        self.tip_time
    }

    /// The last height each reorganisation kept, oldest first.
    pub fn reorgs(&self) -> impl Iterator<Item = u32> + '_ {
        self.forks.iter().map(|start| start.saturating_sub(1))
    }

    /// Adds `blocks` blocks at the target spacing; earlier blocks keep their times.
    pub fn mine(&mut self, blocks: u32) {
        let new_tip = self.tip.saturating_add(blocks);
        self.tip_time += self.heights.spacing_between(self.tip, new_tip) as i64;
        self.tip = new_tip;
    }

    /// Replaces the top `depth` blocks with blocks of a new branch, at the same heights.
    /// Returns the last height both branches share.
    pub fn reorg(&mut self, depth: u32) -> u32 {
        let depth = depth.clamp(1, self.tip.max(1));
        let start = self.tip + 1 - depth;
        if self.forks.len() < usize::from(u16::MAX) {
            self.forks.push(start);
        }
        start - 1
    }

    /// The branch the best chain has at `height`.
    pub fn branch(&self, height: u32) -> u16 {
        branch_among(&self.forks, height)
    }

    /// The best chain's block hash at `height`, up to the tip.
    pub fn hash(&self, height: u32) -> Option<BlockHash> {
        (height <= self.tip).then(|| self.block_hash(height, self.branch(height)))
    }

    /// `getbestblockheightandhash`.
    pub fn best(&self) -> BestBlock {
        BestBlock {
            height: self.tip,
            hash: self.block_hash(self.tip, self.branch(self.tip)),
        }
    }

    /// `getblock <id> 1`: a block of the best chain, as zebrad looks blocks up.
    pub fn block(&self, id: BlockId) -> Option<BlockV1> {
        let height = self.best_height(id)?;
        Some(self.block_v1(height, self.branch(height)))
    }

    /// `getblockheader <id> true`.
    pub fn header(&self, id: BlockId) -> Option<Header> {
        let height = self.best_height(id)?;
        Some(Header {
            hash: self.block_hash(height, self.branch(height)),
            height,
            time: self.time(height, self.branch(height)),
        })
    }

    /// `getrawtransaction <txid> 1`: a transaction of a block on the best chain, or of a block
    /// a reorganisation replaced (height -1, as zebrad reports side chains).
    pub fn tx(&self, txid: &TxId) -> Option<Tx> {
        let (height, branch, index) = self.decode_txid(txid)?;
        if height > self.tip || usize::from(branch) > self.forks.len() {
            return None;
        }
        let best = self.branch(height) == branch;
        if !best && branch > 0 && height < self.forks[usize::from(branch) - 1] {
            return None;
        }
        let mut tx = self.built(height, branch, index)?.tx;
        self.place(&mut tx, height, branch);
        Some(tx)
    }

    /// `getblock <hash> 3`: a block of the best chain with its transactions in full, each
    /// input with the output it spends and each transaction but the coinbase with its fee.
    pub fn block_v3(&self, id: BlockId) -> Option<BlockV3> {
        let height = self.best_height(id)?;
        let branch = self.branch(height);
        let mut built = self.plan(height, branch);
        let base = built.len() as u16;
        for index in base..base + self.fillers(height) as u16 {
            built.push(self.filler(height, branch, index));
        }
        let tx = built
            .into_iter()
            .enumerate()
            .map(|(index, built)| {
                let mut tx = built.tx;
                self.place(&mut tx, height, branch);
                for (input, root) in tx.vin.iter_mut().zip(&built.spent) {
                    input.prevout = Some(root.prevout());
                }
                tx.fee = (index > 0).then_some(built.fee);
                tx
            })
            .collect();
        Some(BlockV3 {
            hash: self.block_hash(height, branch),
            confirmations: i64::from(self.tip - height + 1),
            height: Some(height),
            tx,
        })
    }

    /// Where a transaction of the block at `height` on `branch` is, as zebrad reports it: in
    /// the best chain, or at height -1 in a side chain.
    fn place(&self, tx: &mut Tx, height: u32, branch: u16) {
        if self.branch(height) == branch {
            tx.height = Some(i64::from(height));
            tx.confirmations = Some(i64::from(self.tip - height + 1));
        } else {
            tx.height = Some(-1);
            tx.confirmations = Some(0);
        }
        let time = self.time(height, branch);
        tx.block_hash = Some(self.block_hash(height, branch));
        tx.block_time = Some(time);
        tx.time = Some(time);
    }

    /// The fee a transaction of the best chain paid, which zebrad does not report and the
    /// explorer can only work out from the values of the outputs it spends. `None` for a
    /// coinbase.
    pub fn fee(&self, txid: &TxId) -> Option<i64> {
        let (height, branch, index) = self.decode_txid(txid)?;
        if index == 0 || height > self.tip || self.branch(height) != branch {
            return None;
        }
        self.built(height, branch, index).map(|built| built.fee)
    }

    /// `getblocksubsidy <height>`.
    pub fn subsidy(&self, height: u32) -> Subsidy {
        let split = self.split(height);
        let zec = |zat: u64| zat as f64 / ZEC as f64;
        Subsidy {
            funding_streams: split
                .streams
                .iter()
                .map(|(recipient, address, zat)| Stream {
                    recipient: (*recipient).to_owned(),
                    value_zat: *zat as i64,
                    address: Some(address.clone()),
                })
                .collect(),
            lockbox_streams: (split.lockbox > 0)
                .then(|| Stream {
                    recipient: "Lockbox".to_owned(),
                    value_zat: split.lockbox as i64,
                    address: None,
                })
                .into_iter()
                .collect(),
            miner: zec(split.miner),
            founders: zec(split.founders),
            total: zec(split.total),
        }
    }

    /// The pool balances after the best chain's block at `height`, in zebrad's order.
    pub fn pools(&self, height: u32) -> [u64; 6] {
        self.balances(height, self.branch(height))
    }

    /// The supply after the block at `height`: the subsidies so far, less the burned fees.
    pub fn supply(&self, height: u32) -> u64 {
        self.cumulative_subsidy(height) - self.cumulative_burn(height)
    }

    /// The upgrades as a zebrad with blocks up to `height` reports them.
    pub fn upgrades(&self, height: u32) -> Vec<NetworkUpgrade> {
        Upgrade::ALL
            .into_iter()
            .filter_map(|upgrade| {
                let start = self.heights.height(upgrade)?;
                Some(NetworkUpgrade {
                    name: upgrade.name().to_owned(),
                    branch_id: branch_id(upgrade).to_owned(),
                    activation_height: u64::from(start),
                    status: if start <= height {
                        UpgradeStatus::Active
                    } else {
                        UpgradeStatus::Pending
                    },
                })
            })
            .collect()
    }

    /// The consensus branch id (hex) of the block at `height`.
    pub fn branch_id_at(&self, height: u32) -> &'static str {
        self.heights.current(height).map_or("00000000", branch_id)
    }

    /// The best chain's block time at `height`.
    pub fn time_at(&self, height: u32) -> i64 {
        self.time(height, self.branch(height))
    }

    /// The network difficulty at `height`.
    pub fn difficulty(&self, height: u32) -> f64 {
        let base = match self.network {
            Network::Mainnet => 118_000_000.0,
            Network::Testnet => 42.0,
        };
        let wobble = (self.noise(KIND_FIELD, height, 0, 1_000) as f64 - 500.0) / 20_000.0;
        // In sixteenths, so the decimal text zebrad's JSON carries reads back exactly.
        (base * (1.0 + wobble) * 16.0).round() / 16.0
    }

    fn best_height(&self, id: BlockId) -> Option<u32> {
        match id {
            BlockId::Height(height) => (height <= self.tip).then_some(height),
            BlockId::Hash(hash) => {
                let (height, branch) = self.decode_block_hash(&hash)?;
                (height <= self.tip && self.branch(height) == branch).then_some(height)
            }
        }
    }

    /// The branch of the parent of the block at `height` on `branch`.
    fn parent_branch(&self, height: u32, branch: u16) -> u16 {
        if branch == 0 {
            return 0;
        }
        let start = self.forks[usize::from(branch) - 1];
        if height > start {
            branch
        } else {
            branch_among(
                &self.forks[..usize::from(branch) - 1],
                height.saturating_sub(1),
            )
        }
    }

    fn net_tag(&self) -> u64 {
        match self.network {
            Network::Mainnet => 1,
            Network::Testnet => 2,
        }
    }

    fn seed(&self, kind: u64, height: u32, branch: u16, extra: u64) -> u64 {
        [
            self.net_tag(),
            kind,
            u64::from(height),
            u64::from(branch),
            extra,
        ]
        .into_iter()
        .fold(0x5eed_cafe_f00d_u64, |acc, part| mix(acc ^ part))
    }

    /// Seeded noise in `0..=amp`.
    fn noise(&self, kind: u64, height: u32, branch: u16, amp: u64) -> u64 {
        if amp == 0 {
            return 0;
        }
        self.seed(kind, height, branch, 0) % (amp + 1)
    }

    fn block_hash(&self, height: u32, branch: u16) -> BlockHash {
        let mut bytes = [0u8; 32];
        bytes[4..8].copy_from_slice(&(height ^ BLOCK_HEIGHT_KEY).to_be_bytes());
        bytes[8..10].copy_from_slice(&(branch ^ BRANCH_KEY).to_be_bytes());
        fill(&mut bytes[10..], self.seed(KIND_BLOCK, height, branch, 0));
        Hash32(bytes)
    }

    fn decode_block_hash(&self, hash: &BlockHash) -> Option<(u32, u16)> {
        if hash.0[..4] != [0; 4] {
            return None;
        }
        let height = u32::from_be_bytes(hash.0[4..8].try_into().ok()?) ^ BLOCK_HEIGHT_KEY;
        let branch = u16::from_be_bytes(hash.0[8..10].try_into().ok()?) ^ BRANCH_KEY;
        (self.block_hash(height, branch) == *hash).then_some((height, branch))
    }

    /// A transaction id: seeded bytes, with the height, branch and position in the middle, so
    /// ids of one block do not share their first digits.
    fn txid(&self, height: u32, branch: u16, index: u16) -> TxId {
        let mut bytes = [0u8; 32];
        fill(
            &mut bytes,
            self.seed(KIND_TX, height, branch, u64::from(index)),
        );
        bytes[12..16].copy_from_slice(&(height ^ TX_HEIGHT_KEY).to_be_bytes());
        bytes[16..18].copy_from_slice(&(branch ^ BRANCH_KEY).to_be_bytes());
        bytes[18..20].copy_from_slice(&(index ^ TX_INDEX_KEY).to_be_bytes());
        Hash32(bytes)
    }

    fn decode_txid(&self, txid: &TxId) -> Option<(u32, u16, u16)> {
        let height = u32::from_be_bytes(txid.0[12..16].try_into().ok()?) ^ TX_HEIGHT_KEY;
        let branch = u16::from_be_bytes(txid.0[16..18].try_into().ok()?) ^ BRANCH_KEY;
        let index = u16::from_be_bytes(txid.0[18..20].try_into().ok()?) ^ TX_INDEX_KEY;
        (self.txid(height, branch, index) == *txid).then_some((height, branch, index))
    }

    /// The block time: the tip's time less the target spacing back to the block, and up to two
    /// thirds of the block's spacing earlier, so no block is stamped after the tip's time.
    fn time(&self, height: u32, branch: u16) -> i64 {
        let spacing = i64::from(self.heights.target_spacing(height));
        let base = if height <= self.tip {
            self.tip_time - self.heights.spacing_between(height, self.tip) as i64
        } else {
            self.tip_time + self.heights.spacing_between(self.tip, height) as i64
        };
        let jitter = self.noise(KIND_TIME, height, branch, (2 * spacing / 3) as u64) as i64;
        base - jitter
    }

    fn at(&self, upgrade: Upgrade) -> u32 {
        self.heights.height(upgrade).unwrap_or(u32::MAX)
    }

    /// The heights where the subsidy changes: Blossom, the halvings and NU7.
    fn subsidy_edges(&self) -> Vec<u32> {
        let blossom = self.at(Upgrade::Blossom);
        let first = blossom.saturating_add(850_000u32.saturating_sub(blossom).saturating_mul(2));
        let mut edges = vec![
            1,
            blossom,
            first,
            first.saturating_add(HALVING_INTERVAL),
            first.saturating_add(2 * HALVING_INTERVAL),
            self.at(Upgrade::Nu7),
            self.at(Upgrade::Nu6),
            self.at(Upgrade::Canopy),
        ];
        edges.sort_unstable();
        edges.dedup();
        edges
    }

    /// The block subsidy at `height`: 12.5 ZEC, halved at Blossom (the spacing halves), at
    /// each halving, and divided by three at NU7, whose blocks come three times as often. The
    /// slow start is left out.
    fn block_subsidy(&self, height: u32) -> u64 {
        if height == 0 {
            return 0;
        }
        let blossom = self.at(Upgrade::Blossom);
        let first = blossom.saturating_add(850_000u32.saturating_sub(blossom).saturating_mul(2));
        let mut subsidy = if height < blossom {
            FIRST_SUBSIDY
        } else {
            FIRST_SUBSIDY / 2
        };
        for halving in [
            first,
            first.saturating_add(HALVING_INTERVAL),
            first.saturating_add(2 * HALVING_INTERVAL),
        ] {
            if height >= halving {
                subsidy /= 2;
            }
        }
        if self.heights.is_active(Upgrade::Nu7, height) {
            subsidy /= 3;
        }
        subsidy
    }

    /// The sum of `per_block` over `1..=height`, for a function that changes only at
    /// [`SynthChain::subsidy_edges`].
    fn cumulative(&self, height: u32, per_block: impl Fn(u32) -> u64) -> u64 {
        let mut edges = self.subsidy_edges();
        edges.retain(|edge| *edge >= 1);
        let mut total = 0u64;
        for (index, start) in edges.iter().enumerate() {
            if *start > height {
                break;
            }
            let end = edges
                .get(index + 1)
                .map_or(height, |next| (next - 1).min(height));
            total += per_block(*start) * u64::from(end - start + 1);
        }
        total
    }

    fn cumulative_subsidy(&self, height: u32) -> u64 {
        self.cumulative(height, |at| self.block_subsidy(at))
    }

    /// How the subsidy at `height` splits.
    fn split(&self, height: u32) -> Split {
        let total = self.block_subsidy(height);
        let canopy = self.at(Upgrade::Canopy);
        let blossom = self.at(Upgrade::Blossom);
        let first_halving =
            blossom.saturating_add(850_000u32.saturating_sub(blossom).saturating_mul(2));
        let percent = |share: u64| total * share / 100;
        let mut split = Split {
            total,
            miner: 0,
            founders: 0,
            streams: Vec::new(),
            lockbox: 0,
        };
        if height < canopy.min(first_halving) {
            split.founders = percent(20);
        } else if !self.heights.is_active(Upgrade::Nu6, height) {
            split.streams = vec![
                ("Electric Coin Company", self.fund_address(1), percent(7)),
                ("Zcash Foundation", self.fund_address(2), percent(5)),
                ("Major Grants", self.fund_address(3), percent(8)),
            ];
        } else {
            split.streams = vec![("Zcash Community Grants", self.fund_address(4), percent(8))];
            split.lockbox = percent(12);
        }
        split.miner = total
            - split.founders
            - split.lockbox
            - split.streams.iter().map(|stream| stream.2).sum::<u64>();
        split
    }

    /// The lockbox after the block at `height`: the deferrals since NU6, less the NU6.1
    /// disbursement.
    fn lockbox(&self, height: u32) -> u64 {
        let nu6 = self.at(Upgrade::Nu6);
        if height < nu6 {
            return 0;
        }
        let deferral = |at: u32| {
            if self.heights.is_active(Upgrade::Nu6, at) {
                self.block_subsidy(at) * 12 / 100
            } else {
                0
            }
        };
        let deferred = self.cumulative(height, deferral);
        deferred - self.disbursed(height)
    }

    /// What has left the lockbox by `height`.
    fn disbursed(&self, height: u32) -> u64 {
        let nu6_1 = self.at(Upgrade::Nu6_1);
        if height < nu6_1 {
            return 0;
        }
        let deferral = |at: u32| {
            if self.heights.is_active(Upgrade::Nu6, at) {
                self.block_subsidy(at) * 12 / 100
            } else {
                0
            }
        };
        NU6_1_DISBURSEMENT.min(self.cumulative(nu6_1 - 1, deferral))
    }

    /// The fees burned up to `height`.
    fn cumulative_burn(&self, height: u32) -> u64 {
        let nu7 = self.at(Upgrade::Nu7);
        if height < nu7 {
            return 0;
        }
        // A big block's payments burn their share too.
        let big = match self.big_blocks {
            Some(every) => u64::from(height / every - (nu7 - 1) / every),
            None => 0,
        };
        BURN_BASE * u64::from(height - nu7 + 1)
            + self.noise(KIND_BURN, height, 0, BURN_SPREAD)
            + big * burned_share((FILLER_FEE * i64::from(FILLERS)) as u64)
    }

    fn burned(&self, height: u32) -> u64 {
        self.cumulative_burn(height)
            - self.cumulative_burn(height.saturating_sub(1)) * u64::from(height > 0)
    }

    /// The fees the block's transactions pay. From NU7 they are the smallest whose burned
    /// share is the block's burn.
    fn fees(&self, height: u32, branch: u16) -> u64 {
        if self.heights.is_active(Upgrade::Nu7, height) {
            (10 * self.burned(height)).div_ceil(6)
        } else if height == 0 {
            0
        } else {
            250_000
                + self.noise(KIND_FEES, height, branch, 300_000)
                + self.filler_fees(height) as u64
        }
    }

    /// The transactions the block at `height` holds beyond its plan.
    fn fillers(&self, height: u32) -> u32 {
        match self.big_blocks {
            Some(every) if height > 0 && height.is_multiple_of(every) => FILLERS,
            _ => 0,
        }
    }

    /// What they pay.
    fn filler_fees(&self, height: u32) -> i64 {
        FILLER_FEE * i64::from(self.fillers(height))
    }

    /// Mainnet amounts as they are; Testnet's shielded pools are a tenth.
    fn scale(&self, zat: i128) -> i128 {
        match self.network {
            Network::Mainnet => zat,
            Network::Testnet => zat / 10,
        }
    }

    fn pool_noise(&self, pool: Pool, height: u32, branch: u16, amp: i128) -> i128 {
        self.noise(
            KIND_POOL + pool.index() as u64,
            height,
            branch,
            u64::try_from(amp.max(0)).unwrap_or(0),
        ) as i128
    }

    /// A shielded pool's balance after the block at `height` on `branch`.
    fn shielded(&self, pool: Pool, height: u32, branch: u16) -> u64 {
        let (sapling, canopy, nu5, nu6_3, nu7) = (
            self.at(Upgrade::Sapling),
            self.at(Upgrade::Canopy),
            self.at(Upgrade::Nu5),
            self.at(Upgrade::Nu6_3),
            self.at(Upgrade::Nu7),
        );
        let noisy = |trend: i128, amp: i128| {
            trend + self.pool_noise(pool, height, branch, amp.min(trend / 64))
        };
        let value = match pool {
            Pool::Sprout => {
                if height < sapling {
                    noisy(lerp(height, 0, 0, sapling, 700_000 * ZEC), 40 * ZEC)
                } else if height < canopy {
                    noisy(
                        lerp(height, sapling, 700_000 * ZEC, canopy, 30_000 * ZEC),
                        40 * ZEC,
                    )
                } else {
                    // Only outflows from Canopy, now and then; none from NU7.
                    let steps = (nu7.saturating_sub(canopy) / 16).max(1);
                    let step = 7_000 * ZEC / i128::from(steps);
                    let last = height.min(nu7.saturating_sub(1)).max(canopy);
                    30_000 * ZEC - step * i128::from((last - canopy) / 16)
                }
            }
            Pool::Sapling => {
                if height < sapling {
                    0
                } else if height < nu5 {
                    noisy(lerp(height, sapling, 0, nu5, 600_000 * ZEC), 60 * ZEC)
                } else if height < nu6_3 {
                    noisy(
                        lerp(height, nu5, 600_000 * ZEC, nu6_3, 450_000 * ZEC),
                        60 * ZEC,
                    )
                } else {
                    let trend =
                        (450_000 * ZEC - ZEC / 5 * i128::from(height - nu6_3)).max(100_000 * ZEC);
                    noisy(trend, 60 * ZEC)
                }
            }
            Pool::Orchard => {
                if height < nu5 {
                    0
                } else if height < nu6_3 {
                    noisy(lerp(height, nu5, 0, nu6_3, 2_100_000 * ZEC), 80 * ZEC)
                } else {
                    // Drain only from NU6.3.
                    let start = lerp(nu6_3 - 1, nu5, 0, nu6_3, 2_100_000 * ZEC);
                    let blocks = i128::from(height - nu6_3 + 1);
                    (start - 7 * ZEC / 2 * blocks - 3 * ZEC / 2 * (blocks / 3)).max(0)
                }
            }
            Pool::Ironwood => {
                if height < nu6_3 {
                    0
                } else {
                    noisy(3 * ZEC * i128::from(height - nu6_3 + 1), 30 * ZEC)
                }
            }
            Pool::Transparent | Pool::Lockbox => 0,
        };
        u64::try_from(self.scale(value).max(0)).unwrap_or(0)
    }

    /// Every pool's balance after the block at `height` on `branch`: the shielded pools by
    /// their trends, the lockbox exactly, and the transparent pool the rest of the supply.
    fn balances(&self, height: u32, branch: u16) -> [u64; 6] {
        let mut pools = [0u64; 6];
        for pool in Pool::SHIELDED {
            pools[pool.index()] = self.shielded(pool, height, branch);
        }
        pools[Pool::Lockbox.index()] = self.lockbox(height);
        let others: u64 = pools.iter().sum();
        pools[Pool::Transparent.index()] = self.supply(height).saturating_sub(others);
        pools
    }

    /// The note-tree sizes after the block at `height` on `branch`: Sapling, Orchard,
    /// Ironwood.
    fn trees(&self, height: u32, branch: u16) -> [u64; 3] {
        let (sapling, nu5, nu6_3) = (
            self.at(Upgrade::Sapling),
            self.at(Upgrade::Nu5),
            self.at(Upgrade::Nu6_3),
        );
        let wobble = |tree: u64, amp: u64| self.noise(KIND_TREE + tree, height, branch, amp);
        let sapling_size = if height < sapling {
            0
        } else {
            8 * u64::from(height - sapling + 1) + wobble(0, 5)
        };
        let orchard_size = if height < nu5 {
            0
        } else if height < nu6_3 {
            16 * u64::from(height - nu5 + 1) + wobble(1, 6)
        } else {
            16 * u64::from(nu6_3 - nu5) + 10 * u64::from(height - nu6_3 + 1) + wobble(1, 6)
        };
        let ironwood_size = if height < nu6_3 {
            0
        } else {
            20 * u64::from(height - nu6_3 + 1) + wobble(2, 14)
        };
        [sapling_size, orchard_size, ironwood_size]
    }

    fn has_row(&self, height: u32) -> bool {
        self.missing_rows
            .is_none_or(|every| height % every != every - 1)
    }

    /// The fee a wallet pays for `actions` logical actions.
    fn conventional_fee(&self, height: u32, actions: u32) -> i64 {
        let per_action = match self.network {
            Network::Mainnet if height >= 3_590_000 => 1_000,
            Network::Testnet if self.heights.is_active(Upgrade::Nu7, height) => 1_000,
            _ => 5_000,
        };
        per_action * i64::from(actions.max(2))
    }

    fn address(&self, index: u64) -> String {
        let prefix = match self.network {
            Network::Mainnet => "t1",
            Network::Testnet => "tm",
        };
        made_up_address(prefix, mix(index ^ (self.net_tag() << 40)))
    }

    fn fund_address(&self, stream: u64) -> String {
        let prefix = match self.network {
            Network::Mainnet => "t3",
            Network::Testnet => "t2",
        };
        made_up_address(prefix, mix(0xf00d ^ stream ^ (self.net_tag() << 40)))
    }

    /// The block at `height` on `branch`, at verbosity 1.
    fn block_v1(&self, height: u32, branch: u16) -> BlockV1 {
        let plan = self.plan(height, branch);
        let parent_branch = self.parent_branch(height, branch);
        let pools = self.balances(height, branch);
        let row = self.has_row(height);
        let parent = (height > 0).then(|| self.balances(height - 1, parent_branch));
        let parent_row = height == 0 || self.has_row(height - 1);
        let field = |which: u64| hex(self.seed(KIND_FIELD, height, branch, which));
        let fillers = self.fillers(height);
        let size = HEADER_BYTES
            + plan.iter().map(|built| built.size).sum::<i64>()
            + FILLER_BYTES * i64::from(fillers);
        let value_pools = row.then(|| {
            Pool::ALL
                .into_iter()
                .map(|pool| {
                    let after = pools[pool.index()];
                    // Like zebrad: against zero when only the parent lacks its row.
                    let before = match parent {
                        Some(parent) if parent_row => parent[pool.index()],
                        _ => 0,
                    };
                    PoolValue {
                        id: pool.id().to_owned(),
                        chain_value_zat: after,
                        value_delta_zat: Some(after as i64 - before as i64),
                    }
                })
                .collect()
        });
        let trees = self.trees(height, branch);
        let best = height <= self.tip && self.branch(height) == branch;
        BlockV1 {
            hash: self.block_hash(height, branch),
            confirmations: if best {
                i64::from(self.tip - height + 1)
            } else {
                -1
            },
            size: row.then_some(size),
            height: Some(height),
            version: Some(4),
            merkle_root: Some(field(1)),
            block_commitments: Some(field(2)),
            final_sapling_root: self
                .heights
                .is_active(Upgrade::Sapling, height)
                .then(|| field(3)),
            final_orchard_root: self
                .heights
                .is_active(Upgrade::Nu5, height)
                .then(|| field(4)),
            tx: (0..plan.len() + fillers as usize)
                .map(|index| self.txid(height, branch, index as u16))
                .collect(),
            time: Some(self.time(height, branch)),
            nonce: Some(field(5)),
            bits: Some(
                match self.network {
                    Network::Mainnet => "1c01ad18",
                    Network::Testnet => "1f07ffff",
                }
                .to_owned(),
            ),
            difficulty: Some(self.difficulty(height)),
            chain_supply: row.then(|| PoolValue {
                id: String::new(),
                chain_value_zat: pools.iter().sum(),
                value_delta_zat: None,
            }),
            value_pools,
            trees: Trees {
                sapling: TreeSize { size: trees[0] },
                orchard: TreeSize { size: trees[1] },
                ironwood: TreeSize { size: trees[2] },
            },
            previous: (height > 0).then(|| self.block_hash(height - 1, parent_branch)),
            next: (best && height < self.tip)
                .then(|| self.block_hash(height + 1, self.branch(height + 1))),
        }
    }

    /// The block's coinbase, and what the rest of its plan starts from.
    fn start(&self, height: u32, branch: u16) -> Start {
        let mut rng = Rng(self.seed(KIND_PLAN, height, branch, 0));
        let parent_branch = self.parent_branch(height, branch);
        let after = self.balances(height, branch);
        let before = if height == 0 {
            [0; 6]
        } else {
            self.balances(height - 1, parent_branch)
        };
        let trees_after = self.trees(height, branch);
        let trees_before = if height == 0 {
            [0; 3]
        } else {
            self.trees(height - 1, parent_branch)
        };
        let mut notes: [u32; 3] = std::array::from_fn(|index| {
            u32::try_from(trees_after[index] - trees_before[index]).unwrap_or(u32::MAX)
        });
        let deltas: [i64; 6] =
            std::array::from_fn(|index| after[index] as i64 - before[index] as i64);
        let split = self.split(height);
        let fees = self.fees(height, branch) as i64;
        let burned = if self.heights.is_active(Upgrade::Nu7, height) {
            burned_share(fees as u64) as i64
        } else {
            0
        };
        let disbursement = if height == self.at(Upgrade::Nu6_1) {
            self.disbursed(height) as i64
        } else {
            0
        };
        let active = |upgrade| self.heights.is_active(upgrade, height);

        // The coinbase: the streams, the disbursement, and the miner's claim, now and then
        // into a shielded pool.
        let claim = split.miner as i64 + fees - burned;
        let mut coinbase = Part::default();
        if split.founders > 0 {
            coinbase.pay(split.founders as i64, self.fund_address(0), "scripthash");
        }
        for (_, address, zat) in &split.streams {
            coinbase.pay(*zat as i64, address.clone(), "scripthash");
        }
        if disbursement > 0 {
            coinbase.pay(disbursement, self.fund_address(5), "scripthash");
        }
        let shielded_coinbase = if active(Upgrade::Nu6_3) {
            [(Pool::Ironwood, 2u32), (Pool::Sapling, 1)]
        } else if active(Upgrade::Nu5) {
            [(Pool::Orchard, 2), (Pool::Sapling, 1)]
        } else {
            [(Pool::Sapling, 1), (Pool::Sapling, 1)]
        };
        let pick = shielded_coinbase[rng.below(2) as usize];
        let shield = active(Upgrade::Heartwood)
            && claim > 0
            && rng.chance(30)
            && notes[tree_index(pick.0)] >= pick.1 + 4;
        if shield {
            coinbase.pay_into(pick.0, claim, pick.1);
            notes[tree_index(pick.0)] -= pick.1;
        } else {
            coinbase.pay(claim, self.address(rng.below(12)), "pubkeyhash");
        }
        Start {
            rng,
            notes,
            deltas,
            fees,
            coinbase,
        }
    }

    /// The transactions of the block at `height` on `branch`, coinbase first and each in its
    /// place, built to move exactly the value between the block's parent's pools and its own.
    /// Those without transparent inputs are final, their fees paid; the shielding and the
    /// transparent payments are funded later ([`SynthChain::funded`]), from the fees left. So
    /// funding a later block needs nothing of this one but its skeleton, and never looks
    /// further back. A big block's extra payments are not among them ([`SynthChain::filler`]).
    fn skeleton(&self, height: u32, branch: u16) -> Skeleton {
        let Start {
            mut rng,
            mut notes,
            deltas,
            fees,
            coinbase,
        } = self.start(height, branch);
        let active = |upgrade| self.heights.is_active(upgrade, height);
        let mut paid = [0i64; 6];
        paid[Pool::Transparent.index()] = coinbase.outputs.iter().map(|out| out.0).sum();
        for (pool, net_out) in Pool::SHIELDED.into_iter().zip(coinbase.net_out()) {
            paid[pool.index()] = -net_out;
        }

        // What the other transactions must move into each pool; fees leave the pools they are
        // paid from. A big block's extra payments pay their own.
        let mut need: [i64; 6] = std::array::from_fn(|index| deltas[index] - paid[index]);
        need[Pool::Lockbox.index()] = 0;
        let mut fee_left = fees - self.filler_fees(height);
        let mut parts: Vec<Part> = Vec::new();
        let mut migration = None;

        // The block's transactions pay per logical action what a wallet pays in this era when
        // the block's fees cover that for all of them, with room to spare; else zebrad's
        // conventional fee, which none pays less than. All of them: a note each, the flows'
        // transparent sides and a couple of payments.
        let least = zip317::MARGINAL_FEE as i64;
        let flows = Pool::SHIELDED
            .into_iter()
            .filter(|pool| need[pool.index()] != 0)
            .count();
        let likely = i64::from(notes.iter().sum::<u32>())
            + (flows * (SHIELD_INPUTS_GUESS + 2)) as i64
            + 2 * 2;
        let wallets = self.conventional_fee(height, 2) / 2;
        let rate = if wallets * likely * 5 / 4 <= fee_left {
            wallets
        } else {
            least
        };
        let price = |actions: u32| rate * i64::from(actions.max(2));

        // Orchard to Ironwood, from NU6.3.
        let (orchard, ironwood) = (Pool::Orchard.index(), Pool::Ironwood.index());
        if active(Upgrade::Nu6_3)
            && need[orchard] < 0
            && need[ironwood] > 0
            && notes[1] >= 4
            && notes[2] >= 4
        {
            let fee = price(4).min(fee_left).min(-need[orchard]);
            let moved = need[ironwood].min(-need[orchard] - fee);
            if moved > 0 && fee >= 4 * least {
                let mut part = Part::default();
                part.shielded(Pool::Orchard, 2, moved + fee);
                part.shielded(Pool::Ironwood, 2, -moved);
                need[orchard] += moved + fee;
                need[ironwood] -= moved;
                fee_left -= fee;
                notes[1] -= 2;
                notes[2] -= 2;
                migration = Some(parts.len());
                parts.push(part);
            }
        }

        // Fully shielded transactions, two or three notes each, while notes and fees last.
        // Each needs room in its pool: a pool that only drains cannot take back its fee.
        let flow_reserve = 4 * price(2);
        for pool in [Pool::Sapling, Pool::Orchard, Pool::Ironwood] {
            let tree = tree_index(pool);
            while notes[tree] >= 2 + 2 && rng.chance(85) {
                let notes_here = if notes[tree] >= 7 {
                    2 + rng.below(2) as u32
                } else {
                    2
                };
                let mut fee = price(notes_here).min(fee_left - flow_reserve);
                let drain_only = pool == Pool::Orchard && active(Upgrade::Nu6_3);
                if drain_only {
                    fee = fee.min(-need[pool.index()]);
                }
                if fee < least * i64::from(notes_here) {
                    break;
                }
                let mut part = Part::default();
                part.shielded(pool, notes_here, fee);
                need[pool.index()] += fee;
                fee_left -= fee;
                notes[tree] -= notes_here;
                parts.push(part);
            }
        }

        // One transaction per shielded pool for the rest of its flow, to or from transparent
        // outputs, with up to two of the pool's notes.
        let mut flow_notes = [0u32; 6];
        for pool in Pool::SHIELDED {
            let amount = need[pool.index()];
            if amount == 0 {
                continue;
            }
            let here = match pool {
                Pool::Sapling => notes[0].min(2),
                Pool::Orchard => notes[1].min(2),
                Pool::Ironwood => notes[2].min(2),
                _ => 0,
            };
            let here = if amount > 0 { here.max(1) } else { here };
            take_notes(&mut notes, pool, here);
            flow_notes[pool.index()] = here;
        }
        // The notes left over join their pool's flow, so the fee for them comes from
        // transparent value. With no flow, Orchard's and Ironwood's join the migration, whose
        // fee comes from the side that still has a flow; else a transaction of their own pays it
        // from transparent inputs.
        for pool in [Pool::Sapling, Pool::Orchard, Pool::Ironwood] {
            let tree = tree_index(pool);
            let left = std::mem::take(&mut notes[tree]);
            if left == 0 {
                continue;
            }
            if need[pool.index()] != 0 {
                flow_notes[pool.index()] += left;
                continue;
            }
            if let Some(index) = migration.filter(|_| pool != Pool::Sapling) {
                let part = &mut parts[index];
                let fee = if rate * i64::from(left) <= fee_left - flow_reserve {
                    rate * i64::from(left)
                } else {
                    least * i64::from(left)
                };
                let moved = -part.ironwood.map_or(0, |bundle| bundle.1);
                let side = if need[orchard] + fee < 0 {
                    Some(Pool::Orchard)
                } else if need[ironwood] > 0 && moved > fee {
                    Some(Pool::Ironwood)
                } else {
                    None
                };
                if let Some(side) = side.filter(|_| fee <= fee_left) {
                    // More leaves Orchard, or less reaches Ironwood: the pool's flow makes up
                    // the difference.
                    let bundle = match side {
                        Pool::Orchard => &mut part.orchard,
                        _ => &mut part.ironwood,
                    };
                    *bundle = bundle.map(|(actions, net_out)| (actions, net_out + fee));
                    need[side.index()] += fee;
                    fee_left -= fee;
                    part.more_notes(pool, left);
                    continue;
                }
            }
            let mut part = Part {
                funding: Funding::Shield { amount: 0 },
                ..Part::default()
            };
            part.shielded(pool, left, 0);
            parts.push(part);
        }
        let mut flows: Vec<PoolFlow> = Vec::new();
        for pool in Pool::SHIELDED {
            let amount = need[pool.index()];
            if amount == 0 {
                continue;
            }
            let mut part = Part::default();
            let here = flow_notes[pool.index()];
            if amount > 0 {
                // Shielding, from transparent outputs found when it is funded, which pay its
                // fee too; what they bring beyond comes back as change.
                part.funding = Funding::Shield { amount };
                part.pay_into(pool, amount, here);
            } else {
                // Deshielding, paid for below once its outputs are known.
                part.take_out(pool, -amount, here);
            }
            need[pool.index()] = 0;
            flows.push(PoolFlow {
                index: parts.len(),
                out: (-amount).max(0),
                two_outputs: rng.chance(40),
            });
            parts.push(part);
        }

        // What the shielding is likely to take: a few inputs each, at the block's rate.
        let shielding: i64 = parts
            .iter()
            .filter(|part| matches!(part.funding, Funding::Shield { .. }))
            .map(|part| rate * i64::from(part.paid_actions(SHIELD_INPUTS_GUESS, 1)))
            .sum();
        // Deshielding pays for its actions from what it takes out: at the block's rate while that
        // leaves the shielding and a payment theirs, else at zebrad's.
        for flow in flows.iter().filter(|flow| flow.out > 0) {
            let part = &mut parts[flow.index];
            let actions = i64::from(part.paid_actions(0, if flow.two_outputs { 2 } else { 1 }));
            let room = fee_left - shielding - 2 * rate;
            let fee = if rate * actions <= room {
                rate * actions
            } else {
                least * actions
            }
            .min(fee_left.max(0))
            .min(flow.out);
            let sent = flow.out - fee;
            let first = if flow.two_outputs && sent > 2 {
                sent / 3
            } else {
                sent
            };
            part.pay(first, self.address(rng.zipf()), "pubkeyhash");
            part.pay(sent - first, self.address(rng.zipf()), "pubkeyhash");
            fee_left -= fee;
        }

        // Transparent payments pay the fees the shielding leaves; how many, from what it is
        // likely to take. They are funded with the shielding.
        if fee_left > 0 {
            let spare = (fee_left - shielding).max(0);
            let count = (1 + spare / (3 * price(2))).min(8);
            for _ in 0..count {
                parts.push(Part {
                    funding: Funding::Pay {
                        inputs: 1 + rng.below(2) as u32,
                    },
                    ..Part::default()
                });
            }
        }

        // Coinbase first, the rest in a seeded order.
        for index in (1..parts.len()).rev() {
            let other = rng.below(index as u64 + 1) as usize;
            parts.swap(index, other);
        }
        let mut all = Vec::with_capacity(parts.len() + 1);
        all.push(coinbase);
        all.extend(parts);
        Skeleton {
            parts: all,
            fee_left,
            rate,
        }
    }

    /// The block's skeleton with its shielding and payments funded. The shielding pays for its
    /// logical actions, inputs included, at the block's rate when what is left still pays each
    /// payment zebrad's conventional fee, else at zebrad's; the payments share the rest. The
    /// first payment of every hundredth block pays half of zebrad's conventional fee, so a
    /// transaction that paid less shows; zebrad's mempool takes none such, but a block may
    /// hold one.
    fn funded(&self, skeleton: Skeleton, height: u32, branch: u16) -> Vec<Part> {
        let Skeleton {
            parts,
            fee_left,
            rate,
        } = skeleton;
        let least = zip317::MARGINAL_FEE as i64;
        let pays: Vec<usize> = (0..parts.len())
            .filter(|index| matches!(parts[*index].funding, Funding::Pay { .. }))
            .collect();
        let room = fee_left - 2 * least * pays.len() as i64;
        let mut funded = parts.clone();
        let mut shielding = self.fund_shielding(&mut funded, height, branch, rate);
        if shielding > room {
            funded = parts;
            shielding = self.fund_shielding(&mut funded, height, branch, least);
        }
        let mut parts = funded;
        let mut left = fee_left - shielding;
        if left < 0 {
            // Even zebrad's fee is more than the block's: the shielding pays less, its change
            // taking the difference.
            for part in parts.iter_mut().rev() {
                if let Funding::Shield { .. } = part.funding {
                    let back = (-left).min(part.fee());
                    part.pay(back, self.address(u64::from(height)), "pubkeyhash");
                    left += back;
                }
            }
        }
        let mut fees = vec![0; pays.len()];
        if !pays.is_empty() {
            let count = pays.len() as i64;
            for (fee, share) in fees.iter_mut().zip(0..) {
                *fee = if share + 1 == count {
                    left - left / count * (count - 1)
                } else {
                    left / count
                };
            }
            if height.is_multiple_of(100) && fees.len() >= 2 {
                let short = least.min(fees[0]);
                fees[1] += fees[0] - short;
                fees[0] = short;
            }
        }
        for (index, fee) in pays.into_iter().zip(fees) {
            self.fund_payment(&mut parts[index], height, branch, index as u16, fee.max(0));
        }
        parts
    }

    /// Funds every shielding part of `parts` at `rate` per logical action; returns their fees.
    fn fund_shielding(&self, parts: &mut [Part], height: u32, branch: u16, rate: i64) -> i64 {
        let mut fees = 0;
        for (index, part) in parts.iter_mut().enumerate() {
            if let Funding::Shield { amount } = part.funding {
                self.fund_shield(part, amount, height, branch, index as u16, rate);
                fees += part.fee();
            }
        }
        fees
    }

    /// The transactions of the block at `height` on `branch`, coinbase first, funded and
    /// built; a big block's extra payments follow them ([`SynthChain::filler`]).
    fn plan(&self, height: u32, branch: u16) -> Vec<Built> {
        self.funded(self.skeleton(height, branch), height, branch)
            .into_iter()
            .enumerate()
            .map(|(index, part)| {
                let index = index as u16;
                self.build(part, height, branch, index, index == 0)
            })
            .collect()
    }

    /// The `index`-th transaction of the block at `height` on `branch`, built on its own: one
    /// without transparent inputs from the skeleton, the others funded with the block's rest,
    /// whose fees they share.
    fn built(&self, height: u32, branch: u16, index: u16) -> Option<Built> {
        let skeleton = self.skeleton(height, branch);
        let base = skeleton.parts.len() as u16;
        if index >= base {
            return (index < base + self.fillers(height) as u16)
                .then(|| self.filler(height, branch, index));
        }
        let mut parts = match skeleton.parts[usize::from(index)].funding {
            Funding::None => skeleton.parts,
            _ => self.funded(skeleton, height, branch),
        };
        let part = parts.swap_remove(usize::from(index));
        Some(self.build(part, height, branch, index, index == 0))
    }

    /// The `index`-th transaction of a big block, past its plan: a payment from two coinbase
    /// outputs to two addresses, paying [`FILLER_FEE`].
    fn filler(&self, height: u32, branch: u16, index: u16) -> Built {
        let mut rng = Rng(self.seed(KIND_FUNDS, height, branch, u64::from(index)));
        let (inputs, sum) = self.coinbase_inputs(height, branch, 2, FILLER_FEE, &mut rng);
        let sent = sum - FILLER_FEE;
        let change = sent / 3;
        let mut part = Part {
            t_in: sum,
            inputs,
            ..Part::default()
        };
        part.pay(sent - change, self.address(rng.zipf()), "pubkeyhash");
        part.pay(change, self.address(rng.zipf()), "pubkeyhash");
        self.build(part, height, branch, index, false)
    }

    /// The roots among the block's transactions: the outputs of its coinbase and of its
    /// transactions without transparent inputs, which need nothing older to have a value.
    fn roots(&self, height: u32, branch: u16) -> Vec<Root> {
        self.skeleton(height, branch)
            .parts
            .iter()
            .enumerate()
            .filter(|(_, part)| matches!(part.funding, Funding::None))
            .flat_map(|(index, part)| self.roots_of(part, height, branch, index as u16))
            .collect()
    }

    /// The outputs of the block's coinbase, without the rest of its plan.
    fn coinbase_roots(&self, height: u32, branch: u16) -> Vec<Root> {
        self.roots_of(&self.start(height, branch).coinbase, height, branch, 0)
    }

    fn roots_of(&self, part: &Part, height: u32, branch: u16, index: u16) -> Vec<Root> {
        part.outputs
            .iter()
            .enumerate()
            .map(|(n, (zat, address, kind))| Root {
                txid: self.txid(height, branch, index),
                n: n as u32,
                zat: *zat,
                address: address.clone(),
                kind,
                height,
                coinbase: index == 0,
            })
            .collect()
    }

    /// The heights a transaction at `height` takes outputs from: from 100 blocks back, where
    /// coinbase outputs mature, a few blocks apart, `count` of them; near the chain's start,
    /// younger ones; and last the block itself, whose coinbase comes first in it.
    fn funding_heights(&self, height: u32, count: usize, rng: &mut Rng) -> Vec<u32> {
        let mut heights = Vec::with_capacity(count + 1);
        if height > 100 {
            let mut at = height - 100 - rng.below(u64::from((height - 100).min(20))) as u32;
            while heights.len() < count && at >= 1 {
                heights.push(at);
                at = at.saturating_sub(1 + rng.below(13) as u32);
            }
        } else {
            heights.extend((1..height).rev().take(count));
        }
        heights.push(height);
        heights
    }

    /// Funds a shielding of `amount` by `part`, the `index`-th transaction of the block at
    /// `height` on `branch`: its inputs spend roots until they bring the amount and the fee, at
    /// `rate` per logical action, its own inputs counted; the rest comes back as change.
    fn fund_shield(
        &self,
        part: &mut Part,
        amount: i64,
        height: u32,
        branch: u16,
        index: u16,
        rate: i64,
    ) {
        let mut rng = Rng(self.seed(KIND_FUNDS, height, branch, u64::from(index)));
        let fee = |inputs: usize| rate * i64::from(part.paid_actions(inputs, 1));
        let mut inputs = Vec::new();
        let mut sum = 0;
        'blocks: for at in self.funding_heights(height, 64, &mut rng) {
            let roots = if at == height {
                self.coinbase_roots(height, branch)
            } else {
                self.roots(at, self.branch(at))
            };
            for root in roots {
                sum += root.zat;
                inputs.push(root);
                if sum >= amount + fee(inputs.len()) {
                    break 'blocks;
                }
            }
        }
        let change = sum - amount - fee(inputs.len());
        part.t_in = sum;
        part.inputs = inputs;
        part.pay(change, self.address(rng.zipf()), "pubkeyhash");
    }

    /// Funds a transparent payment, the `index`-th transaction of the block at `height` on
    /// `branch`, paying `fee`: it spends coinbase outputs and sends what they bring beyond.
    fn fund_payment(&self, part: &mut Part, height: u32, branch: u16, index: u16, fee: i64) {
        let Funding::Pay { inputs } = part.funding else {
            return;
        };
        let mut rng = Rng(self.seed(KIND_FUNDS, height, branch, u64::from(index)));
        let (inputs, sum) = self.coinbase_inputs(height, branch, inputs, fee, &mut rng);
        part.t_in = sum;
        part.inputs = inputs;
        let sent = sum - fee;
        if rng.chance(60) && sent > 2 {
            let change = sent / (2 + rng.below(5) as i64);
            part.pay(sent - change, self.address(rng.zipf()), "pubkeyhash");
            part.pay(change, self.address(rng.zipf()), "pubkeyhash");
        } else {
            part.pay(sent, self.address(rng.zipf()), "pubkeyhash");
        }
    }

    /// `wanted` coinbase outputs of blocks 100 to 2,000 back, or more until they bring more
    /// than `fee`; near the chain's start, the block's own coinbase.
    fn coinbase_inputs(
        &self,
        height: u32,
        branch: u16,
        wanted: u32,
        fee: i64,
        rng: &mut Rng,
    ) -> (Vec<Root>, i64) {
        let mut inputs: Vec<Root> = Vec::new();
        let mut sum = 0;
        for _ in 0..32 {
            if inputs.len() as u32 >= wanted && sum > fee {
                break;
            }
            let at = if height > 101 {
                height - 100 - rng.below(u64::from((height - 101).min(1_900))) as u32
            } else {
                height
            };
            let roots = if at == height {
                self.coinbase_roots(height, branch)
            } else {
                self.coinbase_roots(at, self.branch(at))
            };
            if roots.is_empty() {
                continue;
            }
            let root = roots[rng.below(roots.len() as u64) as usize].clone();
            sum += root.zat;
            inputs.push(root);
        }
        (inputs, sum)
    }

    fn build(&self, part: Part, height: u32, branch: u16, index: u16, coinbase: bool) -> Built {
        let mut rng = Rng(self.seed(KIND_TX, height, branch, u64::from(index) + 1_000));
        let vin = if coinbase {
            vec![TxIn {
                coinbase: Some(format!(
                    "03{:06x}{:08x}",
                    height.swap_bytes() >> 8,
                    rng.next() as u32
                )),
                txid: None,
                vout: None,
                script_sig: None,
                prevout: None,
            }]
        } else {
            part.inputs
                .iter()
                .map(|root| TxIn {
                    coinbase: None,
                    txid: Some(root.txid),
                    vout: Some(root.n),
                    script_sig: Some(ScriptSig {
                        hex: HexLen(P2PKH_SCRIPT_SIG),
                    }),
                    prevout: None,
                })
                .collect()
        };
        let vout: Vec<TxOut> = part
            .outputs
            .iter()
            .enumerate()
            .map(|(n, (zat, address, kind))| TxOut {
                value_zat: *zat,
                n: n as u32,
                script: script(kind, address),
            })
            .collect();
        let bundle = |bundle: Option<(u32, i64)>| {
            bundle.map(|(actions, value_balance)| Actions {
                actions: Count(actions),
                value_balance,
            })
        };
        let ironwood = part.ironwood.is_some();
        let version = if ironwood {
            6
        } else if self.heights.is_active(Upgrade::Nu5, height) {
            5
        } else if self.heights.is_active(Upgrade::Sapling, height) {
            4
        } else if self.heights.is_active(Upgrade::Overwinter, height) {
            3
        } else {
            2
        };
        let actions = part.orchard.map_or(0, |b| b.0) + part.ironwood.map_or(0, |b| b.0);
        let size = 10
            + vin
                .iter()
                .map(|input| i64::from(input.serialized_size()))
                .sum::<i64>()
            + vout
                .iter()
                .map(|output| i64::from(output.serialized_size()))
                .sum::<i64>()
            + 384 * i64::from(part.sapling.0)
            + 948 * i64::from(part.sapling.1)
            + if part.sapling.0 + part.sapling.1 > 0 {
                80
            } else {
                0
            }
            + 1_802 * part.joinsplits.len() as i64
            + if actions > 0 {
                2_800 + 3_150 * i64::from(actions)
            } else {
                0
            };
        let fee = part.fee();
        let tx = Tx {
            txid: self.txid(height, branch, index),
            height: None,
            confirmations: None,
            fee: None,
            vin,
            vout,
            sapling_spends: Count(part.sapling.0),
            sapling_outputs: Count(part.sapling.1),
            joinsplits: part
                .joinsplits
                .iter()
                .map(|(vpub_old, vpub_new)| JoinSplit {
                    vpub_old: *vpub_old,
                    vpub_new: *vpub_new,
                })
                .collect(),
            orchard: bundle(part.orchard),
            ironwood: bundle(part.ironwood),
            sapling_value_balance: (part.sapling.0 + part.sapling.1 > 0).then_some(part.sapling.2),
            size: Some(size),
            time: None,
            version,
            lock_time: 0,
            expiry_height: (version >= 3).then(|| if coinbase { height } else { height + 40 }),
            block_hash: None,
            block_time: None,
        };
        Built {
            tx,
            size,
            fee,
            spent: part.inputs,
        }
    }
}

/// The locking script of an output of type `kind` paying `address`.
fn script(kind: &str, address: &str) -> ScriptPubKey {
    let bytes = match kind {
        "scripthash" => 23,
        _ => 25,
    };
    ScriptPubKey {
        kind: kind.to_owned(),
        addresses: Some(vec![address.to_owned()]),
        hex: HexLen(bytes),
    }
}

/// A block's transactions in their places, before the shielding and the payments are funded
/// ([`SynthChain::skeleton`]).
struct Skeleton {
    parts: Vec<Part>,
    /// The block's fees the shielding and the payments share.
    fee_left: i64,
    /// The fee its transactions pay per logical action.
    rate: i64,
}

/// A transaction moving the rest of a shielded pool's change to or from transparent value.
struct PoolFlow {
    /// Its place among the block's transactions while they are planned.
    index: usize,
    /// The value it takes out of the pool; 0 when it shields.
    out: i64,
    /// Whether what it takes out goes to two outputs rather than one.
    two_outputs: bool,
}

/// What a block's plan starts from: its coinbase, and the seeded draws, notes, pool changes
/// and fees the rest of the plan uses.
struct Start {
    rng: Rng,
    notes: [u32; 3],
    deltas: [i64; 6],
    fees: i64,
    coinbase: Part,
}

/// How a block's subsidy splits.
struct Split {
    total: u64,
    miner: u64,
    founders: u64,
    /// `(recipient, address, zatoshis)`.
    streams: Vec<(&'static str, String, u64)>,
    lockbox: u64,
}

/// A transaction of the synthetic chain, with its fee (which the explorer cannot know without
/// the spent outputs' values) and the outputs its inputs spend.
#[derive(Clone, Debug)]
struct Built {
    tx: Tx,
    size: i64,
    fee: i64,
    spent: Vec<Root>,
}

/// An output a later transaction spends: one of a coinbase or of a transaction without
/// transparent inputs, so its value needs nothing older.
#[derive(Clone, Debug)]
struct Root {
    txid: TxId,
    n: u32,
    zat: i64,
    address: String,
    kind: &'static str,
    height: u32,
    coinbase: bool,
}

impl Root {
    /// As `getblock` at verbosity 3 describes it.
    fn prevout(&self) -> Prevout {
        Prevout {
            generated: self.coinbase,
            height: self.height,
            value_zat: self.zat,
            script: script(self.kind, &self.address),
        }
    }
}

/// How a planned transaction is funded from transparent outputs.
#[derive(Clone, Copy, Debug, Default)]
enum Funding {
    /// It has no transparent inputs.
    #[default]
    None,
    /// It shields `amount` into its pools, and pays for its logical actions.
    Shield { amount: i64 },
    /// A transparent payment from at least `inputs` coinbase outputs, paying a share of what
    /// the block's other transactions leave of its fees.
    Pay { inputs: u32 },
}

/// A transaction being planned: what it takes in and pays out, per part.
#[derive(Clone, Debug, Default)]
struct Part {
    funding: Funding,
    /// The outputs its inputs spend, once funded.
    inputs: Vec<Root>,
    /// The value of its transparent inputs.
    t_in: i64,
    /// `(zatoshis, address, script type)`.
    outputs: Vec<(i64, String, &'static str)>,
    /// `(vpub_old, vpub_new)`.
    joinsplits: Vec<(i64, i64)>,
    /// Sapling `(spends, outputs, value balance)`.
    sapling: (u32, u32, i64),
    /// `(actions, value balance)`.
    orchard: Option<(u32, i64)>,
    ironwood: Option<(u32, i64)>,
}

impl Part {
    fn pay(&mut self, zat: i64, address: String, kind: &'static str) {
        if zat > 0 {
            self.outputs.push((zat, address, kind));
        }
    }

    /// A shielded part with `notes` new notes and `net_out` leaving the pool.
    fn shielded(&mut self, pool: Pool, notes: u32, net_out: i64) {
        match pool {
            Pool::Sapling => self.sapling = (notes.min(2), notes, net_out),
            Pool::Orchard => self.orchard = Some((notes, net_out)),
            Pool::Ironwood => self.ironwood = Some((notes, net_out)),
            Pool::Sprout => self.joinsplits.push((0.max(-net_out), 0.max(net_out))),
            Pool::Transparent | Pool::Lockbox => {}
        }
    }

    fn pay_into(&mut self, pool: Pool, amount: i64, notes: u32) {
        match pool {
            Pool::Sapling => self.sapling = (0, notes, -amount),
            Pool::Sprout => self.joinsplits.push((amount, 0)),
            _ => self.shielded(pool, notes.max(2), -amount),
        }
    }

    fn take_out(&mut self, pool: Pool, amount: i64, notes: u32) {
        match pool {
            Pool::Sapling => self.sapling = (1 + u32::from(amount % 2 == 0), notes, amount),
            Pool::Sprout => self.joinsplits.push((0, amount)),
            _ => self.shielded(pool, notes.max(2), amount),
        }
    }

    fn more_notes(&mut self, pool: Pool, notes: u32) {
        match pool {
            Pool::Sapling => self.sapling.1 += notes,
            Pool::Orchard => self.orchard = self.orchard.map(|(n, value)| (n + notes, value)),
            Pool::Ironwood => self.ironwood = self.ironwood.map(|(n, value)| (n + notes, value)),
            _ => {}
        }
    }

    /// The value that leaves each shielded pool, in [`Pool::SHIELDED`]'s order.
    fn net_out(&self) -> [i64; 4] {
        [
            self.joinsplits.iter().map(|(old, new)| new - old).sum(),
            self.sapling.2,
            self.orchard.map_or(0, |bundle| bundle.1),
            self.ironwood.map_or(0, |bundle| bundle.1),
        ]
    }

    fn fee(&self) -> i64 {
        self.t_in - self.outputs.iter().map(|out| out.0).sum::<i64>()
            + self.net_out().iter().sum::<i64>()
    }

    /// The logical actions ZIP 317 makes the transaction pay for, at least two, with `inputs`
    /// standard inputs and `more` standard outputs beside its own.
    fn paid_actions(&self, inputs: usize, more: usize) -> u32 {
        let input = TxIn {
            coinbase: None,
            txid: None,
            vout: None,
            script_sig: Some(ScriptSig {
                hex: HexLen(P2PKH_SCRIPT_SIG),
            }),
            prevout: None,
        }
        .serialized_size();
        let output = |kind: &str| {
            TxOut {
                value_zat: 0,
                n: 0,
                script: script(kind, ""),
            }
            .serialized_size()
        };
        let parts = LogicalActions {
            in_bytes: input * inputs as u32,
            out_bytes: self
                .outputs
                .iter()
                .map(|(_, _, kind)| output(kind))
                .sum::<u32>()
                + output("pubkeyhash") * more as u32,
            joinsplits: self.joinsplits.len() as u32,
            sapling_spends: self.sapling.0,
            sapling_outputs: self.sapling.1,
            orchard_actions: self.orchard.map_or(0, |bundle| bundle.0),
            ironwood_actions: self.ironwood.map_or(0, |bundle| bundle.0),
        };
        zip317::paid_actions(zip317::logical_actions(&parts))
    }
}

fn tree_index(pool: Pool) -> usize {
    match pool {
        Pool::Orchard => 1,
        Pool::Ironwood => 2,
        _ => 0,
    }
}

fn take_notes(notes: &mut [u32; 3], pool: Pool, count: u32) {
    if matches!(pool, Pool::Sapling | Pool::Orchard | Pool::Ironwood) {
        let tree = tree_index(pool);
        notes[tree] = notes[tree].saturating_sub(count);
    }
}

/// The branch `height` is on, after `forks`: the latest reorganisation whose first height is
/// at or below it.
fn branch_among(forks: &[u32], height: u32) -> u16 {
    forks
        .iter()
        .enumerate()
        .rev()
        .find(|(_, start)| **start <= height)
        .map_or(0, |(index, _)| index as u16 + 1)
}

/// `v0` at `h0` to `v1` at `h1`, linearly, at `height`.
fn lerp(height: u32, h0: u32, v0: i128, h1: u32, v1: i128) -> i128 {
    if h1 <= h0 {
        return v1;
    }
    let along = i128::from(height.clamp(h0, h1) - h0);
    v0 + (v1 - v0) * along / i128::from(h1 - h0)
}

/// zebrad 7.0.0's consensus branch ids (zebra-chain/src/parameters/network_upgrade.rs).
fn branch_id(upgrade: Upgrade) -> &'static str {
    match upgrade {
        Upgrade::Overwinter => "5ba81b19",
        Upgrade::Sapling => "76b809bb",
        Upgrade::Blossom => "2bb40e60",
        Upgrade::Heartwood => "f5b9230b",
        Upgrade::Canopy => "e9ff75a6",
        Upgrade::Nu5 => "c2d6d0b4",
        Upgrade::Nu6 => "c8e71055",
        Upgrade::Nu6_1 => "4dec4df0",
        Upgrade::Nu6_2 => "5437f330",
        Upgrade::Nu6_3 => "37a5165b",
        Upgrade::Nu7 => "77190ad9",
    }
}

/// SplitMix64's finaliser.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn fill(bytes: &mut [u8], seed: u64) {
    let mut rng = Rng(seed);
    for chunk in bytes.chunks_mut(8) {
        let word = rng.next().to_be_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
}

fn hex(seed: u64) -> String {
    let mut bytes = [0u8; 32];
    fill(&mut bytes, seed);
    Hash32(bytes).to_hex()
}

/// A string shaped like a base58 address after `prefix`, 35 characters in all, whose checksum
/// will not verify.
fn made_up_address(prefix: &str, seed: u64) -> String {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut rng = Rng(seed);
    let mut out = String::from(prefix);
    while out.len() < 35 {
        out.push(ALPHABET[rng.below(ALPHABET.len() as u64) as usize] as char);
    }
    out
}

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.0)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    /// An address index from 0 to 1,999, small ones far more often.
    fn zipf(&mut self) -> u64 {
        let spread = 1 + self.below(2_000);
        self.below(spread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ledger::{block_ledger, ledger_check},
        zec_to_zat,
    };
    use launcher_types::explorer::LedgerCheck;

    const NOW: i64 = 1_800_000_000;

    /// Heights in every era, the activation blocks and their neighbours included.
    fn heights_to_check(chain: &SynthChain) -> Vec<u32> {
        let mut list = vec![1, 2, 3, 1_000, 99_999];
        for upgrade in Upgrade::ALL {
            if let Some(start) = chain.heights().height(upgrade) {
                list.extend([start - 1, start, start + 1, start + 7]);
            }
        }
        let mut seed = 7u64;
        for _ in 0..300 {
            seed = mix(seed);
            list.push((seed % u64::from(chain.tip())) as u32 + 1);
        }
        list.retain(|height| *height <= chain.tip());
        list
    }

    fn chains() -> Vec<SynthChain> {
        vec![
            SynthChain::new(Network::Mainnet, 3_600_000, NOW),
            SynthChain::new(Network::Testnet, 4_500_000, NOW),
        ]
    }

    #[test]
    fn every_block_balances() {
        for chain in chains() {
            for height in heights_to_check(&chain) {
                let block = chain.block(BlockId::Height(height)).expect("block");
                let parent = chain.block(BlockId::Height(height - 1)).expect("parent");
                let coinbase = chain.tx(&block.tx[0]).expect("coinbase");
                let ledger = block_ledger(&block, Some(&parent), Some(&coinbase), chain.heights());
                let subsidy = zec_to_zat(chain.subsidy(height).total) as u64;
                let check = ledger_check(&ledger, height, Some(subsidy), chain.heights());
                assert_eq!(
                    check,
                    LedgerCheck::Balanced,
                    "{:?} {height}",
                    chain.network()
                );
                // The fees the ledger finds are what the transactions paid.
                let paid: i64 = chain
                    .plan(height, chain.branch(height))
                    .iter()
                    .skip(1)
                    .map(|built| built.fee)
                    .sum();
                assert_eq!(ledger.fees, Some(paid as u64), "{height}");
                if chain.heights().is_active(Upgrade::Nu7, height) {
                    assert_eq!(ledger.burned, Some(burned_share(paid as u64)), "{height}");
                }
                // Each transaction pays a fee, never a negative one.
                for built in chain.plan(height, chain.branch(height)).iter().skip(1) {
                    assert!(built.fee >= 0, "{height}: {:?}", built.tx);
                }
                // The trees grew by the notes the transactions created.
                let notes = ledger.notes.expect("notes");
                let plan = chain.plan(height, chain.branch(height));
                let created = [
                    plan.iter().map(|b| b.tx.sapling_outputs.0).sum::<u32>(),
                    plan.iter()
                        .map(|b| b.tx.orchard.map_or(0, |o| o.actions.0))
                        .sum(),
                    plan.iter()
                        .map(|b| b.tx.ironwood.map_or(0, |o| o.actions.0))
                        .sum(),
                ];
                assert_eq!(notes, created, "{height}");
            }
        }
    }

    #[test]
    fn transactions_pay_for_their_logical_actions_but_one_in_a_hundred_blocks() {
        let chains = [
            // Wallets pay 5,000 zatoshis an action here; 1,000 on the others.
            SynthChain::new(Network::Mainnet, 3_551_000, NOW),
            SynthChain::new(Network::Mainnet, 3_600_000, NOW),
            SynthChain::new(Network::Testnet, 4_500_000, NOW),
        ];
        for chain in chains {
            let (mut all, mut at_rate, mut short) = (0, 0, 0);
            for height in chain.tip() - 300..=chain.tip() {
                let rate = chain.conventional_fee(height, 2) / 2;
                let mut unpaid = 0;
                for built in chain.plan(height, chain.branch(height)).iter().skip(1) {
                    let actions = zip317::logical_actions(&zip317::parts(&built.tx));
                    let fee = u64::try_from(built.fee).expect("no negative fee");
                    all += 1;
                    at_rate += usize::from(fee >= rate as u64 * u64::from(actions.max(2)));
                    unpaid += usize::from(zip317::unpaid_actions(actions, fee) > 0);
                }
                // None pays less than zebrad's conventional fee, but for one transparent
                // payment in every hundredth block.
                let allowed = usize::from(height.is_multiple_of(100));
                assert!(
                    unpaid <= allowed,
                    "{:?} {height}: {unpaid}",
                    chain.network()
                );
                short += unpaid;
            }
            assert!(short > 0, "the fee card's \"paid less\" is shown somewhere");
            // Most pay what wallets pay: the blocks' fees cover it.
            assert!(
                4 * at_rate > 3 * all,
                "{:?}: {at_rate} of {all}",
                chain.network()
            );
        }
    }

    #[test]
    fn every_input_spends_an_output_the_chain_has_and_the_fees_follow() {
        for chain in chains() {
            for height in heights_to_check(&chain) {
                let block = chain.block(BlockId::Height(height)).expect("block");
                let v3 = chain
                    .block_v3(BlockId::Height(height))
                    .expect("verbosity 3");
                assert_eq!(v3.tx.len(), block.tx.len(), "{height}");
                for (txid, full) in block.tx.iter().zip(&v3.tx).skip(1) {
                    let tx = chain.tx(txid).expect("a transaction");
                    assert_eq!(full.txid, *txid);
                    assert_eq!(tx.fee, None, "getrawtransaction gives no fee");
                    // Each input spends an output of a block at or below this one, worth what
                    // verbosity 3 says.
                    let mut spent = Vec::new();
                    for (input, described) in tx.vin.iter().zip(&full.vin) {
                        let parent = chain.tx(&input.txid.expect("an outpoint")).expect("parent");
                        let at = parent.height.expect("mined");
                        assert!(at >= 0 && at as u32 <= height, "{height}: from {at}");
                        let output = crate::views::spent_from_parent(&parent, input.vout.unwrap())
                            .expect("the output exists");
                        let prevout = described.prevout.as_ref().expect("a prevout");
                        assert_eq!(crate::views::spent_from_prevout(prevout), output);
                        spent.push(Some(output));
                    }
                    // So the fee the explorer works out is what the transaction paid.
                    let paid = chain.fee(txid).expect("a fee");
                    assert!(paid >= 0, "{height}: {paid}");
                    assert_eq!(
                        crate::views::tx_fee(&tx, &spent),
                        Some(launcher_types::explorer::Shown::Public { zat: paid }),
                        "{:?} {height} {txid}",
                        chain.network()
                    );
                    assert_eq!(full.fee, Some(paid));
                    // Scripts are kept as their lengths, which ZIP 317 counts: standard
                    // inputs of 148 bytes.
                    for input in &tx.vin {
                        assert_eq!(input.serialized_size(), 148);
                    }
                    assert_eq!(tx.size, full.size);
                }
            }
        }
    }

    #[test]
    fn big_blocks_hold_thousands_more_payments_under_two_megabytes() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW).with_big_blocks(10);
        let plain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let height = 3_550_990;
        assert!(chain.is_big(height) && !chain.is_big(height + 1));
        let block = chain.block(BlockId::Height(height)).unwrap();
        let small = plain.block(BlockId::Height(height)).unwrap();
        assert_eq!(block.tx.len(), small.tx.len() + FILLERS as usize);
        // Each stays under zebrad's 2,000,000 bytes, on either network.
        for chain in [
            chain.clone(),
            SynthChain::new(Network::Testnet, 4_500_000, NOW).with_big_blocks(10),
        ] {
            for height in (chain.tip() - 3_000..=chain.tip()).step_by(10) {
                let size = chain.block(BlockId::Height(height)).unwrap().size.unwrap();
                assert!((1_700_000..2_000_000).contains(&size), "{height}: {size}");
            }
        }
        // The last one is a payment from two coinbase outputs, as found by its id.
        let last = chain
            .tx(block.tx.last().unwrap())
            .expect("the last payment");
        assert_eq!((last.vin.len(), last.vout.len()), (2, 2));
        assert_eq!(chain.fee(&last.txid), Some(FILLER_FEE));
        assert_eq!(last.size, Some(FILLER_BYTES));
        assert_eq!(
            chain.tx(&chain.txid(height, 0, block.tx.len() as u16)),
            None
        );
        // Their fees are the block's: it still balances, and burns their share.
        for height in [height, height + 1, 3_551_000] {
            let block = chain.block(BlockId::Height(height)).unwrap();
            let parent = chain.block(BlockId::Height(height - 1)).unwrap();
            let coinbase = chain.tx(&block.tx[0]).unwrap();
            let ledger = block_ledger(&block, Some(&parent), Some(&coinbase), chain.heights());
            let subsidy = zec_to_zat(chain.subsidy(height).total) as u64;
            assert_eq!(
                ledger_check(&ledger, height, Some(subsidy), chain.heights()),
                LedgerCheck::Balanced,
                "{height}"
            );
        }
        let ledger = |chain: &SynthChain| {
            let block = chain.block(BlockId::Height(height)).unwrap();
            let parent = chain.block(BlockId::Height(height - 1)).unwrap();
            let coinbase = chain.tx(&block.tx[0]).unwrap();
            block_ledger(&block, Some(&parent), Some(&coinbase), chain.heights())
        };
        let (big, small) = (ledger(&chain), ledger(&plain));
        assert_eq!(
            big.fees.unwrap() - small.fees.unwrap(),
            FILLER_FEE as u64 * u64::from(FILLERS)
        );
        // Pools elsewhere agree with the plain chain's, but for what the big blocks burned.
        assert_eq!(chain.pools(3_000_000), plain.pools(3_000_000));
    }

    #[test]
    fn era_rules_hold() {
        for chain in chains() {
            let heights = chain.heights().clone();
            for height in heights_to_check(&chain) {
                let pools = chain.pools(height);
                let before = chain.pools(height - 1);
                let plan = chain.plan(height, chain.branch(height));
                if !heights.is_active(Upgrade::Nu6_3, height) {
                    assert_eq!(pools[Pool::Ironwood.index()], 0, "{height}");
                }
                if !heights.is_active(Upgrade::Nu5, height) {
                    assert_eq!(pools[Pool::Orchard.index()], 0, "{height}");
                }
                if heights.is_active(Upgrade::Canopy, height) {
                    assert!(
                        pools[Pool::Sprout.index()] <= before[Pool::Sprout.index()],
                        "no Sprout inflow at {height}"
                    );
                }
                if heights.is_active(Upgrade::Nu7, height) {
                    assert!(plan.iter().all(|b| b.tx.joinsplits.is_empty()), "{height}");
                    let block = chain.block(BlockId::Height(height)).unwrap();
                    let parent = chain.block(BlockId::Height(height - 1)).unwrap();
                    let notes: u32 = block
                        .trees
                        .sizes()
                        .iter()
                        .zip(parent.trees.sizes())
                        .map(|(after, before)| (after - before) as u32)
                        .sum();
                    let spends: u32 = plan.iter().map(|b| b.tx.sapling_spends.0).sum();
                    assert!(notes + spends <= 330, "{height}");
                }
                if heights.is_active(Upgrade::Nu6_3, height) {
                    for built in &plan {
                        let balance = built.tx.orchard.map_or(0, |o| o.value_balance);
                        assert!(balance >= 0, "{height}: {:?}", built.tx);
                    }
                    assert!(
                        plan[0].tx.orchard.is_none(),
                        "no Orchard coinbase at {height}"
                    );
                }
                if !heights.is_active(Upgrade::Nu6, height) {
                    assert_eq!(pools[Pool::Lockbox.index()], 0, "{height}");
                }
            }
            // The NU6.1 disbursement empties the lockbox on Mainnet.
            if chain.network() == Network::Mainnet {
                let nu6_1 = heights.height(Upgrade::Nu6_1).unwrap();
                let before = chain.pools(nu6_1 - 1)[Pool::Lockbox.index()];
                assert_eq!(before, NU6_1_DISBURSEMENT);
                let after = chain.pools(nu6_1)[Pool::Lockbox.index()];
                assert_eq!(after, chain.split(nu6_1).lockbox);
            }
        }
    }

    #[test]
    fn hashes_and_txids_lead_back_to_their_blocks() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let block = chain.block(BlockId::Height(3_550_000)).unwrap();
        assert!(block.hash.to_hex().starts_with("00000000"));
        assert_eq!(chain.block(BlockId::Hash(block.hash)), Some(block.clone()));
        assert_eq!(
            chain.header(BlockId::Hash(block.hash)).map(|h| h.height),
            Some(3_550_000)
        );
        let tx = chain.tx(&block.tx[1]).expect("a transaction");
        assert_eq!(tx.height, Some(3_550_000));
        assert_eq!(tx.block_hash, Some(block.hash));
        assert_eq!(tx.confirmations, Some(1_001));
        assert!(!tx.txid.to_hex().starts_with("00000000"));
        // Not ours: a hash and a txid of the other network, and a made-up one.
        let testnet = SynthChain::new(Network::Testnet, 3_551_000, NOW);
        assert_eq!(testnet.block(BlockId::Hash(block.hash)), None);
        assert_eq!(testnet.tx(&block.tx[1]), None);
        assert_eq!(chain.tx(&Hash32([7; 32])), None);
        // Above the tip there is nothing yet.
        assert_eq!(chain.block(BlockId::Height(3_551_001)), None);
        assert_eq!(chain.best().height, 3_551_000);
        assert_eq!(block.next, chain.hash(3_550_001));
        let tip = chain.block(BlockId::Height(3_551_000)).unwrap();
        assert_eq!(tip.next, None);
        assert_eq!(tip.previous, chain.hash(3_550_999));
    }

    #[test]
    fn mining_keeps_older_blocks_and_their_times() {
        let mut chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let before = chain.block(BlockId::Height(3_550_990)).unwrap();
        chain.mine(3);
        assert_eq!(chain.tip(), 3_551_003);
        assert_eq!(chain.tip_time(), NOW + 75);
        let after = chain.block(BlockId::Height(3_550_990)).unwrap();
        assert_eq!(after.time, before.time);
        assert_eq!(after.hash, before.hash);
        assert_eq!(after.confirmations, before.confirmations + 3);
        // Times run forward, give or take a third of the spacing.
        let older = chain.time_at(3_551_001);
        let newer = chain.time_at(3_551_002);
        assert!((newer - older - 25).abs() <= 16, "{older} {newer}");
    }

    #[test]
    fn a_reorg_replaces_the_top_blocks() {
        let mut chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let kept = chain.hash(3_550_998).unwrap();
        let replaced = chain.block(BlockId::Height(3_550_999)).unwrap();
        let replaced_tx = replaced.tx[1];
        assert_eq!(chain.reorg(2), 3_550_998);
        assert_eq!(chain.hash(3_550_998), Some(kept));
        let new = chain.block(BlockId::Height(3_550_999)).unwrap();
        assert_ne!(new.hash, replaced.hash);
        assert_eq!(new.previous, Some(kept));
        // zebrad looks blocks up by hash in its best chain only.
        assert_eq!(chain.block(BlockId::Hash(replaced.hash)), None);
        // A transaction of a replaced block reads as a side chain's.
        let side = chain.tx(&replaced_tx).expect("side chain transaction");
        assert_eq!(side.height, Some(-1));
        assert_eq!(side.block_hash, Some(replaced.hash));
        // The new branch balances too.
        for height in [3_550_999, 3_551_000] {
            let block = chain.block(BlockId::Height(height)).unwrap();
            let parent = chain.block(BlockId::Height(height - 1)).unwrap();
            let coinbase = chain.tx(&block.tx[0]).unwrap();
            let ledger = block_ledger(&block, Some(&parent), Some(&coinbase), chain.heights());
            let subsidy = zec_to_zat(chain.subsidy(height).total) as u64;
            assert_eq!(
                ledger_check(&ledger, height, Some(subsidy), chain.heights()),
                LedgerCheck::Balanced
            );
        }
        // A deeper reorg later reaches below the first one.
        chain.mine(5);
        assert_eq!(chain.reorg(10), 3_550_995);
        assert_eq!(chain.branch(3_550_996), 2);
        assert_eq!(chain.branch(3_550_995), 0);
        assert_eq!(chain.reorgs().collect::<Vec<_>>(), [3_550_998, 3_550_995]);
    }

    #[test]
    fn missing_rows_leave_zebrads_zero_diff() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW).with_missing_rows(7);
        let bare = (3_550_000..3_550_007).find(|h| h % 7 == 6).unwrap();
        let block = chain.block(BlockId::Height(bare)).unwrap();
        assert_eq!(block.value_pools, None);
        assert_eq!(block.size, None);
        let next = chain.block(BlockId::Height(bare + 1)).unwrap();
        let pools = next.value_pools.as_ref().unwrap();
        assert_eq!(
            pools[0].value_delta_zat,
            Some(pools[0].chain_value_zat as i64),
            "zebrad diffs against zero when only the parent lacks its row"
        );
        let coinbase = chain.tx(&next.tx[0]).unwrap();
        let ledger = block_ledger(&next, Some(&block), Some(&coinbase), chain.heights());
        assert_eq!(ledger.deltas, None);
        assert_eq!(ledger.fees, None);
    }

    #[test]
    fn the_synthetic_json_reads_back() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let block = chain.block(BlockId::Height(3_550_500)).unwrap();
        let json = serde_json::to_string(&block).unwrap();
        assert!(!json.contains("null"), "absent fields are left out: {json}");
        let back: BlockV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
        for txid in &block.tx {
            let tx = chain.tx(txid).unwrap();
            let back: Tx = serde_json::from_str(&serde_json::to_string(&tx).unwrap()).unwrap();
            assert_eq!(back, tx);
        }
        let subsidy = chain.subsidy(3_550_500);
        let back: Subsidy =
            serde_json::from_str(&serde_json::to_string(&subsidy).unwrap()).unwrap();
        assert_eq!(back, subsidy);
        // The genesis block's parent is 64 zeros, as zebrad writes it, and reads back as none.
        let genesis = chain.block(BlockId::Height(0)).unwrap();
        assert_eq!(genesis.previous, None);
        let json = serde_json::to_value(&genesis).unwrap();
        assert_eq!(json["previousblockhash"], "0".repeat(64));
        assert_eq!(serde_json::from_value::<BlockV1>(json).unwrap(), genesis);
    }

    #[test]
    fn upgrades_report_their_status() {
        let chain = SynthChain::new(Network::Mainnet, 3_551_000, NOW);
        let list = chain.upgrades(3_500_000);
        assert_eq!(list.len(), 11);
        assert_eq!(list[10].name, "NU7");
        assert_eq!(list[10].status, UpgradeStatus::Pending);
        assert_eq!(list[9].status, UpgradeStatus::Active);
        assert_eq!(chain.branch_id_at(3_543_405), "77190ad9");
        let address = chain.address(3);
        assert_eq!(address.len(), 35);
        assert!(address.starts_with("t1"));
    }

    #[test]
    fn a_short_chain_works() {
        let chain = SynthChain::new(Network::Mainnet, 9, NOW);
        for height in 0..=9 {
            let block = chain.block(BlockId::Height(height)).expect("block");
            assert!(!block.tx.is_empty());
            assert!(chain.tx(&block.tx[0]).is_some());
        }
        assert_eq!(chain.block(BlockId::Height(10)), None);
        assert_eq!(chain.supply(9), 9 * FIRST_SUBSIDY);
    }
}
