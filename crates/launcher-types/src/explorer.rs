//! The block explorer's wire types: what the UI asks the explorer backend (`explorer`
//! command), what it answers, and what it publishes on its own (`explorer_live` event). See
//! docs/EXPLORER.md.
//!
//! Amounts are integer zatoshis. A shielded amount that the chain hides is never a number
//! here: [`Shown::Hidden`] carries none.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::NodeId;

/// A 32-byte hash as zebrad prints it: 64 hex digits, lowercase on the way out, in zebrad's
/// display order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Hash32(pub [u8; 32]);

/// A block hash.
pub type BlockHash = Hash32;
/// A transaction id.
pub type TxId = Hash32;

impl Hash32 {
    /// `text` as a hash: exactly 64 hex digits, either case.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (index, pair) in bytes.chunks_exact(2).enumerate() {
            out[index] = (hex_digit(pair[0])? << 4) | hex_digit(pair[1])?;
        }
        Some(Hash32(out))
    }

    /// The 64 lowercase hex digits.
    pub fn to_hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(DIGITS[usize::from(byte >> 4)] as char);
            out.push(DIGITS[usize::from(byte & 0x0f)] as char);
        }
        out
    }
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash32({})", self.to_hex())
    }
}

impl FromStr for Hash32 {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Hash32::parse(text).ok_or_else(|| "not a 64-digit hex hash".to_owned())
    }
}

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    /// Reads the digits from the borrowed string where the format allows, without allocating.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HashVisitor;
        impl de::Visitor<'_> for HashVisitor {
            type Value = Hash32;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a 64-digit hex hash")
            }
            fn visit_str<E: de::Error>(self, text: &str) -> Result<Hash32, E> {
                Hash32::parse(text)
                    .ok_or_else(|| E::invalid_value(de::Unexpected::Str(text), &self))
            }
        }
        deserializer.deserialize_str(HashVisitor)
    }
}

/// A block by height or by hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "snake_case")]
pub enum BlockId {
    Height(u32),
    Hash(BlockHash),
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BlockId::Height(height) => write!(f, "{height}"),
            BlockId::Hash(hash) => write!(f, "{hash}"),
        }
    }
}

/// zebrad's value pools, in zebrad's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    Transparent,
    Sprout,
    Sapling,
    Orchard,
    /// The deferred development fund (ZIP 1015), funded from NU6.
    Lockbox,
    /// The pool NU6.3 opens.
    Ironwood,
}

impl Pool {
    /// Every pool, in zebrad's order: the order of [`PerPool`].
    pub const ALL: [Pool; 6] = [
        Pool::Transparent,
        Pool::Sprout,
        Pool::Sapling,
        Pool::Orchard,
        Pool::Lockbox,
        Pool::Ironwood,
    ];

    /// The pools that hold notes: their amounts are hidden.
    pub const SHIELDED: [Pool; 4] = [Pool::Sprout, Pool::Sapling, Pool::Orchard, Pool::Ironwood];

    /// The position in [`Pool::ALL`].
    pub fn index(self) -> usize {
        self as usize
    }

    /// zebrad's id in `valuePools`: `transparent`, `sprout`, ...
    pub fn id(self) -> &'static str {
        match self {
            Pool::Transparent => "transparent",
            Pool::Sprout => "sprout",
            Pool::Sapling => "sapling",
            Pool::Orchard => "orchard",
            Pool::Lockbox => "lockbox",
            Pool::Ironwood => "ironwood",
        }
    }

    /// The pool for zebrad's id.
    pub fn from_id(id: &str) -> Option<Pool> {
        Pool::ALL.into_iter().find(|pool| pool.id() == id)
    }

    /// The pool's name for people.
    pub fn label(self) -> &'static str {
        match self {
            Pool::Transparent => "Transparent",
            Pool::Sprout => "Sprout",
            Pool::Sapling => "Sapling",
            Pool::Orchard => "Orchard",
            Pool::Lockbox => "Lockbox",
            Pool::Ironwood => "Ironwood",
        }
    }

    /// Whether the pool holds notes, whose amounts are hidden.
    pub fn is_shielded(self) -> bool {
        Pool::SHIELDED.contains(&self)
    }
}

