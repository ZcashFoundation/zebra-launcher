//! What a search query is: pure, table-tested, and run on every keystroke by the UI and again
//! by the backend. The first match wins, refused secrets first (docs/EXPLORER.md,
//! Classification). This phase looks up heights, block hashes and transaction ids; the other
//! classes are recognised, so the hint can say what they are, and looked up in later phases.
//!
//! The work is linear in the query, whatever it holds: a pasted link is unwrapped one level,
//! a route parsed once, and a search route's query read one level down. A key anywhere in a
//! link or a route is still refused, by a scan of its parts and of the runs of characters keys
//! are written in.
//!
//! Address and key prefixes are zcash_protocol 0.11.0's and, for ZIP 316 Revision 2,
//! zcash_address 0.14.0's. zebrad checks checksums; this only looks at the shape.

use std::borrow::Cow;

use launcher_types::{
    Network,
    explorer::{ExplorerRoute, Hash32, OutPoint, SecretKind, percent_decode},
};

/// What a query is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classified {
    /// Nothing typed.
    Empty,
    /// A key or a recovery phrase: refused, never stored, logged or sent.
    Secret(SecretKind),
    /// A block height.
    Height(u32),
    /// `tip`, `latest`, `tip-N` or `-N`: this many blocks below the tip (`-1` is the tip, as
    /// in `getblockhash`).
    BelowTip(u32),
    /// 64 hex digits: a block hash or a transaction id.
    Hash(Hash32),
    /// A transaction output, `<txid>:<n>`.
    Outpoint(OutPoint),
    /// One of the Explorer's own routes, such as `block/3120000`.
    Route(ExplorerRoute),
    /// Recognised, but not looked up in this phase.
    Later(Later),
    /// None of the above.
    Unrecognised,
}

/// A class the search recognises and looks up in a later phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Later {
    /// Ten digits: a Unix time.
    UnixTime,
    /// A date, such as `2026-10-01`.
    Date,
    /// The start of a hash: suggestions come from what Zeal has seen, never from the node.
    HashPrefix,
    /// A transparent address; `other_network` when its prefix is the other network's.
    Transparent {
        other_network: bool,
    },
    /// A TEX address (ZIP 320).
    Tex {
        other_network: bool,
    },
    Sapling {
        other_network: bool,
    },
    Unified {
        other_network: bool,
    },
    Sprout {
        other_network: bool,
    },
    /// A `zcash:` payment request (ZIP 321).
    PaymentRequest,
    /// A value pool's name.
    Pool,
    /// A network upgrade's name.
    Upgrade,
    /// A ZIP number.
    Zip,
    /// A `>command`.
    Command,
}

impl Later {
    /// What it is, for a sentence: "This looks like {label}."
    pub fn label(self) -> &'static str {
        match self {
            Later::UnixTime => "a Unix time",
            Later::Date => "a date",
            Later::HashPrefix => "the start of a hash",
            Later::Transparent { .. } => "a transparent address",
            Later::Tex { .. } => "a TEX address",
            Later::Sapling { .. } => "a Sapling address",
            Later::Unified { .. } => "a unified address",
            Later::Sprout { .. } => "a Sprout address",
            Later::PaymentRequest => "a payment request",
            Later::Pool => "a value pool's name",
            Later::Upgrade => "a network upgrade's name",
            Later::Zip => "a ZIP number",
            Later::Command => "a command",
        }
    }

    /// Whether it belongs to the other network than the node's.
    pub fn other_network(self) -> bool {
        matches!(
            self,
            Later::Transparent {
                other_network: true
            } | Later::Tex {
                other_network: true
            } | Later::Sapling {
                other_network: true
            } | Later::Unified {
                other_network: true
            } | Later::Sprout {
                other_network: true
            }
        )
    }
}

