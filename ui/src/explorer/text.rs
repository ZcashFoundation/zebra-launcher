//! The Explorer's words and numbers: amounts in ZEC (TAZ on Testnet), pool movements, block
//! and transaction sentences, budgets, confirmations, search hints and refusals; a block's
//! rows, a transaction's flow (V6) and its fee card. Pure, tested natively. A hidden amount
//! has no number here: callers show "hidden" instead.

use explorer_model::{
    classify::{Classified, Later},
    flow::{Balance, Flow, FlowKey},
    zip317,
};
use launcher_types::{
    Network, Phase,
    explorer::{
        BlockSummary, Budget, LedgerCheck, LogicalActions, NotFoundHint, PerPool, Pool, PoolPart,
        ReadOutcome, ReorgNotice, SecretKind, Shown, SourceCall, TxKind, TxPage, TxRow, TxStatus,
        UnknownWhy,
    },
};

use std::fmt;

use crate::format::{bytes, datetime_short, local_datetime_short, thousands};

/// Blocks this deep are final in Zebra.
pub const FINAL_DEPTH: u32 = 1_000;

/// The unit amounts read in on `network`: Testnet coins are not ZEC.
pub fn unit(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "ZEC",
        Network::Testnet => "TAZ",
    }
}

/// An exact amount in whole coins, without trailing zeros: `1.5625`, `0.0010007`, `2`,
/// `-0.5`, `14,654,995.76514816`.
pub fn coins(zat: i64) -> String {
    let sign = if zat < 0 { "-" } else { "" };
    let magnitude = zat.unsigned_abs();
    let whole = thousands(magnitude / 100_000_000);
    let fraction = format!("{:08}", magnitude % 100_000_000);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        format!("{sign}{whole}")
    } else {
        format!("{sign}{whole}.{fraction}")
    }
}

/// How amounts read: in the network's coin (ZEC, or TAZ on Testnet), or in zatoshis, the
/// chain's own whole unit. The source line's toggle picks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unit {
    coin: &'static str,
    zat: bool,
}

impl Unit {
    /// `network`'s coin, or zatoshis.
    pub fn new(network: Network, zat: bool) -> Self {
        Unit {
            coin: unit(network),
            zat,
        }
    }

    /// The number alone: `1.5625`, or `156,250,000` in zatoshis.
    pub fn number(self, zat: i64) -> String {
        if self.zat {
            let sign = if zat < 0 { "-" } else { "" };
            format!("{sign}{}", thousands(zat.unsigned_abs()))
        } else {
            coins(zat)
        }
    }
}

/// The unit's name: `ZEC`, `TAZ` or `zat`.
impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.zat { "zat" } else { self.coin })
    }
}

/// An exact amount with its unit: `1.5625 ZEC`, `156,250,000 zat`.
pub fn amount(zat: i64, unit: Unit) -> String {
    format!("{} {unit}", unit.number(zat))
}

/// A large amount rounded to two decimals from 1,000 coins up, exact below and in zatoshis:
/// pool balances and the supply.
pub fn amount_rounded(zat: u64, unit: Unit) -> String {
    if zat < 100_000_000_000 || unit.zat {
        return amount(i64::try_from(zat).unwrap_or(i64::MAX), unit);
    }
    let hundredths = (zat + 500_000) / 1_000_000;
    format!(
        "{}.{:02} {unit}",
        thousands(hundredths / 100),
        hundredths % 100
    )
}

/// What a transaction or a block did to one pool, as a movement: `1.2 ZEC entered the
/// Ironwood pool`, `0.5 ZEC left the Orchard pool`.
pub fn movement(pool: Pool, into: i64, unit: Unit) -> String {
    match into {
        0 => format!("No change in the {} pool", pool.label()),
        into if into > 0 => format!("{} entered the {} pool", amount(into, unit), pool.label()),
        out => format!("{} left the {} pool", amount(-out, unit), pool.label()),
    }
}

/// What users moved into the shielded pools in a block, net of what they took out.
pub fn net_shielding(users: &PerPool<i64>) -> i64 {
    Pool::SHIELDED.iter().map(|pool| users.get(*pool)).sum()
}

/// The net shielding as words: `13.4 ZEC into shielded pools`.
pub fn shielding_text(net: i64, unit: Unit) -> String {
    match net {
        0 => "nothing in or out of shielded pools".to_owned(),
        net if net > 0 => format!("{} into shielded pools", amount(net, unit)),
        net => format!("{} out of shielded pools", amount(-net, unit)),
    }
}

/// The notes a block created, `212 note commitments`, or `None` when unknown.
pub fn notes_text(notes: Option<[u32; 3]>) -> Option<String> {
    let total: u32 = notes?.iter().sum();
    Some(match total {
        1 => "1 note commitment".to_owned(),
        n => format!("{} note commitments", thousands(u64::from(n))),
    })
}

/// A block in one sentence: `37 transactions · 212 note commitments · users moved 13.4 ZEC
/// into shielded pools · fees 0.0021 ZEC (0.00126 burned)`.
pub fn block_sentence(summary: &BlockSummary, unit: Unit) -> String {
    let ledger = &summary.ledger;
    let mut parts = vec![match summary.tx_count {
        1 => "1 transaction".to_owned(),
        n => format!("{} transactions", thousands(u64::from(n))),
    }];
    parts.extend(notes_text(ledger.notes));
    if let Some(users) = &ledger.users {
        parts.push(format!(
            "users moved {}",
            shielding_text(net_shielding(users), unit)
        ));
    }
    match (ledger.fees, ledger.burned) {
        (Some(fees), Some(burned)) => parts.push(format!(
            "fees {} ({} burned)",
            amount(fees as i64, unit),
            unit.number(burned as i64)
        )),
        (Some(fees), None) => parts.push(format!("fees {}", amount(fees as i64, unit))),
        (None, _) => parts.push("fees unknown".to_owned()),
    }
    parts.join(" · ")
}

/// How full a block is: `≥ 39 of 330`, `70.6 kB of 2.0 MB`.
pub fn budget_text(budget: &Budget) -> String {
    match *budget {
        Budget::Units {
            used,
            limit,
            complete,
        } => {
            let at_least = if complete { "" } else { "≥ " };
            format!("{at_least}{used} of {limit}")
        }
        Budget::Bytes { used, limit } => {
            format!("{} of {}", bytes(u64::from(used)), bytes(u64::from(limit)))
        }
        Budget::Unknown => "unknown".to_owned(),
    }
}

/// A budget as a fraction of its limit, for a meter.
pub fn budget_fraction(budget: &Budget) -> Option<f64> {
    match *budget {
        Budget::Units { used, limit, .. } | Budget::Bytes { used, limit } if limit > 0 => {
            Some(f64::from(used) / f64::from(limit))
        }
        _ => None,
    }
}