/// One value per pool, in [`Pool::ALL`]'s order; an array of six on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PerPool<T>(pub [T; 6]);

impl<T: Copy> PerPool<T> {
    /// The value for `pool`.
    pub fn get(&self, pool: Pool) -> T {
        self.0[pool.index()]
    }

    /// The pools with their values.
    pub fn iter(&self) -> impl Iterator<Item = (Pool, T)> + '_ {
        Pool::ALL.into_iter().zip(self.0.iter().copied())
    }
}

impl<T> PerPool<T> {
    /// Sets the value for `pool`.
    pub fn set(&mut self, pool: Pool, value: T) {
        self.0[pool.index()] = value;
    }
}

/// An amount that is public, hidden by the chain's design, or unknown to Zeal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shown", rename_all = "snake_case")]
pub enum Shown {
    /// Public on the chain.
    Public { zat: i64 },
    /// Hidden by design: there is no number to show.
    Hidden,
    /// Public, but Zeal does not have it.
    Unknown { why: UnknownWhy },
}

/// Why a public amount is unknown ([`Shown::Unknown`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownWhy {
    /// zebrad has no value-pool row for the block or its parent.
    NoBlockInfo,
    /// It needs the values of the outputs the transaction spends, which this page does not
    /// read yet.
    InputValues,
}

/// The chain as the explorer saw it when it answered: its tip, and how many times the
/// blocks near the tip were replaced since it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainView {
    pub height: u32,
    pub hash: BlockHash,
    /// Bumped once per reorganisation the explorer followed.
    pub epoch: u64,
    /// Served from memory while the node is not answering.
    pub stale: bool,
}

/// How full a block is in the unit its era limits it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", rename_all = "snake_case")]
pub enum Budget {
    /// ZIP 218 from NU7: shielded spends, outputs and actions against the limit. From the
    /// note commitment trees alone Sapling spends are not counted, so this is a lower bound
    /// while `complete` is false.
    Units {
        used: u32,
        limit: u32,
        complete: bool,
    },
    /// Before NU7: the block's size against the 2,000,000-byte limit.
    Bytes { used: u32, limit: u32 },
    /// The size is unknown: zebrad has no row for the block.
    Unknown,
}

/// Whether a block's value flows add up the way its era's rules say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum LedgerCheck {
    /// The pool changes, the subsidy and the fees agree.
    Balanced,
    /// Before NU6 a miner may claim less than the subsidy and the fees; this much was left
    /// unclaimed.
    Unclaimed { zat: u64 },
    /// They do not agree: the numbers are shown, but not vouched for.
    Unverified,
    /// Not checked: the subsidy or the pool changes are unknown.
    NotChecked,
}

/// What one block did to the value pools, from the block, its parent and its coinbase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockLedger {
    /// The pools after the block; `None` without zebrad's value-pool row for it.
    pub pools: Option<PerPool<u64>>,
    /// The change of each pool; `None` unless both the block and its parent have a row.
    pub deltas: Option<PerPool<i64>>,
    /// What the coinbase paid into each pool: transparent outputs and shielded coinbase
    /// outputs. Public by consensus.
    pub coinbase: Option<PerPool<u64>>,
    /// The net flow of the block's other transactions into each pool: the change minus the
    /// coinbase. Fees leave the pools they are paid from.
    pub users: Option<PerPool<i64>>,
    /// The fees paid in the block: the coinbase's outputs minus what the five pools other
    /// than the lockbox gained.
    pub fees: Option<u64>,
    /// The share of the fees removed from circulation from NU7 (⌊6 F / 10⌋).
    pub burned: Option<u64>,
    /// Note commitments created, from the trees' sizes: Sapling, Orchard, Ironwood.
    pub notes: [u32; 3],
    pub budget: Budget,
}