/// Viewing and spending key prefixes, from zcash_protocol's and zcash_address's constants.
const KEY_PREFIXES: [(&str, SecretKind); 18] = [
    ("zxviews1", SecretKind::ViewingKey),
    ("zxviewtestsapling1", SecretKind::ViewingKey),
    ("zxviewregtestsapling1", SecretKind::ViewingKey),
    ("uview1", SecretKind::ViewingKey),
    ("uviewtest1", SecretKind::ViewingKey),
    ("uviewregtest1", SecretKind::ViewingKey),
    ("uivk1", SecretKind::ViewingKey),
    ("uivktest1", SecretKind::ViewingKey),
    ("uivkregtest1", SecretKind::ViewingKey),
    ("uvf1", SecretKind::ViewingKey),
    ("uvftest1", SecretKind::ViewingKey),
    ("uvfregtest1", SecretKind::ViewingKey),
    ("uvi1", SecretKind::ViewingKey),
    ("uvitest1", SecretKind::ViewingKey),
    ("uviregtest1", SecretKind::ViewingKey),
    ("secret-extended-key-main1", SecretKind::SpendingKey),
    ("secret-extended-key-test1", SecretKind::SpendingKey),
    ("secret-extended-key-regtest1", SecretKind::SpendingKey),
];

const POOLS: [&str; 6] = [
    "transparent",
    "sprout",
    "sapling",
    "orchard",
    "ironwood",
    "lockbox",
];

const UPGRADES: [&str; 11] = [
    "overwinter",
    "sapling",
    "blossom",
    "heartwood",
    "canopy",
    "nu5",
    "nu6",
    "nu6.1",
    "nu6.2",
    "nu6.3",
    "nu7",
];

fn is_base58(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() && !matches!(byte, b'0' | b'O' | b'I' | b'l')
}

fn is_bech32(byte: u8) -> bool {
    matches!(byte, b'0' | b'2'..=b'9' | b'a'..=b'h' | b'j'..=b'k' | b'm'..=b'n' | b'p'..=b'z')
}

fn all(text: &str, class: fn(u8) -> bool) -> bool {
    !text.is_empty() && text.bytes().all(class)
}

/// Strips what surrounds a pasted query: whitespace, quotes, angle brackets and backticks.
pub fn normalize(input: &str) -> &str {
    let mut text = input.trim();
    loop {
        let stripped = [('"', '"'), ('\'', '\''), ('`', '`'), ('<', '>')]
            .into_iter()
            .find_map(|(open, close)| text.strip_prefix(open)?.strip_suffix(close));
        match stripped {
            Some(inner) => text = inner.trim(),
            None => return text,
        }
    }
}

/// What `input` is, for an Explorer whose node runs on `network`.
pub fn classify_query(input: &str, network: Network) -> Classified {
    let text = normalize(input);
    if text.is_empty() {
        return Classified::Empty;
    }
    if let Some(kind) = secret(text).or_else(|| secret_inside(text)) {
        return Classified::Secret(kind);
    }
    if let Some((_, rest)) = text.split_once("://") {
        return link(rest, network);
    }
    if is_route(text) {
        return route(text, network);
    }
    classify_plain(text, network)
}

/// How many rounds of percent-encoding [`secret_inside`] undoes: enough for a search route
/// nested four deep, as the Explorer writes routes. Deeper ones make no route ([`route`]).
const DECODE_ROUNDS: usize = 4;

/// A key or a recovery phrase inside a pasted link or route, at any depth: a part between the
/// separators of links and routes that is one, or a run of the characters keys are written in
/// that starts like one. Looked for again once percent-decoding has undone each of up to
/// [`DECODE_ROUNDS`] rounds of encoding, so the work stays linear in the text.
fn secret_inside(text: &str) -> Option<SecretKind> {
    let mut decoded = Cow::Borrowed(text);
    for _ in 0..DECODE_ROUNDS {
        let parts = decoded.split(['?', '&', '#', '=', '/']).map(normalize);
        let runs = decoded.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
        if let Some(kind) = parts.chain(runs).find_map(secret) {
            return Some(kind);
        }
        match percent_decode(&decoded) {
            Some(next) if next != *decoded => decoded = Cow::Owned(next),
            _ => break,
        }
    }
    None
}