/// The era's check as a caption.
pub fn check_text(check: &LedgerCheck, unit: Unit) -> String {
    match check {
        LedgerCheck::Balanced => "Pool changes, the subsidy and the fees add up ✓".to_owned(),
        LedgerCheck::Unclaimed { zat } => format!(
            "The miner left {} of the subsidy and fees unclaimed",
            amount(*zat as i64, unit)
        ),
        LedgerCheck::Unverified => {
            "Unverified: the pool changes, the subsidy and the fees do not add up".to_owned()
        }
        LedgerCheck::NotChecked => {
            "Not checked: the subsidy or the pool changes are unknown".to_owned()
        }
    }
}

fn pools_list(pools: &[Pool]) -> String {
    let names: Vec<&str> = pools.iter().map(|pool| pool.label()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A transaction kind's badge: `Shielding`, `Orchard → Ironwood`.
pub fn kind_label(kind: &TxKind) -> String {
    match kind {
        TxKind::Coinbase { .. } => "Coinbase".to_owned(),
        TxKind::Transparent => "Transparent".to_owned(),
        TxKind::Shielded { pool, .. } => format!("Fully shielded · {}", pool.label()),
        TxKind::Migration { from, to, .. } => format!("{} → {}", from.label(), to.label()),
        TxKind::Shielding { .. } => "Shielding".to_owned(),
        TxKind::Deshielding { .. } => "Deshielding".to_owned(),
        TxKind::Mixed => "Mixed".to_owned(),
    }
}

/// A transaction in a sentence, from its public parts only.
pub fn tx_sentence(page: &TxPage, unit: Unit) -> String {
    let counts: Vec<String> = page
        .shielded
        .iter()
        .map(|part| match part.pool {
            Pool::Sapling => format!(
                "{} Sapling spend{} and {} output{}",
                part.spends,
                plural(part.spends),
                part.outputs,
                plural(part.outputs)
            ),
            Pool::Sprout => format!("{} JoinSplit{}", part.joinsplits, plural(part.joinsplits)),
            pool => format!(
                "{} {} action{}",
                part.actions,
                pool.label(),
                plural(part.actions)
            ),
        })
        .collect();
    let what = match &page.kind {
        TxKind::Coinbase { shielded_to } if shielded_to.is_empty() => {
            "The block's coinbase: the subsidy and the fees, paid to transparent outputs."
                .to_owned()
        }
        TxKind::Coinbase { shielded_to } => format!(
            "The block's coinbase, paying into {}.",
            pools_list(shielded_to)
        ),
        TxKind::Transparent => "Moved value between transparent addresses.".to_owned(),
        TxKind::Shielded { pool, .. } => {
            format!("Moved value within the {} pool.", pool.label())
        }
        TxKind::Migration { from, to, zat } => format!(
            "Moved {} from {} to {}.",
            amount(*zat as i64, unit),
            from.label(),
            to.label()
        ),
        TxKind::Shielding {
            into,
            transparent_change,
        } => {
            let change = if *transparent_change {
                ", with transparent change"
            } else {
                ""
            };
            format!("Moved transparent value into {}{change}.", pools_list(into))
        }
        TxKind::Deshielding { from } => {
            format!(
                "Moved value out of {} to transparent outputs.",
                pools_list(from)
            )
        }
        TxKind::Mixed => "Moved value between transparent and shielded pools.".to_owned(),
    };
    let mut sentence = what;
    if !counts.is_empty() {
        sentence.push_str(&format!(
            " {}; {}.",
            capitalise(&counts.join(" and ")),
            note_amounts(&page.kind)
        ));
    }
    match page.fee {
        Some(Shown::Public { zat }) => sentence.push_str(&format!(" Fee {}.", amount(zat, unit))),
        Some(Shown::Unknown {
            why: UnknownWhy::InputValues,
        }) => sentence.push_str(" The fee needs the values of the spent outputs."),
        Some(Shown::Unknown { .. } | Shown::Hidden) | None => {}
    }
    sentence
}

/// What the page says of each shielded note's amount: hidden, except in a coinbase, whose
/// shielded outputs anyone can decrypt (consensus makes them use the all-zero outgoing viewing
/// key), though the Explorer does not.
pub fn note_amounts(kind: &TxKind) -> &'static str {
    match kind {
        TxKind::Coinbase { .. } => {
            "amounts per note are public by consensus, but the Explorer does not decrypt notes"
        }
        _ => "amounts per note hidden",
    }
}

fn plural(count: u32) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Confirmations of a block at `height` with the node's tip at `tip`: the block itself counts.
pub fn confirmations(height: u32, tip: u32) -> u32 {
    tip.saturating_sub(height)
        .saturating_add(1)
        .min(if height > tip { 0 } else { u32::MAX })
}

/// Zebra's finality: a block 1,000 deep is final; above that it is provisional, `deep` blocks
/// down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finality {
    Final,
    Provisional { deep: u32 },
}

pub fn finality(height: u32, tip: u32) -> Finality {
    let deep = tip.saturating_sub(height);
    if deep >= FINAL_DEPTH {
        Finality::Final
    } else {
        Finality::Provisional { deep }
    }
}

pub fn finality_text(finality: Finality) -> String {
    match finality {
        Finality::Final => "Final in Zebra".to_owned(),
        Finality::Provisional { deep } => format!("Provisional · {deep} of 1,000 deep"),
    }
}

/// Where a confirmation count sits on the meter's log scale from 1 to 1,001, from 0 to 1.
pub fn confirmations_fraction(confirmations: u32) -> f64 {
    if confirmations == 0 {
        return 0.0;
    }
    (f64::from(confirmations).log10() / 1_001f64.log10()).min(1.0)
}

/// `about 2 h 5 min`, `about 4 d 3 h`, `under a minute`.
pub fn eta_text(secs: u64) -> String {
    if secs < 60 {
        return "under a minute".to_owned();
    }
    format!(
        "about {}",
        crate::charts::uptime_text(std::time::Duration::from_secs(secs))
    )
}

/// The network difficulty: whole with separators from a thousand up, two decimals below.
pub fn difficulty(difficulty: f64) -> String {
    if !difficulty.is_finite() || difficulty < 0.0 {
        "unknown".to_owned()
    } else if difficulty >= 1000.0 {
        thousands(difficulty.round() as u64)
    } else {
        format!("{difficulty:.2}")
    }
}

/// The seconds between two blocks: `1 min 15 s`, `-5 s` (miners set the times).
pub fn interval_text(secs: i64) -> String {
    let sign = if secs < 0 { "-" } else { "" };
    let magnitude = secs.unsigned_abs();
    match magnitude {
        0..60 => format!("{sign}{magnitude} s"),
        _ => format!("{sign}{} min {} s", magnitude / 60, magnitude % 60),
    }
}

