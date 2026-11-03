//! Network upgrades: their activation heights and the target block spacing they set.
//!
//! The heights come from the node (`NodeDetails::upgrades`, from `getblockchaininfo`); the
//! constants zebrad 7.0.0 builds in (zebra-chain/src/parameters/constants.rs) fill in what the
//! node does not report.

use launcher_types::{Network, NetworkUpgrade, UpgradeStatus};

/// The network upgrades the explorer knows, in activation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Upgrade {
    Overwinter,
    Sapling,
    Blossom,
    Heartwood,
    Canopy,
    Nu5,
    Nu6,
    Nu6_1,
    Nu6_2,
    /// Ironwood.
    Nu6_3,
    Nu7,
}

impl Upgrade {
    /// Every upgrade, in activation order.
    pub const ALL: [Upgrade; 11] = [
        Upgrade::Overwinter,
        Upgrade::Sapling,
        Upgrade::Blossom,
        Upgrade::Heartwood,
        Upgrade::Canopy,
        Upgrade::Nu5,
        Upgrade::Nu6,
        Upgrade::Nu6_1,
        Upgrade::Nu6_2,
        Upgrade::Nu6_3,
        Upgrade::Nu7,
    ];

    /// zebrad's name, as `getblockchaininfo` prints it.
    pub fn name(self) -> &'static str {
        match self {
            Upgrade::Overwinter => "Overwinter",
            Upgrade::Sapling => "Sapling",
            Upgrade::Blossom => "Blossom",
            Upgrade::Heartwood => "Heartwood",
            Upgrade::Canopy => "Canopy",
            Upgrade::Nu5 => "NU5",
            Upgrade::Nu6 => "NU6",
            Upgrade::Nu6_1 => "NU6.1",
            Upgrade::Nu6_2 => "NU6.2",
            Upgrade::Nu6_3 => "NU6.3",
            Upgrade::Nu7 => "NU7",
        }
    }

    /// The upgrade zebrad calls `name`.
    pub fn from_name(name: &str) -> Option<Upgrade> {
        Upgrade::ALL
            .into_iter()
            .find(|upgrade| upgrade.name() == name)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// Seconds between blocks before Blossom.
pub const PRE_BLOSSOM_SPACING: u32 = 150;
/// Seconds between blocks from Blossom.
pub const POST_BLOSSOM_SPACING: u32 = 75;
/// Seconds between blocks from NU7 (ZIP 218).
pub const POST_NU7_SPACING: u32 = 25;

/// zebrad 7.0.0's Mainnet activation heights, in [`Upgrade::ALL`]'s order.
const MAINNET: [u32; 11] = [
    347_500, 419_200, 653_600, 903_000, 1_046_400, 1_687_104, 2_726_400, 3_146_400, 3_364_600,
    3_428_143, 3_543_405,
];

/// zebrad 7.0.0's Testnet activation heights, in [`Upgrade::ALL`]'s order.
const TESTNET: [u32; 11] = [
    207_500, 280_000, 584_000, 903_800, 1_028_500, 1_842_420, 2_976_000, 3_536_500, 4_052_000,
    4_134_000, 4_465_026,
];

/// Where each upgrade activates on one network; `None` for an upgrade without a height there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeHeights {
    heights: [Option<u32>; 11],
}

impl UpgradeHeights {
    /// The heights zebrad 7.0.0 builds in for `network`.
    pub fn defaults(network: Network) -> Self {
        let table = match network {
            Network::Mainnet => MAINNET,
            Network::Testnet => TESTNET,
        };
        Self {
            heights: table.map(Some),
        }
    }

    /// The node's heights where it reports them, the defaults elsewhere. An upgrade the node
    /// reports as disabled has no height.
    pub fn from_node(upgrades: &[NetworkUpgrade], network: Network) -> Self {
        let mut heights = Self::defaults(network);
        for reported in upgrades {
            let Some(upgrade) = Upgrade::from_name(&reported.name) else {
                continue;
            };
            heights.heights[upgrade.index()] = match reported.status {
                UpgradeStatus::Disabled => None,
                UpgradeStatus::Active | UpgradeStatus::Pending => {
                    u32::try_from(reported.activation_height).ok()
                }
            };
        }
        heights
    }

    /// Where `upgrade` activates.
    pub fn height(&self, upgrade: Upgrade) -> Option<u32> {
        self.heights[upgrade.index()]
    }

    /// Whether `upgrade`'s rules apply at `height`.
    pub fn is_active(&self, upgrade: Upgrade, height: u32) -> bool {
        self.height(upgrade).is_some_and(|start| height >= start)
    }

    /// The latest upgrade whose rules apply at `height`.
    pub fn current(&self, height: u32) -> Option<Upgrade> {
        Upgrade::ALL
            .into_iter()
            .filter(|upgrade| self.is_active(*upgrade, height))
            .max_by_key(|upgrade| self.height(*upgrade))
    }

    /// The upgrade whose first block is at `height`.
    pub fn activating_at(&self, height: u32) -> Option<Upgrade> {
        Upgrade::ALL
            .into_iter()
            .find(|upgrade| self.height(*upgrade) == Some(height))
    }