fn is_route(text: &str) -> bool {
    text.contains('/') || text.starts_with("search?")
}

/// The part of a link after `scheme://`: its fragment, its `q` parameter or its last path
/// segment, the first that is something the search reads. One level only: a link inside a
/// link is not unwrapped.
fn link(rest: &str, network: Network) -> Classified {
    let (before, fragment) = match rest.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (rest, None),
    };
    let (path, query) = match before.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (before, None),
    };
    // The fragment may be one of the Explorer's routes.
    if let Some(fragment) = fragment.and_then(percent_decode) {
        let fragment = normalize(&fragment);
        let found = if is_route(fragment) {
            route(fragment, network)
        } else {
            classify_plain(fragment, network)
        };
        if !matches!(found, Classified::Unrecognised | Classified::Empty) {
            return found;
        }
    }
    let q = query.and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("q=")));
    let last = path.rsplit('/').find(|segment| !segment.is_empty());
    for part in [q, last].into_iter().flatten().filter_map(percent_decode) {
        match classify_plain(normalize(&part), network) {
            Classified::Unrecognised | Classified::Empty => {}
            found => return found,
        }
    }
    Classified::Unrecognised
}

/// One of the Explorer's own routes, parsed once. A search route's query is read one level
/// down: a key there is refused, and a query the search does not read, such as another route,
/// makes no route, so a route never carries text nothing looked at.
fn route(text: &str, network: Network) -> Classified {
    match ExplorerRoute::parse(text) {
        Some(ExplorerRoute::Search { query }) => {
            let query = normalize(&query);
            if let Some(kind) = secret(query) {
                return Classified::Secret(kind);
            }
            match classify_plain(query, network) {
                Classified::Unrecognised | Classified::Empty => Classified::Unrecognised,
                _ => Classified::Route(ExplorerRoute::Search {
                    query: query.to_owned(),
                }),
            }
        }
        Some(route) => Classified::Route(route),
        None => Classified::Unrecognised,
    }
}

fn classify_plain(text: &str, network: Network) -> Classified {
    let lower = text.to_ascii_lowercase();
    if let Some(height) = height(text) {
        return Classified::Height(height);
    }
    if text.len() == 10 && text.bytes().all(|b| b.is_ascii_digit()) {
        return Classified::Later(Later::UnixTime);
    }
    if let Some(below) = below_tip(&lower) {
        return Classified::BelowTip(below);
    }
    if let Some(hash) = Hash32::parse(text) {
        return Classified::Hash(hash);
    }
    if let Some(outpoint) = outpoint(text) {
        return Classified::Outpoint(outpoint);
    }
    let hex = text.bytes().all(|b| b.is_ascii_hexdigit());
    if hex
        && (6..64).contains(&text.len())
        && (text.bytes().any(|b| b.is_ascii_alphabetic()) || text.starts_with('0'))
    {
        return Classified::Later(Later::HashPrefix);
    }
    if let Some(later) = address(text, network) {
        return Classified::Later(later);
    }
    if lower.starts_with("zcash:") {
        return Classified::Later(Later::PaymentRequest);
    }
    if is_date(text) {
        return Classified::Later(Later::Date);
    }
    if POOLS.contains(&lower.as_str()) {
        return Classified::Later(Later::Pool);
    }
    if UPGRADES.contains(&lower.as_str()) {
        return Classified::Later(Later::Upgrade);
    }
    if let Some(number) = lower.strip_prefix("zip") {
        let number = number.trim_start_matches([' ', '-']);
        if (1..=4).contains(&number.len()) && number.bytes().all(|b| b.is_ascii_digit()) {
            return Classified::Later(Later::Zip);
        }
    }
    if text.starts_with('>') {
        return Classified::Later(Later::Command);
    }
    Classified::Unrecognised
}