/// A block in a list: the Chain page's strip and recent blocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSummary {
    pub height: u32,
    pub hash: BlockHash,
    /// The block time, in seconds since the Unix epoch.
    pub time: u64,
    /// Seconds since the parent's time; miners set block times, so it can be negative.
    pub interval: Option<i64>,
    /// The size in bytes, when zebrad has a row for the block.
    pub size: Option<u32>,
    pub tx_count: u32,
    pub ledger: BlockLedger,
}

/// The Chain page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChainPage {
    /// The newest blocks, newest first.
    pub blocks: Vec<BlockSummary>,
    /// The average seconds between the last 100 blocks.
    pub average_interval: Option<f64>,
    /// The seconds the network aims for between blocks at the tip.
    pub target_spacing: u32,
    /// The supply at the tip, in zatoshis: the sum of the pools.
    pub supply: Option<u64>,
    /// Removed from circulation since NU7: the subsidies minus the growth of the supply.
    pub burned_since_nu7: Option<u64>,
    /// The next upgrade that has an activation height above the tip.
    pub next_upgrade: Option<UpgradeCountdown>,
    /// The network difficulty at the tip.
    pub difficulty: Option<f64>,
}

/// An upgrade that has not activated yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeCountdown {
    pub name: String,
    pub height: u32,
    pub blocks_left: u32,
    /// The time those blocks take at the target spacing of each era they cross.
    pub eta_secs: u64,
}

/// Where a block's subsidy went (`getblocksubsidy` with the block's fees).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardSplit {
    /// The block subsidy, fees not included.
    pub subsidy: u64,
    /// The miner's part of the subsidy.
    pub miner: u64,
    /// The funding and lockbox streams, as zebrad names them.
    pub streams: Vec<RewardStream>,
    /// The fees the miner could claim: the fees less the burned share.
    pub fees_to_miner: Option<u64>,
    /// The burned share of the fees.
    pub burned: Option<u64>,
}

/// One funding or lockbox stream of a block's subsidy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardStream {
    pub recipient: String,
    pub zat: u64,
    /// The payout address; lockbox streams have none.
    pub address: Option<String>,
    /// Deferred into the lockbox rather than paid out.
    pub lockbox: bool,
}

/// One page of a block's transaction ids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxidPage {
    pub offset: u32,
    pub total: u32,
    pub txids: Vec<TxId>,
}

/// The Block page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockPage {
    pub summary: BlockSummary,
    pub previous: Option<BlockHash>,
    pub next: Option<BlockHash>,
    /// The upgrade whose rules the block follows.
    pub era: Option<String>,
    /// The upgrade this block activates, when it is the first block of one.
    pub activates: Option<String>,
    /// The subsidy split; `None` when zebrad did not answer `getblocksubsidy`.
    pub reward: Option<RewardSplit>,
    /// Whether the block's value flows add up.
    pub check: LedgerCheck,
    pub details: BlockDetails,
    /// The first page of transaction ids.
    pub txids: TxidPage,
}

/// The header fields the Block page folds away.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BlockDetails {
    pub version: Option<u32>,
    pub merkle_root: Option<String>,
    pub block_commitments: Option<String>,
    pub final_sapling_root: Option<String>,
    pub final_orchard_root: Option<String>,
    pub bits: Option<String>,
    pub nonce: Option<String>,
    pub difficulty: Option<f64>,
    /// The note commitment trees' sizes after the block: Sapling, Orchard, Ironwood.
    pub trees: [u64; 3],
    /// The supply after the block, when zebrad has a row for it.
    pub chain_supply: Option<u64>,
}

/// A height above the node's tip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FutureBlock {
    pub height: u32,
    pub tip: u32,
    pub blocks_left: u32,
    /// The time those blocks take at the target spacing of each era they cross.
    pub eta_secs: u64,
    /// The upgrade that activates at exactly this height.
    pub activates: Option<String>,
    /// The upgrades that activate between the tip and this height.
    pub upgrades_before: Vec<(String, u32)>,
    /// The last height this zebrad release runs at, if it reports one.
    pub end_of_support: Option<u64>,
}