    /// The first upgrade that activates above `height`, with its height.
    pub fn next_after(&self, height: u32) -> Option<(Upgrade, u32)> {
        Upgrade::ALL
            .into_iter()
            .filter_map(|upgrade| Some((upgrade, self.height(upgrade)?)))
            .filter(|(_, start)| *start > height)
            .min_by_key(|(_, start)| *start)
    }

    /// The upgrades that activate in `low+1..=high`, in order.
    pub fn between(&self, low: u32, high: u32) -> Vec<(Upgrade, u32)> {
        let mut list: Vec<(Upgrade, u32)> = Upgrade::ALL
            .into_iter()
            .filter_map(|upgrade| Some((upgrade, self.height(upgrade)?)))
            .filter(|(_, start)| *start > low && *start <= high)
            .collect();
        list.sort_by_key(|(_, start)| *start);
        list
    }

    /// The target seconds between the block at `height` and its parent.
    pub fn target_spacing(&self, height: u32) -> u32 {
        if self.is_active(Upgrade::Nu7, height) {
            POST_NU7_SPACING
        } else if self.is_active(Upgrade::Blossom, height) {
            POST_BLOSSOM_SPACING
        } else {
            PRE_BLOSSOM_SPACING
        }
    }

    /// The target seconds of the blocks in `from+1..=to`, each at its own height's spacing:
    /// zebra-chain's `duration_between_heights` for `from <= to`, zero otherwise.
    pub fn spacing_between(&self, from: u32, to: u32) -> u64 {
        if to <= from {
            return 0;
        }
        let mut edges = vec![(0u32, PRE_BLOSSOM_SPACING)];
        if let Some(blossom) = self.height(Upgrade::Blossom) {
            edges.push((blossom, POST_BLOSSOM_SPACING));
        }
        if let Some(nu7) = self.height(Upgrade::Nu7) {
            edges.push((nu7, POST_NU7_SPACING));
        }
        edges.sort_by_key(|(start, _)| *start);
        let (low, high) = (u64::from(from) + 1, u64::from(to));
        edges
            .iter()
            .enumerate()
            .map(|(index, (start, spacing))| {
                let first = u64::from(*start).max(low);
                let last = edges
                    .get(index + 1)
                    .map_or(high, |(next, _)| (u64::from(*next) - 1).min(high));
                (last + 1).saturating_sub(first) * u64::from(*spacing)
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for upgrade in Upgrade::ALL {
            assert_eq!(Upgrade::from_name(upgrade.name()), Some(upgrade));
        }
        assert_eq!(Upgrade::from_name("NU8"), None);
    }

    #[test]
    fn heights_and_eras() {
        let main = UpgradeHeights::defaults(Network::Mainnet);
        assert_eq!(main.height(Upgrade::Nu7), Some(3_543_405));
        assert_eq!(main.current(3_543_404), Some(Upgrade::Nu6_3));
        assert_eq!(main.current(3_543_405), Some(Upgrade::Nu7));
        assert_eq!(main.current(1), None);
        assert_eq!(main.activating_at(3_428_143), Some(Upgrade::Nu6_3));
        assert_eq!(main.activating_at(3_428_144), None);
        assert_eq!(main.next_after(3_428_143), Some((Upgrade::Nu7, 3_543_405)));
        assert_eq!(main.next_after(3_543_405), None);
        assert_eq!(
            main.between(3_300_000, 3_543_405),
            vec![
                (Upgrade::Nu6_2, 3_364_600),
                (Upgrade::Nu6_3, 3_428_143),
                (Upgrade::Nu7, 3_543_405)
            ]
        );
        let test = UpgradeHeights::defaults(Network::Testnet);
        assert_eq!(test.height(Upgrade::Nu7), Some(4_465_026));
    }

    #[test]
    fn the_node_overrides_the_constants() {
        let reported = [
            NetworkUpgrade {
                name: "NU7".into(),
                branch_id: "77190ad9".into(),
                activation_height: 3_600_000,
                status: UpgradeStatus::Pending,
            },
            NetworkUpgrade {
                name: "NU6.2".into(),
                branch_id: "5437f330".into(),
                activation_height: 0,
                status: UpgradeStatus::Disabled,
            },
            NetworkUpgrade {
                name: "Unknown".into(),
                branch_id: "00".into(),
                activation_height: 1,
                status: UpgradeStatus::Active,
            },
        ];
        let heights = UpgradeHeights::from_node(&reported, Network::Mainnet);
        assert_eq!(heights.height(Upgrade::Nu7), Some(3_600_000));
        assert_eq!(heights.height(Upgrade::Nu6_2), None);
        assert_eq!(heights.height(Upgrade::Nu6_3), Some(3_428_143), "kept");
    }

    #[test]
    fn spacing_follows_the_eras() {
        let main = UpgradeHeights::defaults(Network::Mainnet);
        assert_eq!(main.target_spacing(653_599), 150);
        assert_eq!(main.target_spacing(653_600), 75);
        assert_eq!(main.target_spacing(3_543_405), 25);
        // Each block counts at its own height's spacing.
        assert_eq!(main.spacing_between(653_598, 653_600), 150 + 75);
        assert_eq!(main.spacing_between(3_543_403, 3_543_406), 75 + 25 + 25);
        assert_eq!(main.spacing_between(10, 10), 0);
        assert_eq!(main.spacing_between(10, 5), 0);
        assert_eq!(main.spacing_between(3_000_000, 3_000_100), 100 * 75);
    }
}