/// A key or a recovery phrase, by prefix or by shape.
fn secret(text: &str) -> Option<SecretKind> {
    let lower = text.to_ascii_lowercase();
    if let Some((_, kind)) = KEY_PREFIXES
        .iter()
        .find(|(prefix, _)| lower.starts_with(prefix))
    {
        return Some(*kind);
    }
    let base58 = all(text, is_base58);
    // zcashd's Sprout keys: spending keys `SK…` and `ST…`, viewing keys `ZiVK…` and `ZiVt…`.
    if base58 && (text.starts_with("SK") || text.starts_with("ST")) && text.len() >= 50 {
        return Some(SecretKind::SpendingKey);
    }
    if base58 && (text.starts_with("ZiVK") || text.starts_with("ZiVt")) && text.len() >= 90 {
        return Some(SecretKind::ViewingKey);
    }
    // WIF private keys: base58 of 51 characters from `5` or `9`, or 52 from `K`, `L` or `c`.
    let first = *text.as_bytes().first()?;
    if base58
        && ((text.len() == 51 && matches!(first, b'5' | b'9'))
            || (text.len() == 52 && matches!(first, b'K' | b'L' | b'c')))
    {
        return Some(SecretKind::SpendingKey);
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if matches!(words.len(), 12 | 15 | 18 | 21 | 24)
        && words
            .iter()
            .all(|word| word.bytes().all(|b| b.is_ascii_lowercase()))
    {
        return Some(SecretKind::RecoveryPhrase);
    }
    None
}

/// One to nine digits once `,`, `_`, `'` and spaces are removed, without a leading zero.
fn height(text: &str) -> Option<u32> {
    if !text
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b',' | b'_' | b'\'' | b' '))
    {
        return None;
    }
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let valid = (1..=9).contains(&digits.len()) && (digits == "0" || !digits.starts_with('0'));
    valid.then(|| digits.parse().ok()).flatten()
}

fn below_tip(lower: &str) -> Option<u32> {
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if compact == "tip" || compact == "latest" {
        return Some(0);
    }
    let number = |digits: &str| {
        let valid = (1..=9).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit());
        valid.then(|| digits.parse::<u32>().ok()).flatten()
    };
    if let Some(back) = compact.strip_prefix("tip-") {
        return number(back);
    }
    compact
        .strip_prefix('-')
        .and_then(number)
        .filter(|n| *n > 0)
        .map(|n| n - 1)
}

fn outpoint(text: &str) -> Option<OutPoint> {
    let (txid, vout) = text.split_once([':', '-'])?;
    let valid = (1..=5).contains(&vout.len()) && vout.bytes().all(|b| b.is_ascii_digit());
    Some(OutPoint {
        txid: Hash32::parse(txid)?,
        vout: valid.then(|| vout.parse().ok()).flatten()?,
    })
}