/// What a transaction does with value, read from its public parts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TxKind {
    /// A block's first transaction, with the shielded pools it paid into.
    Coinbase { shielded_to: Vec<Pool> },
    /// No shielded parts.
    Transparent,
    /// No transparent parts, one shielded pool. From NU6.3 every Orchard action pays the
    /// receiver of the note it spends (`same_receiver`).
    Shielded { pool: Pool, same_receiver: bool },
    /// No transparent parts; one pool loses value and another gains it, and the amount that
    /// moved is public.
    Migration { from: Pool, to: Pool, zat: u64 },
    /// Value enters shielded pools.
    Shielding {
        into: Vec<Pool>,
        transparent_change: bool,
    },
    /// Value leaves shielded pools for transparent outputs.
    Deshielding { from: Vec<Pool> },
    /// Anything else.
    Mixed,
}

/// Where a transaction is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TxStatus {
    /// Waiting in the node's mempool.
    Mempool,
    /// In a block of the node's best chain.
    Mined {
        block: BlockHash,
        height: u32,
        time: Option<u64>,
    },
    /// In a block the node has, outside its best chain.
    SideChain { block: Option<BlockHash> },
}

/// A transaction output another transaction can spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutPoint {
    pub txid: TxId,
    pub vout: u32,
}

/// A transparent input. Its value is the spent output's, which this page does not read yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxInput {
    /// The output it spends; `None` for a coinbase input.
    pub prevout: Option<OutPoint>,
}

/// A transparent output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxOutput {
    pub n: u32,
    pub zat: u64,
    /// The address it pays, when the script has one.
    pub address: Option<String>,
    /// zebrad's script type: `pubkeyhash`, `scripthash`, `nulldata`, ...
    pub script: String,
}

/// A transaction's part in one shielded pool: counts, and the net value that left the pool
/// (negative when value entered it). Per-note amounts are hidden.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolPart {
    pub pool: Pool,
    /// Sapling spends.
    pub spends: u32,
    /// Sapling outputs.
    pub outputs: u32,
    /// Orchard or Ironwood actions.
    pub actions: u32,
    /// Sprout JoinSplits.
    pub joinsplits: u32,
    /// The value that left the pool in this transaction; negative when value entered it.
    pub net_out: i64,
}

/// The Transaction page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TxPage {
    pub txid: TxId,
    pub status: TxStatus,
    pub kind: TxKind,
    pub version: u32,
    pub size: Option<u32>,
    pub lock_time: u32,
    /// `None` when the transaction does not expire (zebrad reports 0).
    pub expiry_height: Option<u32>,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    /// One entry per shielded pool the transaction touches.
    pub shielded: Vec<PoolPart>,
    /// The fee, public but unknown while transparent inputs' values are not read; not
    /// applicable to a coinbase.
    pub fee: Option<Shown>,
}

/// Where a query leads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "to", rename_all = "snake_case")]
pub enum Resolution {
    /// One page.
    Go { route: ExplorerRoute },
    /// The query names several things (a hash that is both a block and a transaction).
    Choose { routes: Vec<ExplorerRoute> },
    /// Nothing, for this reason.
    Explain { hint: NotFoundHint },
}

/// Why a query leads nowhere, with what the page can offer instead.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "hint", rename_all = "snake_case")]
pub enum NotFoundHint {
    /// The node has blocks up to `node_height`; the network is further on.
    PastNodeTip { node_height: u32 },
    /// Neither a block in the node's best chain nor a transaction it knows.
    UnknownHash,
    /// Something the search does not look up yet, such as an address.
    NotSearchable { what: String },
    /// A key or a recovery phrase: refused, never stored or sent.
    Refused { what: SecretKind },
    /// Not a height, a hash or anything else the search reads.
    Unrecognised,
}