/// The banner's words for a reorganisation the explorer noticed at `at`: how many blocks were
/// replaced, or at least how many when its checks could not tell where the chains part, and
/// above which height.
pub fn reorg_text(reorg: &ReorgNotice, at: &str) -> String {
    let blocks = match reorg.replaced {
        1 => "1 block was".to_owned(),
        n => format!("{} blocks were", thousands(u64::from(n))),
    };
    let shared = thousands(u64::from(reorg.fork_height));
    if reorg.first_replaced <= reorg.fork_height.saturating_add(1) {
        format!(
            "{} replaced at {at}: the node switched to another branch above {shared}.",
            capitalise(&blocks)
        )
    } else {
        format!(
            "At least {blocks} replaced at {at}: the node switched to another branch above a \
             height between {shared} and {}.",
            thousands(u64::from(reorg.first_replaced - 1))
        )
    }
}

/// What the banner says while the node is not running, in its phase's words; `None` while it
/// runs.
pub fn not_running_text(phase: &Phase) -> Option<&'static str> {
    Some(match phase {
        Phase::Running => return None,
        Phase::Stopped => "The node is not running, so the Explorer cannot read its chain.",
        Phase::Starting => "The node is starting; the Explorer reads its chain once it runs.",
        Phase::Stopping => "The node is stopping, so the Explorer cannot read its chain.",
        Phase::Backoff { .. } => {
            "The node stopped and restarts shortly; the Explorer reads its chain once it runs."
        }
        Phase::Failed { .. } => "The node failed, so the Explorer cannot read its chain.",
    })
}

/// How times read: in the local zone, in UTC, or as Unix seconds. The source line's toggle
/// picks it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeFormat {
    #[default]
    Local,
    Utc,
    Unix,
}

impl TimeFormat {
    pub const ALL: [TimeFormat; 3] = [TimeFormat::Local, TimeFormat::Utc, TimeFormat::Unix];

    /// The toggle's label.
    pub fn label(self) -> &'static str {
        match self {
            TimeFormat::Local => "Local",
            TimeFormat::Utc => "UTC",
            TimeFormat::Unix => "Unix",
        }
    }

    /// The name it is stored under.
    pub fn id(self) -> &'static str {
        match self {
            TimeFormat::Local => "local",
            TimeFormat::Utc => "utc",
            TimeFormat::Unix => "unix",
        }
    }

    pub fn from_id(id: &str) -> Option<TimeFormat> {
        TimeFormat::ALL.into_iter().find(|format| format.id() == id)
    }

    /// The format `by` places on in [`TimeFormat::ALL`], wrapping around: the toggle's arrow
    /// keys.
    pub fn step(self, by: i32) -> TimeFormat {
        let index = TimeFormat::ALL
            .iter()
            .position(|format| *format == self)
            .unwrap_or(0) as i32;
        let len = TimeFormat::ALL.len() as i32;
        TimeFormat::ALL[(index + by).rem_euclid(len) as usize]
    }
}

/// A time `secs` after the Unix epoch, in `format`: `23 Oct, 21:13`, `23 Oct, 21:13 UTC`,
/// `1792786380`.
pub fn time_text(secs: u64, format: TimeFormat) -> String {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    match format {
        TimeFormat::Local => local_datetime_short(time),
        TimeFormat::Utc => format!("{} UTC", datetime_short(time, 0)),
        TimeFormat::Unix => secs.to_string(),
    }
}

/// The banner's words while zebrad still runs on `running`, though its node is now on `ours`.
pub fn other_network_text(running: Network, ours: Network) -> String {
    format!(
        "zebrad still runs on {running}, and moves to {ours} at its next start. The Explorer \
         reads nothing until then; restart the node to explore {ours}."
    )
}

/// The hint under the search box, as the query is typed.
pub fn hint(classified: &Classified, tip: Option<u32>, network: Network) -> String {
    match classified {
        Classified::Empty => "A height, a block hash or a transaction id".to_owned(),
        Classified::Secret(_) => "A key or a recovery phrase · Zeal never takes keys".to_owned(),
        Classified::Height(height) => match tip {
            Some(tip) if *height > tip => format!(
                "Height · past your node's tip at {}",
                thousands(u64::from(tip))
            ),
            Some(tip) => format!("Height · your node is at {}", thousands(u64::from(tip))),
            None => "Height".to_owned(),
        },
        Classified::BelowTip(0) => "The latest block".to_owned(),
        Classified::BelowTip(back) => {
            let blocks = match back {
                1 => "1 block".to_owned(),
                n => format!("{} blocks", thousands(u64::from(*n))),
            };
            match tip.and_then(|tip| tip.checked_sub(*back)) {
                Some(height) => format!(
                    "{blocks} below the tip · block {}",
                    thousands(u64::from(height))
                ),
                None => format!("{blocks} below the tip"),
            }
        }
        Classified::Hash(_) => "A block hash or a transaction id".to_owned(),
        Classified::Outpoint(_) => "A transaction output".to_owned(),
        Classified::Route(_) => "A page of the Explorer".to_owned(),
        Classified::Later(later) if later.other_network() => format!(
            "{} of the other network; this node is on {network}",
            capitalise(later.label())
        ),
        Classified::Later(Later::HashPrefix) => {
            "The start of a hash · type all 64 digits to look it up".to_owned()
        }
        Classified::Later(later) => format!(
            "{} · the Explorer looks these up in a later version",
            capitalise(later.label())
        ),
        Classified::Unrecognised => "Not a height, a block hash or a transaction id".to_owned(),
    }
}

/// What the search box says after refusing a secret. The field is cleared and nothing is kept.
pub fn refusal(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::ViewingKey => {
            "This looks like a viewing key. Zeal never needs it, and it was not saved."
        }
        SecretKind::SpendingKey => {
            "This looks like a spending key. Zeal never needs it, and it was not saved. Anyone \
             who has a spending key can spend your funds."
        }
        SecretKind::RecoveryPhrase => {
            "This looks like a recovery phrase. Zeal never needs it, and it was not saved. \
             Anyone who has a recovery phrase can spend your funds."
        }
    }
}

/// Why a search found nothing.
pub fn not_found_text(hint: &NotFoundHint) -> String {
    match hint {
        NotFoundHint::PastNodeTip { node_height } => format!(
            "Your node has blocks up to {}.",
            thousands(u64::from(*node_height))
        ),
        NotFoundHint::UnknownHash => "Not a block in your node's best chain, nor a transaction \
                                      in its chain or mempool. Your node only knows the \
                                      transactions it has received."
            .to_owned(),
        NotFoundHint::NotSearchable { what } => {
            format!("This looks like {what}. The Explorer looks these up in a later version.")
        }
        NotFoundHint::Refused { what } => refusal(*what).to_owned(),
        NotFoundHint::Unrecognised => "Not a height, a block hash or a transaction id.".to_owned(),
    }
}