fn is_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// An address of either network, by its prefix and length.
fn address(text: &str, network: Network) -> Option<Later> {
    let mainnet = network == Network::Mainnet;
    let base58_after = |prefix: &str, len: usize| {
        text.strip_prefix(prefix)
            .is_some_and(|rest| rest.len() == len && all(rest, is_base58))
    };
    let bech32_after = |prefix: &str, len: Option<usize>| {
        text.strip_prefix(prefix)
            .is_some_and(|rest| all(rest, is_bech32) && len.is_none_or(|len| rest.len() == len))
    };
    // (is this network's, matches)
    let transparent = [
        (mainnet, base58_after("t1", 33) || base58_after("t3", 33)),
        (!mainnet, base58_after("tm", 33) || base58_after("t2", 33)),
    ];
    let tex = [
        (mainnet, bech32_after("tex1", Some(38))),
        (!mainnet, bech32_after("textest1", Some(38))),
    ];
    let sapling = [
        (mainnet, bech32_after("zs1", Some(75))),
        (!mainnet, bech32_after("ztestsapling1", Some(75))),
    ];
    let unified = [
        (
            mainnet,
            ["u1", "zu1", "tu1"]
                .into_iter()
                .any(|prefix| bech32_after(prefix, None)),
        ),
        (
            !mainnet,
            ["utest1", "zutest1", "tutest1"]
                .into_iter()
                .any(|prefix| bech32_after(prefix, None)),
        ),
    ];
    let sprout = [
        (mainnet, base58_after("zc", 93)),
        (!mainnet, base58_after("zt", 93)),
    ];
    let pick = |matches: [(bool, bool); 2]| {
        matches
            .into_iter()
            .find(|(_, matched)| *matched)
            .map(|(ours, _)| !ours)
    };
    if let Some(other_network) = pick(transparent) {
        return Some(Later::Transparent { other_network });
    }
    if let Some(other_network) = pick(tex) {
        return Some(Later::Tex { other_network });
    }
    if let Some(other_network) = pick(sapling) {
        return Some(Later::Sapling { other_network });
    }
    if let Some(other_network) = pick(unified) {
        return Some(Later::Unified { other_network });
    }
    if let Some(other_network) = pick(sprout) {
        return Some(Later::Sprout { other_network });
    }
    None
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::BlockId;

    use super::*;

    const HASH: &str = "0000000000d8a9ac4e30b7a7ce2bd2b3b1b9c67b6ec23f5a3e7ba7ca1d4ab501";

    fn main(input: &str) -> Classified {
        classify_query(input, Network::Mainnet)
    }

    #[test]
    fn secrets_are_refused_first() {
        let viewing = [
            "zxviews1qvyaaaaa",
            "ZXVIEWS1QVYAAAAA",
            "zxviewtestsapling1q",
            "zxviewregtestsapling1q",
            "uview1abc",
            "uviewtest1abc",
            "uviewregtest1abc",
            "uivk1abc",
            "uivktest1abc",
            "uivkregtest1abc",
            "uvf1abc",
            "uvftest1abc",
            "uvfregtest1abc",
            "uvi1abc",
            "uvitest1abc",
            "uviregtest1abc",
        ];
        for key in viewing {
            assert_eq!(
                main(key),
                Classified::Secret(SecretKind::ViewingKey),
                "{key}"
            );
            assert_eq!(
                classify_query(key, Network::Testnet),
                Classified::Secret(SecretKind::ViewingKey),
                "{key} on Testnet"
            );
        }
        for key in [
            "secret-extended-key-main1qq",
            "secret-extended-key-test1qq",
            "secret-extended-key-regtest1qq",
            &format!("SK{}", "a".repeat(50)),
            &format!("5{}", "H".repeat(50)),
            &format!("K{}", "z".repeat(51)),
            &format!("c{}", "z".repeat(51)),
        ] {
            assert_eq!(
                main(key),
                Classified::Secret(SecretKind::SpendingKey),
                "{key}"
            );
        }
        assert_eq!(
            main(&format!("ZiVK{}", "a".repeat(93))),
            Classified::Secret(SecretKind::ViewingKey)
        );
        let phrase =
            "abandon ability able about above absent absorb abstract absurd abuse access accident";
        assert_eq!(main(phrase), Classified::Secret(SecretKind::RecoveryPhrase));
        assert_eq!(
            main(&format!("{phrase} acid account achieve")),
            Classified::Secret(SecretKind::RecoveryPhrase)
        );
        assert_eq!(
            main("abandon ability able"),
            Classified::Unrecognised,
            "three words"
        );
        // A secret inside a search route is refused too, in either spelling.
        assert_eq!(
            main("search?q=uview1abc"),
            Classified::Secret(SecretKind::ViewingKey)
        );
        assert_eq!(
            main("https://example.com/search?q=zxviews1qqqq"),
            Classified::Secret(SecretKind::ViewingKey)
        );
        assert!(matches!(main("search?q=3120000"), Classified::Route(_)));
        // A secret inside quotes is still refused.
        assert_eq!(
            main("\"uview1abc\""),
            Classified::Secret(SecretKind::ViewingKey)
        );
        // A key in a link's `q` beside a fragment that is something else, a phrase written as
        // a query string writes it, a key in another parameter, and a key one route further
        // down.
        for link in [
            format!("https://x.example/?q=uview1qqqqqqqq#{HASH}"),
            "https://x.example/?q=uview1qqqqqqqq#tip-5".to_owned(),
            "https://x.example/search?q=abandon+ability+able+about+above+absent+absorb+abstract+\
             absurd+abuse+access+accident"
                .to_owned(),
            "https://x.example/?vk=zxviews1qqqq&q=3120000".to_owned(),
            "http://localhost:1420/#search?q=uview1qqqq".to_owned(),
            "search?q=search%3Fq%3Duview1abc".to_owned(),
        ] {
            assert!(matches!(main(&link), Classified::Secret(_)), "{link}");
        }
        // Search routes nested as the Explorer writes them: refused four deep, and never a
        // route deeper down.
        let nest = |depth: usize| {
            (0..depth).fold("uview1abc".to_owned(), |query, _| {
                ExplorerRoute::Search { query }.to_text()
            })
        };
        assert_eq!(main(&nest(4)), Classified::Secret(SecretKind::ViewingKey));
        assert!(!matches!(main(&nest(12)), Classified::Route(_)));
    }

    #[test]
    fn nested_links_and_routes_are_read_one_level_down() {
        // However deep, a link or a route is unwrapped once: linear work, no recursion.
        let routes = format!("{}3120000", "search?q=".repeat(10_000));
        assert_eq!(
            main(&routes),
            Classified::Unrecognised,
            "a route in a route"
        );
        let links = format!("{}{HASH}", "https://a/#".repeat(4_000));
        assert_eq!(main(&links), Classified::Unrecognised, "a link in a link");
        let fragments = format!("{}3120000", "s://a?q=#".repeat(40));
        assert_eq!(main(&fragments), Classified::Unrecognised);
        assert!(matches!(
            main(&format!("https://x.example/#search?q={HASH}")),
            Classified::Route(ExplorerRoute::Search { .. })
        ));
        assert_eq!(main("search?q=hello"), Classified::Unrecognised);
    }

    #[test]
    fn heights_with_separators() {
        assert_eq!(main("3120000"), Classified::Height(3_120_000));
        assert_eq!(main(" 3,120,000 "), Classified::Height(3_120_000));
        assert_eq!(main("3_120_000"), Classified::Height(3_120_000));
        assert_eq!(main("3'120'000"), Classified::Height(3_120_000));
        assert_eq!(main("3 120 000"), Classified::Height(3_120_000));
        assert_eq!(main("0"), Classified::Height(0));
        assert_eq!(main("999999999"), Classified::Height(999_999_999));
        assert_eq!(main("1761200000"), Classified::Later(Later::UnixTime));
        assert_eq!(main("0123"), Classified::Unrecognised, "a leading zero");
        assert_eq!(main("012345"), Classified::Later(Later::HashPrefix));
    }

    #[test]
    fn heights_below_the_tip() {
        assert_eq!(main("tip"), Classified::BelowTip(0));
        assert_eq!(main("LATEST"), Classified::BelowTip(0));
        assert_eq!(main("tip-5"), Classified::BelowTip(5));
        assert_eq!(main("tip - 5"), Classified::BelowTip(5));
        assert_eq!(main("-1"), Classified::BelowTip(0), "-1 is the tip");
        assert_eq!(main("-10"), Classified::BelowTip(9));
        assert_eq!(main("-0"), Classified::Unrecognised);
    }

    #[test]
    fn hashes_outpoints_and_prefixes() {
        let hash = Hash32::parse(HASH).unwrap();
        assert_eq!(main(HASH), Classified::Hash(hash));
        assert_eq!(main(&HASH.to_uppercase()), Classified::Hash(hash));
        assert_eq!(main(&format!("`{HASH}`")), Classified::Hash(hash));
        assert_eq!(main(&format!("<{HASH}>")), Classified::Hash(hash));
        assert_eq!(
            main(&format!("{HASH}:3")),
            Classified::Outpoint(OutPoint {
                txid: hash,
                vout: 3
            })
        );
        assert_eq!(
            main(&format!("{HASH}-12")),
            Classified::Outpoint(OutPoint {
                txid: hash,
                vout: 12
            })
        );
        assert_eq!(main(&format!("{HASH}:123456")), Classified::Unrecognised);
        assert_eq!(main("d8a9ac4e"), Classified::Later(Later::HashPrefix));
        assert_eq!(
            main("abc"),
            Classified::Unrecognised,
            "too short for a prefix"
        );
    }

    #[test]
    fn links_and_routes() {
        let hash = Hash32::parse(HASH).unwrap();
        assert_eq!(
            main(&format!("https://example.com/tx/{HASH}")),
            Classified::Hash(hash)
        );
        assert_eq!(
            main(&format!("https://example.com/search?q={HASH}")),
            Classified::Hash(hash)
        );
        assert_eq!(
            main("https://example.com/block/3120000/"),
            Classified::Height(3_120_000)
        );
        assert_eq!(main("https://example.com/about"), Classified::Unrecognised);
        assert_eq!(
            main("block/3120000"),
            Classified::Route(ExplorerRoute::Block {
                id: BlockId::Height(3_120_000)
            })
        );
        assert_eq!(main("tx/nothex"), Classified::Unrecognised);
    }

    #[test]
    fn addresses_by_prefix_and_network() {
        let t1 = format!("t1{}", "a".repeat(33));
        let tm = format!("tm{}", "a".repeat(33));
        assert_eq!(
            main(&t1),
            Classified::Later(Later::Transparent {
                other_network: false
            })
        );
        assert_eq!(
            main(&tm),
            Classified::Later(Later::Transparent {
                other_network: true
            })
        );
        assert_eq!(
            classify_query(&tm, Network::Testnet),
            Classified::Later(Later::Transparent {
                other_network: false
            })
        );
        assert_eq!(
            main(&format!("t1{}", "a".repeat(32))),
            Classified::Unrecognised,
            "one short"
        );
        assert_eq!(
            main(&format!("t1{}0", "a".repeat(32))),
            Classified::Unrecognised,
            "0 is not base58"
        );
        assert_eq!(
            main(&format!("tex1{}", "q".repeat(38))),
            Classified::Later(Later::Tex {
                other_network: false
            })
        );
        assert_eq!(
            main(&format!("zs1{}", "q".repeat(75))),
            Classified::Later(Later::Sapling {
                other_network: false
            })
        );
        assert_eq!(
            main(&format!("ztestsapling1{}", "q".repeat(75))),
            Classified::Later(Later::Sapling {
                other_network: true
            })
        );
        for unified in ["u1qqqq", "zu1qqqq", "tu1qqqq"] {
            assert_eq!(
                main(unified),
                Classified::Later(Later::Unified {
                    other_network: false
                }),
                "{unified}"
            );
        }
        assert_eq!(
            main("utest1qqqq"),
            Classified::Later(Later::Unified {
                other_network: true
            })
        );
        assert_eq!(
            main(&format!("zc{}", "a".repeat(93))),
            Classified::Later(Later::Sprout {
                other_network: false
            })
        );
    }

    #[test]
    fn names_dates_and_commands() {
        assert_eq!(
            main("zcash:t1abc?amount=1"),
            Classified::Later(Later::PaymentRequest)
        );
        assert_eq!(main("2026-10-01"), Classified::Later(Later::Date));
        assert_eq!(main("2026-10-01T14:02"), Classified::Later(Later::Date));
        assert_eq!(main("Ironwood"), Classified::Later(Later::Pool));
        assert_eq!(
            main("sapling"),
            Classified::Later(Later::Pool),
            "a pool first"
        );
        assert_eq!(main("NU6.3"), Classified::Later(Later::Upgrade));
        assert_eq!(main("zip 317"), Classified::Later(Later::Zip));
        assert_eq!(main("ZIP-218"), Classified::Later(Later::Zip));
        assert_eq!(main(">mempool"), Classified::Later(Later::Command));
        assert_eq!(main(""), Classified::Empty);
        assert_eq!(main("   "), Classified::Empty);
        assert_eq!(main("hello"), Classified::Unrecognised);
    }
}