/// What a refused query looks like.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretKind {
    ViewingKey,
    SpendingKey,
    RecoveryPhrase,
}

/// An Explorer page, as a value the history keeps and a text the search box accepts
/// (`block/3120000`, `tx/<txid>?out=1`, `search?q=…`; the Chain page is empty).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "page", rename_all = "snake_case")]
pub enum ExplorerRoute {
    Chain,
    Block { id: BlockId },
    Tx { txid: TxId, out: Option<u32> },
    Search { query: String },
}

impl ExplorerRoute {
    /// The route's text.
    pub fn to_text(&self) -> String {
        match self {
            ExplorerRoute::Chain => String::new(),
            ExplorerRoute::Block { id } => format!("block/{id}"),
            ExplorerRoute::Tx { txid, out: None } => format!("tx/{txid}"),
            ExplorerRoute::Tx {
                txid,
                out: Some(out),
            } => format!("tx/{txid}?out={out}"),
            ExplorerRoute::Search { query } => format!("search?q={}", percent_encode(query)),
        }
    }

    /// The route a text names, as [`ExplorerRoute::to_text`] writes it.
    pub fn parse(text: &str) -> Option<ExplorerRoute> {
        let text = text.trim().trim_start_matches('/');
        if text.is_empty() {
            return Some(ExplorerRoute::Chain);
        }
        let (path, query) = text.split_once('?').unwrap_or((text, ""));
        let param = |name: &str| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
        };
        if let Some(id) = path.strip_prefix("block/") {
            let id = match Hash32::parse(id) {
                Some(hash) => BlockId::Hash(hash),
                None => BlockId::Height(parse_height(id)?),
            };
            return Some(ExplorerRoute::Block { id });
        }
        if let Some(txid) = path.strip_prefix("tx/") {
            let out = match param("out") {
                Some(out) => Some(out.parse().ok()?),
                None => None,
            };
            return Some(ExplorerRoute::Tx {
                txid: Hash32::parse(txid)?,
                out,
            });
        }
        if path == "search" {
            return Some(ExplorerRoute::Search {
                query: percent_decode(param("q")?)?,
            });
        }
        None
    }

    /// The page the route shows, for [`ExplorerRequest::Interest`].
    pub fn page(&self) -> PageKind {
        match self {
            ExplorerRoute::Chain => PageKind::Chain,
            ExplorerRoute::Block { .. } => PageKind::Block,
            ExplorerRoute::Tx { .. } => PageKind::Tx,
            ExplorerRoute::Search { .. } => PageKind::Search,
        }
    }
}

/// Decimal digits without a leading zero (but `0` itself), at most nine of them.
fn parse_height(text: &str) -> Option<u32> {
    let valid = !text.is_empty()
        && text.len() <= 9
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    valid.then(|| text.parse().ok()).flatten()
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let high = hex_digit(*bytes.get(index + 1)?)?;
                let low = hex_digit(*bytes.get(index + 2)?)?;
                out.push((high << 4) | low);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// The kind of page a route shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    Chain,
    Block,
    Tx,
    Search,
}

/// A request to the explorer of one node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "q", rename_all = "snake_case")]
pub enum ExplorerRequest {
    /// The explorer's state, as `explorer_live` publishes it.
    Status,
    /// Whether the Explorer is on screen, and which page: the explorer follows the tip only
    /// while it is.
    Interest { visible: bool, page: PageKind },
    /// The Chain page.
    Chain,
    /// The Block page; a height above the tip answers [`ExplorerResponse::Future`].
    Block { id: BlockId },
    /// A page of a block's transaction ids.
    BlockTxs {
        hash: BlockHash,
        offset: u32,
        limit: u16,
    },
    /// The Transaction page.
    Tx { txid: TxId },
    /// Where a search query leads.
    Resolve { query: String },
}

/// A call of the `explorer` command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerCall {
    pub request: ExplorerRequest,
}