/// A row's flow in a glyph, from where value came to where it went: `t` transparent, `z`
/// shielded, `cb` a coinbase's new coins. `t → z·t` is shielding with transparent change;
/// `z → z`, value that stays shielded.
pub fn flow_glyph(row: &TxRow) -> String {
    let lost = row.shielded.iter().any(|part| part.net_out > 0);
    let gained = row.shielded.iter().any(|part| part.net_out < 0);
    let shielded = !row.shielded.is_empty();
    let mut from = Vec::new();
    if matches!(row.kind, TxKind::Coinbase { .. }) {
        from.push("cb");
    } else if row.inputs > 0 {
        from.push("t");
    }
    if lost || (from.is_empty() && shielded) {
        from.push("z");
    }
    let mut to = Vec::new();
    if gained || (row.outputs == 0 && shielded) {
        to.push("z");
    }
    if row.outputs > 0 {
        to.push("t");
    }
    format!("{} → {}", from.join("·"), to.join("·"))
}

/// What a row moved into or out of each shielded pool: `Ironwood +1.2 · Orchard −1.2001`.
/// Value that left a pool is negative; value that stayed in it is "nothing net", never a
/// zero, which would read as an amount the chain hides.
pub fn net_text(shielded: &[PoolPart], unit: Unit) -> String {
    shielded
        .iter()
        .map(|part| {
            let into = -part.net_out;
            if into == 0 {
                return format!("{} nothing net", part.pool.label());
            }
            let sign = if into > 0 { "+" } else { "" };
            format!("{} {sign}{}", part.pool.label(), unit.number(into))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// What a fee pays, by ZIP 317's numbers: per logical action due, and against the
/// conventional fee.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeeFigures {
    pub fee: u64,
    /// The logical actions (zebrad's sum, before the minimum of two).
    pub logical: u32,
    /// The actions a fee is due for: at least two.
    pub paid: u32,
    pub conventional: u64,
    /// Zatoshis per action due.
    pub per_action: u64,
    /// The fee over the conventional fee.
    pub multiple: f64,
    /// zebrad's weight ratio: the multiple, at least a zatoshi counted, capped at 10.
    pub ratio: f32,
    pub unpaid: u32,
}

impl FeeFigures {
    pub fn new(fee: u64, actions: &LogicalActions) -> Self {
        let logical = zip317::logical_actions(actions);
        let paid = zip317::paid_actions(logical);
        let conventional = zip317::conventional_fee(logical);
        FeeFigures {
            fee,
            logical,
            paid,
            conventional,
            per_action: fee / u64::from(paid),
            multiple: zip317::multiple(fee, conventional),
            ratio: zip317::weight_ratio(fee, conventional),
            unpaid: zip317::unpaid_actions(logical, fee),
        }
    }
}

/// A row's public fee, as figures; `None` while unknown, or for a coinbase.
pub fn fee_figures(fee: Option<Shown>, actions: &LogicalActions) -> Option<FeeFigures> {
    match fee {
        Some(Shown::Public { zat }) => Some(FeeFigures::new(u64::try_from(zat).ok()?, actions)),
        _ => None,
    }
}

/// A multiple of the conventional fee: `1×`, `2.5×`, `250×`.
pub fn multiple_text(multiple: f64) -> String {
    if multiple >= 100.0 || (multiple - multiple.round()).abs() < 0.05 {
        format!("{}×", thousands(multiple.round() as u64))
    } else {
        format!("{multiple:.1}×")
    }
}

/// The logical actions part by part, as zebrad adds them up: `max(⌈296 / 150⌉, ⌈68 / 34⌉)
/// = 2 transparent + 2 Ironwood = 4`.
pub fn actions_breakdown(actions: &LogicalActions) -> String {
    let mut parts = Vec::new();
    if actions.in_bytes + actions.out_bytes > 0 {
        parts.push(format!(
            "max(⌈{} / 150⌉, ⌈{} / 34⌉) = {} transparent",
            thousands(u64::from(actions.in_bytes)),
            thousands(u64::from(actions.out_bytes)),
            zip317::transparent_actions(actions)
        ));
    }
    if actions.joinsplits > 0 {
        parts.push(format!("2 × {} JoinSplit", actions.joinsplits));
    }
    if actions.sapling_spends + actions.sapling_outputs > 0 {
        parts.push(format!(
            "{} Sapling (the larger of {} spends and {} outputs)",
            actions.sapling_spends.max(actions.sapling_outputs),
            actions.sapling_spends,
            actions.sapling_outputs
        ));
    }
    if actions.orchard_actions > 0 {
        parts.push(format!("{} Orchard", actions.orchard_actions));
    }
    if actions.ironwood_actions > 0 {
        parts.push(format!("{} Ironwood", actions.ironwood_actions));
    }
    let total = zip317::logical_actions(actions);
    match parts.len() {
        0 => "0".to_owned(),
        1 if parts[0].ends_with(&format!("= {total} transparent")) => {
            format!("{} = {total}", parts[0])
        }
        _ => format!("{} = {total}", parts.join(" + ")),
    }
}

/// What the fee card says of a fee: whether it pays zebrad's conventional fee, for a mined
/// transaction or a waiting one. Never "invalid": blocks may hold what the mempool would not.
pub fn fee_verdict(figures: &FeeFigures, status: &TxStatus) -> String {
    let pays = if figures.unpaid == 0 {
        format!(
            "{} zebrad's conventional fee",
            multiple_text(figures.multiple)
        )
    } else {
        format!(
            "less than zebrad's conventional fee: {} of its {} actions unpaid",
            figures.unpaid, figures.paid
        )
    };
    match status {
        TxStatus::Mempool => format!("Pays {pays}."),
        _ => format!("Paid {pays}."),
    }
}

/// The node of a flow, in words: what it is, without its amount.
pub fn flow_label(key: &FlowKey, page: &TxPage) -> String {
    match key {
        FlowKey::Inputs { address, count } => format!(
            "{} · {} input{}",
            address
                .as_deref()
                .map_or("no address".to_owned(), short_address),
            count,
            if *count == 1 { "" } else { "s" }
        ),
        FlowKey::MoreInputs { count } => format!("{count} more inputs"),
        FlowKey::UnknownInputs { count } => format!(
            "{count} input{}: value not read",
            if *count == 1 { "" } else { "s" }
        ),
        FlowKey::Output { n } => {
            let address = page
                .outputs
                .iter()
                .find(|output| output.n == *n)
                .and_then(|output| output.address.as_deref())
                .map_or("no address".to_owned(), short_address);
            format!("#{n} {address}")
        }
        FlowKey::MoreOutputs { count } => format!("{count} more outputs"),
        FlowKey::Pool { pool } => {
            let counts = page
                .shielded
                .iter()
                .find(|part| part.pool == *pool)
                .map(part_counts)
                .unwrap_or_default();
            format!("{} · {counts}", pool.label())
        }
        FlowKey::Fee => "Fee".to_owned(),
        FlowKey::Subsidy => "Block subsidy".to_owned(),
        FlowKey::BlockFees => "The block's fees".to_owned(),
        FlowKey::FromLockbox => "From the lockbox".to_owned(),
        FlowKey::IntoLockbox => "Into the lockbox".to_owned(),
        FlowKey::Burned => "Burned".to_owned(),
    }
}

/// A pool part's counts: `2 actions`, `1 spend · 2 outputs`, `1 JoinSplit`.
pub fn part_counts(part: &PoolPart) -> String {
    let plural =
        |count: u32, one: &str| format!("{count} {one}{}", if count == 1 { "" } else { "s" });
    match part.pool {
        Pool::Sapling => format!(
            "{} · {}",
            plural(part.spends, "spend"),
            plural(part.outputs, "output")
        ),
        Pool::Sprout => plural(part.joinsplits, "JoinSplit"),
        _ => plural(part.actions, "action"),
    }
}

/// An address cut to its ends, for a label: `t1Ab3…x9Qz`.
pub fn short_address(address: &str) -> String {
    let chars: Vec<char> = address.chars().collect();
    if chars.len() <= 14 {
        return address.to_owned();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// One side's part of the conservation line, summed by kind: `1.2 from transparent inputs`,
/// `1.5 into Orchard`, `0.0001 fee`.
fn side_parts(flow: &Flow, left: bool, unit: Unit) -> Vec<String> {
    let nodes = if left { &flow.left } else { &flow.right };
    let mut transparent = 0u64;
    let mut parts = Vec::new();
    for node in nodes {
        let Some(zat) = node.zat else { continue };
        let amount = unit.number(zat as i64);
        match &node.key {
            FlowKey::Inputs { .. } | FlowKey::MoreInputs { .. } => transparent += zat,
            FlowKey::Output { .. } | FlowKey::MoreOutputs { .. } => transparent += zat,
            FlowKey::Pool { pool } if zat > 0 => parts.push(if left {
                format!("{amount} out of {}", pool.label())
            } else {
                format!("{amount} into {}", pool.label())
            }),
            FlowKey::Pool { .. } | FlowKey::UnknownInputs { .. } => {}
            FlowKey::Fee => parts.push(format!("{amount} fee")),
            FlowKey::Subsidy => parts.push(format!("{amount} subsidy")),
            FlowKey::BlockFees => parts.push(format!("{amount} in fees")),
            FlowKey::FromLockbox => parts.push(format!("{amount} from the lockbox")),
            FlowKey::IntoLockbox => parts.push(format!("{amount} into the lockbox")),
            FlowKey::Burned => parts.push(format!("{amount} burned")),
        }
    }
    if transparent > 0 {
        let amount = unit.number(transparent as i64);
        parts.insert(
            0,
            if left {
                format!("{amount} from transparent inputs")
            } else {
                format!("{amount} to transparent outputs")
            },
        );
    }
    parts
}

/// The line under V6: what went in and what came out, and whether they balance: `1.5001 ZEC
/// in = 1.5 into Orchard + 0.0001 fee ✓`.
pub fn conservation_text(flow: &Flow, unit: Unit) -> String {
    match flow.balance {
        Balance::Balanced { total } => {
            let right = side_parts(flow, false, unit);
            let left = side_parts(flow, true, unit);
            let left = if left.len() > 1 {
                left.join(" + ")
            } else {
                format!("{} in", amount(total as i64, unit))
            };
            format!("{left} = {} ✓", right.join(" + "))
        }
        Balance::Unknown => {
            let unread = flow.left.iter().find_map(|node| match node.key {
                FlowKey::UnknownInputs { count } => Some(count),
                _ => None,
            });
            match unread {
                Some(1) => "1 input's value is not read, so the balance is unknown".to_owned(),
                Some(count) => {
                    format!("{count} inputs' values are not read, so the balance is unknown")
                }
                None => "zebrad has no value-pool row for this block or its parent, so its fees \
                         and the balance are unknown"
                    .to_owned(),
            }
        }
        Balance::Off { left, right } if left > right => format!(
            "{} in, {} out: the miner left {} unclaimed",
            amount(left as i64, unit),
            amount(right as i64, unit),
            amount((left - right) as i64, unit)
        ),
        Balance::Off { left, right } => format!(
            "Unverified: {} in, {} out",
            amount(left as i64, unit),
            amount(right as i64, unit)
        ),
    }
}

/// How long since an output was made, in blocks: `1,204 blocks`, `in this block`, `waiting`.
pub fn coin_age(spent_at: Option<u32>, status: &TxStatus) -> String {
    match (spent_at, status) {
        (None, _) => "waiting".to_owned(),
        (Some(made), TxStatus::Mined { height, .. }) => match height.saturating_sub(made) {
            0 => "in this block".to_owned(),
            1 => "1 block".to_owned(),
            blocks => format!("{} blocks", thousands(u64::from(blocks))),
        },
        (Some(_), _) => "–".to_owned(),
    }
}

/// A read for Show RPC: `getblock ["3549000",1]`.
pub fn source_text(call: &SourceCall) -> String {
    format!("{} {}", call.method, call.params)
}

/// Where a read's answer came from and what it took: `12 ms · 3.8 kB`, `from memory`, `12 ms
/// · not found`, `12 ms · error`, `10,000 ms · no answer`.
pub fn source_cost(call: &SourceCall) -> String {
    let ended = match call.outcome {
        ReadOutcome::Answered => None,
        ReadOutcome::NotFound => Some("not found"),
        ReadOutcome::Error => Some("error"),
        ReadOutcome::NoAnswer => Some("no answer"),
    };
    if let Some(ended) = ended {
        return format!("{} ms · {ended}", thousands(u64::from(call.ms)));
    }
    if call.cached {
        return "from memory".to_owned();
    }
    match call.bytes {
        Some(size) => format!(
            "{} ms · {}",
            thousands(u64::from(call.ms)),
            bytes(u64::from(size))
        ),
        None => format!("{} ms", thousands(u64::from(call.ms))),
    }
}

#[cfg(test)]
mod tests {
    use explorer_model::classify::classify_query;
    use launcher_types::explorer::{BlockLedger, Hash32};

    use super::*;

    const ZEC: Unit = Unit {
        coin: "ZEC",
        zat: false,
    };
    const TAZ: Unit = Unit {
        coin: "TAZ",
        zat: false,
    };
    const ZAT: Unit = Unit {
        coin: "ZEC",
        zat: true,
    };

    #[test]
    fn amounts_are_exact_and_say_their_unit() {
        assert_eq!(coins(156_250_000), "1.5625");
        assert_eq!(coins(100_070), "0.0010007");
        assert_eq!(coins(200_000_000), "2");
        assert_eq!(coins(-50_000_000), "-0.5");
        assert_eq!(coins(0), "0");
        assert_eq!(coins(1_465_499_576_514_816), "14,654,995.76514816");
        assert_eq!(amount(1, TAZ), "0.00000001 TAZ");
        assert_eq!(unit(Network::Testnet), "TAZ");
        assert_eq!(unit(Network::Mainnet), "ZEC");
        assert_eq!(
            amount_rounded(1_465_499_576_514_816, ZEC),
            "14,654,995.77 ZEC"
        );
        assert_eq!(amount_rounded(12_345_678, ZEC), "0.12345678 ZEC");
    }

    #[test]
    fn amounts_in_zatoshis_and_times_in_each_format() {
        let zat = Unit::new(Network::Testnet, true);
        assert_eq!(zat.to_string(), "zat");
        assert_eq!(amount(156_250_000, zat), "156,250,000 zat");
        assert_eq!(zat.number(-1_000), "-1,000");
        assert_eq!(
            amount_rounded(1_465_499_576_514_816, zat),
            "1,465,499,576,514,816 zat",
            "zatoshis are never rounded"
        );
        assert_eq!(Unit::new(Network::Testnet, false).to_string(), "TAZ");
        // 2026-10-23 21:13 UTC.
        let secs = 1_792_789_980;
        assert_eq!(time_text(secs, TimeFormat::Utc), "23 Oct, 21:13 UTC");
        assert_eq!(time_text(secs, TimeFormat::Unix), "1792789980");
        for format in TimeFormat::ALL {
            assert_eq!(TimeFormat::from_id(format.id()), Some(format));
        }
        assert_eq!(TimeFormat::from_id("julian"), None);
        assert_eq!(TimeFormat::Local.step(1), TimeFormat::Utc);
        assert_eq!(TimeFormat::Unix.step(1), TimeFormat::Local, "wraps around");
        assert_eq!(TimeFormat::Local.step(-1), TimeFormat::Unix);
    }

    #[test]
    fn movements_read_as_value_entering_or_leaving() {
        assert_eq!(
            movement(Pool::Ironwood, 120_000_000, ZEC),
            "1.2 ZEC entered the Ironwood pool"
        );
        assert_eq!(
            movement(Pool::Orchard, -50_000_000, ZEC),
            "0.5 ZEC left the Orchard pool"
        );
        assert_eq!(
            movement(Pool::Sapling, 0, ZEC),
            "No change in the Sapling pool"
        );
    }

    fn summary(users: Option<[i64; 6]>, fees: Option<u64>, burned: Option<u64>) -> BlockSummary {
        BlockSummary {
            height: 3_550_000,
            hash: Hash32([0; 32]),
            time: 0,
            interval: Some(30),
            size: Some(70_000),
            tx_count: 37,
            ledger: BlockLedger {
                pools: None,
                deltas: None,
                coinbase: None,
                users: users.map(PerPool),
                fees,
                burned,
                notes: Some([10, 2, 200]),
                budget: Budget::Units {
                    used: 212,
                    limit: 330,
                    complete: false,
                },
            },
        }
    }

    #[test]
    fn a_block_in_a_sentence() {
        let block = summary(
            Some([-1_350_000_000, 0, 340_000_000, 0, 0, 1_000_000_000]),
            Some(210_000),
            Some(126_000),
        );
        assert_eq!(
            block_sentence(&block, ZEC),
            "37 transactions · 212 note commitments · users moved 13.4 ZEC into shielded pools · \
             fees 0.0021 ZEC (0.00126 burned)"
        );
        let unknown = summary(None, None, None);
        assert_eq!(
            block_sentence(&unknown, TAZ),
            "37 transactions · 212 note commitments · fees unknown"
        );
        assert_eq!(budget_text(&block.ledger.budget), "≥ 212 of 330");
        assert_eq!(
            budget_text(&Budget::Bytes {
                used: 70_553,
                limit: 2_000_000
            }),
            "70.6 kB of 2.0 MB"
        );
        assert_eq!(budget_fraction(&Budget::Unknown), None);
    }

    fn row(kind: TxKind, inputs: u32, outputs: u32, nets: &[(Pool, i64)]) -> TxRow {
        TxRow {
            index: 1,
            txid: Hash32([1; 32]),
            kind,
            version: 5,
            size: Some(2_000),
            inputs,
            outputs,
            shielded: nets
                .iter()
                .map(|(pool, net_out)| PoolPart {
                    pool: *pool,
                    spends: 0,
                    outputs: 0,
                    actions: 2,
                    joinsplits: 0,
                    net_out: *net_out,
                })
                .collect(),
            actions: LogicalActions::default(),
            fee: None,
        }
    }

    #[test]
    fn rows_say_where_value_went() {
        let shielding = row(
            TxKind::Shielding {
                into: vec![Pool::Orchard],
                transparent_change: true,
            },
            2,
            1,
            &[(Pool::Orchard, -150_000_000)],
        );
        assert_eq!(flow_glyph(&shielding), "t → z·t");
        assert_eq!(net_text(&shielding.shielded, ZEC), "Orchard +1.5");
        let shielded = row(
            TxKind::Shielded {
                pool: Pool::Orchard,
                same_receiver: true,
            },
            0,
            0,
            &[(Pool::Orchard, 10_000)],
        );
        assert_eq!(flow_glyph(&shielded), "z → z");
        let migration = row(
            TxKind::Migration {
                from: Pool::Orchard,
                to: Pool::Ironwood,
                zat: 100_000_000,
            },
            0,
            0,
            &[(Pool::Orchard, 100_020_000), (Pool::Ironwood, -100_000_000)],
        );
        assert_eq!(flow_glyph(&migration), "z → z");
        assert_eq!(
            net_text(&migration.shielded, ZEC),
            "Orchard -1.0002 · Ironwood +1"
        );
        let deshielding = row(
            TxKind::Deshielding {
                from: vec![Pool::Sapling],
            },
            0,
            2,
            &[(Pool::Sapling, 5)],
        );
        assert_eq!(flow_glyph(&deshielding), "z → t");
        // Value that stayed in a pool is nothing net there, never a zero.
        let paying_from_orchard = row(
            TxKind::Mixed,
            0,
            0,
            &[(Pool::Sapling, 0), (Pool::Orchard, 10_000)],
        );
        assert_eq!(
            net_text(&paying_from_orchard.shielded, ZEC),
            "Sapling nothing net · Orchard -0.0001"
        );
        assert_eq!(
            net_text(&paying_from_orchard.shielded, ZAT),
            "Sapling nothing net · Orchard -10,000"
        );
        assert_eq!(flow_glyph(&row(TxKind::Transparent, 1, 2, &[])), "t → t");
        let coinbase = row(
            TxKind::Coinbase {
                shielded_to: vec![Pool::Ironwood],
            },
            1,
            3,
            &[(Pool::Ironwood, -20)],
        );
        assert_eq!(flow_glyph(&coinbase), "cb → z·t");
    }

    #[test]
    fn the_fee_card_counts_as_zebrad_does() {
        // Two standard inputs and one output into Ironwood with two actions: 4 actions.
        let actions = LogicalActions {
            in_bytes: 296,
            out_bytes: 34,
            ironwood_actions: 2,
            ..LogicalActions::default()
        };
        let figures = FeeFigures::new(10_000, &actions);
        assert_eq!((figures.logical, figures.paid), (4, 4));
        assert_eq!(figures.conventional, 4_000);
        assert_eq!(figures.per_action, 2_500);
        assert_eq!(multiple_text(figures.multiple), "2.5×");
        assert_eq!(figures.ratio, 2.5);
        assert_eq!(
            actions_breakdown(&actions),
            "max(⌈296 / 150⌉, ⌈34 / 34⌉) = 2 transparent + 2 Ironwood = 4"
        );
        assert_eq!(
            fee_verdict(&figures, &TxStatus::Mempool),
            "Pays 2.5× zebrad's conventional fee."
        );
        // A mined one that paid less is said to, never called invalid.
        let short = FeeFigures::new(1_000, &actions);
        assert_eq!(short.unpaid, 3);
        assert_eq!(
            fee_verdict(
                &short,
                &TxStatus::Mined {
                    block: Hash32([2; 32]),
                    height: 1,
                    time: None
                }
            ),
            "Paid less than zebrad's conventional fee: 3 of its 4 actions unpaid."
        );
        assert_eq!(multiple_text(1.0), "1×");
        assert_eq!(multiple_text(250.0), "250×");
        assert_eq!(
            fee_figures(
                Some(Shown::Unknown {
                    why: UnknownWhy::InputValues
                }),
                &actions
            ),
            None
        );
        // Grace actions: a tiny transaction pays for two.
        let tiny = LogicalActions {
            in_bytes: 148,
            out_bytes: 34,
            ..LogicalActions::default()
        };
        assert_eq!(FeeFigures::new(2_000, &tiny).paid, 2);
        assert_eq!(
            actions_breakdown(&tiny),
            "max(⌈148 / 150⌉, ⌈34 / 34⌉) = 1 transparent = 1"
        );
    }

    #[test]
    fn the_flow_line_says_what_balances() {
        use explorer_model::flow::FlowNode;
        let node = |key, zat| FlowNode { key, zat };
        let shielding = Flow {
            left: vec![node(
                FlowKey::Inputs {
                    address: Some("t1abc".into()),
                    count: 2,
                },
                Some(150_010_000),
            )],
            right: vec![
                node(
                    FlowKey::Pool {
                        pool: Pool::Orchard,
                    },
                    Some(150_000_000),
                ),
                node(FlowKey::Fee, Some(10_000)),
            ],
            balance: Balance::Balanced { total: 150_010_000 },
        };
        assert_eq!(
            conservation_text(&shielding, ZEC),
            "1.5001 ZEC in = 1.5 into Orchard + 0.0001 fee ✓"
        );
        let coinbase = Flow {
            left: vec![
                node(FlowKey::Subsidy, Some(300)),
                node(FlowKey::BlockFees, Some(100)),
            ],
            right: vec![
                node(FlowKey::Output { n: 0 }, Some(340)),
                node(FlowKey::Burned, Some(60)),
            ],
            balance: Balance::Balanced { total: 400 },
        };
        assert_eq!(
            conservation_text(&coinbase, ZAT),
            "300 subsidy + 100 in fees = 340 to transparent outputs + 60 burned ✓"
        );
        let unread = Flow {
            left: vec![node(FlowKey::UnknownInputs { count: 1 }, None)],
            right: vec![node(FlowKey::Fee, None)],
            balance: Balance::Unknown,
        };
        assert_eq!(
            conservation_text(&unread, ZEC),
            "1 input's value is not read, so the balance is unknown"
        );
        let unclaimed = Flow {
            balance: Balance::Off {
                left: 400,
                right: 390,
            },
            ..coinbase
        };
        assert_eq!(
            conservation_text(&unclaimed, ZAT),
            "400 zat in, 390 zat out: the miner left 10 zat unclaimed"
        );
        assert_eq!(
            short_address("t1abcdefghijklmnopqrstuvwxyz12345678"),
            "t1abcd…5678"
        );
        assert_eq!(short_address("t1abc"), "t1abc");
    }

    #[test]
    fn show_rpc_says_where_each_answer_came_from() {
        let call = SourceCall {
            method: "getblock".into(),
            params: r#"["3549000",1]"#.into(),
            ms: 12,
            bytes: Some(3_800),
            cached: false,
            outcome: ReadOutcome::Answered,
        };
        assert_eq!(source_text(&call), r#"getblock ["3549000",1]"#);
        assert_eq!(source_cost(&call), "12 ms · 3.8 kB");
        let cached = SourceCall {
            cached: true,
            ms: 0,
            bytes: None,
            ..call.clone()
        };
        assert_eq!(source_cost(&cached), "from memory");
        let ended = |outcome| SourceCall {
            outcome,
            bytes: None,
            ..call.clone()
        };
        assert_eq!(
            source_cost(&ended(ReadOutcome::NotFound)),
            "12 ms · not found"
        );
        assert_eq!(source_cost(&ended(ReadOutcome::Error)), "12 ms · error");
        assert_eq!(
            source_cost(&ended(ReadOutcome::NoAnswer)),
            "12 ms · no answer"
        );
        let mined = TxStatus::Mined {
            block: Hash32([2; 32]),
            height: 1_204,
            time: None,
        };
        assert_eq!(coin_age(Some(1_000), &mined), "204 blocks");
        assert_eq!(coin_age(Some(1_204), &mined), "in this block");
        assert_eq!(coin_age(None, &TxStatus::Mempool), "waiting");
    }

    #[test]
    fn transactions_in_a_sentence_never_show_hidden_amounts() {
        let page = TxPage {
            txid: Hash32([1; 32]),
            status: TxStatus::Mempool,
            kind: TxKind::Migration {
                from: Pool::Orchard,
                to: Pool::Ironwood,
                zat: 10_000_000_000,
            },
            version: 6,
            size: Some(9_000),
            lock_time: 0,
            expiry_height: None,
            inputs: Vec::new(),
            outputs: Vec::new(),
            shielded: vec![
                PoolPart {
                    pool: Pool::Orchard,
                    spends: 0,
                    outputs: 0,
                    actions: 2,
                    joinsplits: 0,
                    net_out: 10_000_004_000,
                },
                PoolPart {
                    pool: Pool::Ironwood,
                    spends: 0,
                    outputs: 0,
                    actions: 2,
                    joinsplits: 0,
                    net_out: -10_000_000_000,
                },
            ],
            fee: Some(Shown::Public { zat: 4_000 }),
            actions: LogicalActions {
                orchard_actions: 2,
                ironwood_actions: 2,
                ..LogicalActions::default()
            },
            unresolved: 0,
            resolvable: false,
            burns_fees: false,
            coinbase: None,
        };
        assert_eq!(
            tx_sentence(&page, ZEC),
            "Moved 100 ZEC from Orchard to Ironwood. 2 Orchard actions and 2 Ironwood actions; \
             amounts per note hidden. Fee 0.00004 ZEC."
        );
        assert_eq!(kind_label(&page.kind), "Orchard → Ironwood");
        let unknown_fee = TxPage {
            fee: Some(Shown::Unknown {
                why: UnknownWhy::InputValues,
            }),
            kind: TxKind::Shielding {
                into: vec![Pool::Sapling, Pool::Orchard],
                transparent_change: true,
            },
            ..page
        };
        assert!(tx_sentence(&unknown_fee, ZEC).starts_with(
            "Moved transparent value into Sapling and Orchard, with transparent change."
        ));
        // A coinbase's shielded outputs are public by consensus: never "hidden".
        let coinbase = TxPage {
            kind: TxKind::Coinbase {
                shielded_to: vec![Pool::Sapling],
            },
            shielded: vec![PoolPart {
                pool: Pool::Sapling,
                spends: 0,
                outputs: 2,
                actions: 0,
                joinsplits: 0,
                net_out: -250_000_000,
            }],
            fee: None,
            ..unknown_fee
        };
        let sentence = tx_sentence(&coinbase, ZEC);
        assert_eq!(
            sentence,
            "The block's coinbase, paying into Sapling. 0 Sapling spends and 2 outputs; amounts \
             per note are public by consensus, but the Explorer does not decrypt notes."
        );
        assert!(!sentence.contains("hidden"));
    }

    #[test]
    fn confirmations_and_finality() {
        assert_eq!(confirmations(100, 100), 1);
        assert_eq!(confirmations(90, 100), 11);
        assert_eq!(confirmations(101, 100), 0);
        assert_eq!(finality(100, 1_099), Finality::Provisional { deep: 999 });
        assert_eq!(finality(100, 1_100), Finality::Final);
        assert_eq!(
            finality_text(Finality::Provisional { deep: 3 }),
            "Provisional · 3 of 1,000 deep"
        );
        assert_eq!(confirmations_fraction(0), 0.0);
        assert_eq!(confirmations_fraction(1), 0.0);
        assert_eq!(confirmations_fraction(1_001), 1.0);
        assert!(confirmations_fraction(10) > 0.3 && confirmations_fraction(10) < 0.35);
    }

    #[test]
    fn difficulty_reads_whole_or_with_two_decimals() {
        assert_eq!(difficulty(118_462_377.5), "118,462,378");
        assert_eq!(difficulty(42.123), "42.12");
        assert_eq!(difficulty(f64::NAN), "unknown");
    }

    #[test]
    fn times_and_intervals() {
        assert_eq!(eta_text(30), "under a minute");
        assert_eq!(eta_text(7_500), "about 2 h 5 min");
        assert_eq!(interval_text(75), "1 min 15 s");
        assert_eq!(interval_text(-5), "-5 s");
    }

    #[test]
    fn reorganisations_say_only_what_the_checks_showed() {
        let exact = ReorgNotice {
            fork_height: 3_550_998,
            first_replaced: 3_550_999,
            replaced: 2,
            epoch: 1,
            at: 0,
        };
        assert_eq!(
            reorg_text(&exact, "14:02"),
            "2 blocks were replaced at 14:02: the node switched to another branch above 3,550,998."
        );
        let bounded = ReorgNotice {
            fork_height: 3_550_000,
            first_replaced: 3_551_000,
            replaced: 1,
            ..exact
        };
        assert_eq!(
            reorg_text(&bounded, "14:02"),
            "At least 1 block was replaced at 14:02: the node switched to another branch above a \
             height between 3,550,000 and 3,550,999."
        );
    }

    #[test]
    fn a_zebrad_on_the_old_network_is_named() {
        assert_eq!(
            other_network_text(Network::Mainnet, Network::Testnet),
            "zebrad still runs on Mainnet, and moves to Testnet at its next start. The Explorer \
             reads nothing until then; restart the node to explore Testnet."
        );
    }

    #[test]
    fn the_banner_follows_the_phase() {
        assert_eq!(not_running_text(&Phase::Running), None);
        assert!(
            not_running_text(&Phase::Starting)
                .unwrap()
                .contains("is starting")
        );
        assert!(
            not_running_text(&Phase::Stopped)
                .unwrap()
                .contains("not running")
        );
    }

    #[test]
    fn hints_and_refusals() {
        let net = Network::Mainnet;
        assert_eq!(
            hint(&Classified::Height(3_601_234), Some(3_601_234), net),
            "Height · your node is at 3,601,234"
        );
        assert_eq!(
            hint(&Classified::Height(4_000_000), Some(3_601_234), net),
            "Height · past your node's tip at 3,601,234"
        );
        assert_eq!(
            hint(
                &Classified::Later(Later::Transparent {
                    other_network: true
                }),
                None,
                net
            ),
            "A transparent address of the other network; this node is on Mainnet"
        );
        // `tip-1` and `-2` both name the block under the tip.
        for query in ["tip-1", "-2"] {
            assert_eq!(
                hint(&classify_query(query, net), Some(3_601_234), net),
                "1 block below the tip · block 3,601,233",
                "{query}"
            );
        }
        assert_eq!(
            hint(&classify_query("tip-1500", net), None, net),
            "1,500 blocks below the tip"
        );
        assert!(hint(&Classified::Secret(SecretKind::ViewingKey), None, net).contains("never"));
        assert!(refusal(SecretKind::RecoveryPhrase).contains("not saved"));
        assert_eq!(
            not_found_text(&NotFoundHint::PastNodeTip {
                node_height: 2_951_020
            }),
            "Your node has blocks up to 2,951,020."
        );
    }
}