/// The answer to an [`ExplorerCall`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExplorerReply {
    /// The chain the answer was read against.
    pub chain: Option<ChainView>,
    pub response: ExplorerResponse,
}

/// One answer per request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "r", content = "value", rename_all = "snake_case")]
pub enum ExplorerResponse {
    Status(ExplorerLive),
    /// The request needs no answer ([`ExplorerRequest::Interest`]).
    Done,
    Chain(ChainPage),
    Block(Box<BlockPage>),
    Future(FutureBlock),
    BlockTxs(TxidPage),
    Tx(Box<TxPage>),
    Resolve(Resolution),
}

/// Why an explorer call failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerError {
    pub kind: ExplorerErrorKind,
    /// A message for people.
    pub message: String,
}

impl ExplorerError {
    pub fn new(kind: ExplorerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for ExplorerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExplorerError {}

/// What kind of problem an [`ExplorerError`] is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExplorerErrorKind {
    /// No explorer for this node id.
    UnknownNode,
    /// The node is not running, or has no RPC address yet.
    NodeNotRunning,
    /// The node has blocks up to `node_height`.
    PastTip { height: u32, node_height: u32 },
    /// The node does not have it.
    NotFound,
    /// A key or a recovery phrase: refused, never stored or sent.
    Refused { what: SecretKind },
    /// The request is malformed.
    InvalidQuery,
    /// The node answered with a JSON-RPC error.
    Rpc { code: i64 },
    /// The node did not answer in time.
    Timeout,
    /// The explorer could not finish for a reason of its own.
    Internal,
}

/// What the explorer publishes on its own, as the `explorer_live` event's `live`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerLive {
    pub state: LiveState,
    /// The tip the explorer follows, once it has read one.
    pub tip: Option<ChainView>,
    /// The last reorganisation the explorer followed.
    pub reorg: Option<ReorgNotice>,
}

/// Whether the explorer can read its node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LiveState {
    /// The node does not run, or has not opened its RPC port yet.
    #[default]
    NotRunning,
    /// The node is catching up: it has blocks up to `height` of about `estimated`.
    Syncing { height: u32, estimated: u32 },
    /// The node is at the network's tip.
    AtTip,
}

/// A reorganisation the explorer followed: the blocks above `fork_height` were replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReorgNotice {
    /// The last height both chains share.
    pub fork_height: u32,
    /// How many blocks were replaced.
    pub replaced: u32,
    /// The epoch the reorganisation started.
    pub epoch: u64,
    /// When the explorer noticed, in seconds since the Unix epoch.
    pub at: u64,
}

/// The `explorer_live` event: one node's [`ExplorerLive`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplorerLiveEvent {
    pub node: NodeId,
    pub live: ExplorerLive,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const HASH: &str = "00000000012d1f2a7b4e7a0d0ffb3a2b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0a";

    #[test]
    fn hashes_are_64_hex_digits() {
        let hash = Hash32::parse(HASH).expect("a hash");
        assert_eq!(hash.to_hex(), HASH);
        assert_eq!(
            Hash32::parse(&HASH.to_uppercase()),
            Some(hash),
            "either case"
        );
        assert_eq!(Hash32::parse(&HASH[1..]), None);
        assert_eq!(Hash32::parse(&format!("{}g", &HASH[1..])), None);
        assert_eq!(serde_json::to_value(hash).unwrap(), json!(HASH));
        let back: Hash32 = serde_json::from_value(json!(HASH)).unwrap();
        assert_eq!(back, hash);
        assert!(serde_json::from_value::<Hash32>(json!("00")).is_err());
    }

    #[test]
    fn wire_shapes() {
        let hash = Hash32::parse(HASH).unwrap();
        assert_eq!(
            serde_json::to_value(BlockId::Height(7)).unwrap(),
            json!({"by": "height", "value": 7})
        );
        assert_eq!(
            serde_json::to_value(BlockId::Hash(hash)).unwrap(),
            json!({"by": "hash", "value": HASH})
        );
        assert_eq!(
            serde_json::to_value(Shown::Hidden).unwrap(),
            json!({"shown": "hidden"})
        );
        assert_eq!(
            serde_json::to_value(Shown::Unknown {
                why: UnknownWhy::InputValues
            })
            .unwrap(),
            json!({"shown": "unknown", "why": "input_values"})
        );
        assert_eq!(
            serde_json::to_value(ExplorerRequest::Block {
                id: BlockId::Height(1)
            })
            .unwrap(),
            json!({"q": "block", "id": {"by": "height", "value": 1}})
        );
        assert_eq!(
            serde_json::to_value(ExplorerResponse::Done).unwrap(),
            json!({"r": "done"})
        );
        assert_eq!(
            serde_json::to_value(ExplorerErrorKind::PastTip {
                height: 9,
                node_height: 5
            })
            .unwrap(),
            json!({"kind": "past_tip", "height": 9, "node_height": 5})
        );
        assert_eq!(
            serde_json::to_value(TxKind::Migration {
                from: Pool::Orchard,
                to: Pool::Ironwood,
                zat: 5
            })
            .unwrap(),
            json!({"kind": "migration", "from": "orchard", "to": "ironwood", "zat": 5})
        );
        assert_eq!(
            serde_json::to_value(PerPool([1u64, 2, 3, 4, 5, 6])).unwrap(),
            json!([1, 2, 3, 4, 5, 6])
        );
        let live = ExplorerLive {
            state: LiveState::Syncing {
                height: 1,
                estimated: 2,
            },
            tip: None,
            reorg: None,
        };
        let back: ExplorerLive =
            serde_json::from_value(serde_json::to_value(&live).unwrap()).unwrap();
        assert_eq!(back, live);
    }

    #[test]
    fn pools_keep_zebrad_order_and_ids() {
        for (index, pool) in Pool::ALL.into_iter().enumerate() {
            assert_eq!(pool.index(), index);
            assert_eq!(Pool::from_id(pool.id()), Some(pool));
            assert_eq!(serde_json::to_value(pool).unwrap(), json!(pool.id()));
        }
        assert!(!Pool::Transparent.is_shielded());
        assert!(!Pool::Lockbox.is_shielded());
        assert!(Pool::Ironwood.is_shielded());
        let mut pools = PerPool([0i64; 6]);
        pools.set(Pool::Ironwood, -3);
        assert_eq!(pools.get(Pool::Ironwood), -3);
        assert_eq!(pools.iter().last(), Some((Pool::Ironwood, -3)));
    }

    #[test]
    fn routes_round_trip_through_their_text() {
        let hash = Hash32::parse(HASH).unwrap();
        for route in [
            ExplorerRoute::Chain,
            ExplorerRoute::Block {
                id: BlockId::Height(3_120_000),
            },
            ExplorerRoute::Block {
                id: BlockId::Hash(hash),
            },
            ExplorerRoute::Tx {
                txid: hash,
                out: None,
            },
            ExplorerRoute::Tx {
                txid: hash,
                out: Some(2),
            },
            ExplorerRoute::Search {
                query: "tip - 1 & more?".into(),
            },
        ] {
            let text = route.to_text();
            assert_eq!(ExplorerRoute::parse(&text), Some(route), "{text}");
        }
        assert_eq!(
            ExplorerRoute::parse("block/3120000").map(|route| route.page()),
            Some(PageKind::Block)
        );
        assert_eq!(ExplorerRoute::parse("/"), Some(ExplorerRoute::Chain));
        let bad_out = format!("tx/{HASH}?out=x");
        for bad in [
            "block/0123",
            "block/1234567890",
            "block/abc",
            "tx/12",
            bad_out.as_str(),
            "mempool",
            "search",
            "search?q=%G1",
        ] {
            assert_eq!(ExplorerRoute::parse(bad), None, "{bad}");
        }
    }
}
