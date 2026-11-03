//! The check a wallet runs on a server before it uses it: Zodl's
//! `SdkSynchronizer.validateServerEndpoint` compares `GetLightdInfo`'s chain name, Sapling
//! activation height and consensus branch id with its own tables. Zeal runs the same check on
//! zebrad's endpoint, and through the front door in the test on this computer.

use launcher_types::{Network, public::LightdInfo, public::WalletProblem};

/// How far the endpoint's height may be from zebrad's own.
pub const HEIGHT_TOLERANCE: u64 = 2;

/// The network upgrades and their consensus branch ids, as zebra-chain at zebrad 7.0.0 has
/// them (`zebra-chain/src/parameters/network_upgrade.rs`, `CONSENSUS_BRANCH_IDS`).
const BRANCH_IDS: [(&str, u32); 11] = [
    ("Overwinter", 0x5ba8_1b19),
    ("Sapling", 0x76b8_09bb),
    ("Blossom", 0x2bb4_0e60),
    ("Heartwood", 0xf5b9_230b),
    ("Canopy", 0xe9ff_75a6),
    ("NU5", 0xc2d6_d0b4),
    ("NU6", 0xc8e7_1055),
    ("NU6.1", 0x4dec_4df0),
    ("NU6.2", 0x5437_f330),
    ("NU6.3", 0x37a5_165b),
    ("NU7", 0x7719_0ad9),
];

/// Mainnet's activation heights, in [`BRANCH_IDS`]' order
/// (`zebra-chain/src/parameters/constants.rs` at zebrad 7.0.0).
const MAINNET_HEIGHTS: [u64; 11] = [
    347_500, 419_200, 653_600, 903_000, 1_046_400, 1_687_104, 2_726_400, 3_146_400, 3_364_600,
    3_428_143, 3_543_405,
];

/// Testnet's activation heights, in [`BRANCH_IDS`]' order.
const TESTNET_HEIGHTS: [u64; 11] = [
    207_500, 280_000, 584_000, 903_800, 1_028_500, 1_842_420, 2_976_000, 3_536_500, 4_052_000,
    4_134_000, 4_465_026,
];

fn heights(network: Network) -> &'static [u64; 11] {
    match network {
        Network::Mainnet => &MAINNET_HEIGHTS,
        Network::Testnet => &TESTNET_HEIGHTS,
    }
}

/// The chain name `GetLightdInfo` reports: `main` or `test`.
pub fn chain_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "main",
        Network::Testnet => "test",
    }
}

/// The height Sapling activated at: 419,200 on Mainnet, 280,000 on Testnet.
pub fn sapling_activation_height(network: Network) -> u64 {
    heights(network)[1]
}

/// The network upgrade active at `height`, by name, or `None` before Overwinter.
pub fn upgrade_at(network: Network, height: u64) -> Option<&'static str> {
    let index = heights(network)
        .iter()
        .rposition(|start| *start <= height)?;
    Some(BRANCH_IDS[index].0)
}

/// The consensus branch id at `height` as `GetLightdInfo` writes it: eight lowercase hex
/// digits. `None` before Overwinter, which has no branch id.
pub fn branch_id_at(network: Network, height: u64) -> Option<String> {
    let index = heights(network)
        .iter()
        .rposition(|start| *start <= height)?;
    Some(format!("{:08x}", BRANCH_IDS[index].1))
}

/// Zodl's check of `info` for `network`; with `node_height`, also that the endpoint is within
/// [`HEIGHT_TOLERANCE`] blocks of zebrad's own height.
pub fn check(
    info: &LightdInfo,
    network: Network,
    node_height: Option<u64>,
) -> Result<(), WalletProblem> {
    let want = chain_name(network);
    if info.chain_name != want {
        return Err(WalletProblem::WrongChain {
            got: info.chain_name.clone(),
            want: want.to_string(),
        });
    }
    let want = sapling_activation_height(network);
    if info.sapling_activation_height != want {
        return Err(WalletProblem::WrongSaplingHeight {
            got: info.sapling_activation_height,
            want,
        });
    }
    let want = branch_id_at(network, info.block_height).unwrap_or_else(|| "00000000".into());
    if !info.consensus_branch_id.eq_ignore_ascii_case(&want) {
        return Err(WalletProblem::WrongBranch {
            got: info.consensus_branch_id.clone(),
            want,
        });
    }
    if let Some(want) = node_height
        && info.block_height.abs_diff(want) > HEIGHT_TOLERANCE
    {
        return Err(WalletProblem::HeightOff {
            got: info.block_height,
            want,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(network: Network, height: u64) -> LightdInfo {
        LightdInfo {
            version: "7.0.0".into(),
            vendor: "ZcashFoundation/zebra".into(),
            chain_name: chain_name(network).into(),
            sapling_activation_height: sapling_activation_height(network),
            consensus_branch_id: branch_id_at(network, height).unwrap(),
            block_height: height,
            zcashd_build: "7.0.0".into(),
            zcashd_subversion: "/Zebra:7.0.0/".into(),
        }
    }

    #[test]
    fn branch_ids_follow_the_activation_heights() {
        assert_eq!(branch_id_at(Network::Mainnet, 0), None);
        assert_eq!(
            branch_id_at(Network::Mainnet, 347_500).as_deref(),
            Some("5ba81b19")
        );
        assert_eq!(
            branch_id_at(Network::Mainnet, 3_543_404).as_deref(),
            Some("37a5165b")
        );
        assert_eq!(
            branch_id_at(Network::Mainnet, 3_543_405).as_deref(),
            Some("77190ad9")
        );
        assert_eq!(upgrade_at(Network::Mainnet, 3_543_405), Some("NU7"));
        assert_eq!(
            branch_id_at(Network::Testnet, 4_465_025).as_deref(),
            Some("37a5165b")
        );
        assert_eq!(
            branch_id_at(Network::Testnet, 4_465_026).as_deref(),
            Some("77190ad9")
        );
        assert_eq!(sapling_activation_height(Network::Mainnet), 419_200);
        assert_eq!(sapling_activation_height(Network::Testnet), 280_000);
        for network in [Network::Mainnet, Network::Testnet] {
            assert!(heights(network).windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn a_matching_server_passes() {
        let info = info(Network::Mainnet, 3_544_812);
        assert_eq!(check(&info, Network::Mainnet, Some(3_544_813)), Ok(()));
        assert_eq!(check(&info, Network::Mainnet, None), Ok(()));
    }

    #[test]
    fn each_mismatch_is_named() {
        let good = info(Network::Mainnet, 3_544_812);
        let testnet = info(Network::Testnet, 3_544_812);
        assert!(matches!(
            check(&testnet, Network::Mainnet, None),
            Err(WalletProblem::WrongChain { .. })
        ));
        let sapling = LightdInfo {
            sapling_activation_height: 280_000,
            ..good.clone()
        };
        assert!(matches!(
            check(&sapling, Network::Mainnet, None),
            Err(WalletProblem::WrongSaplingHeight { got: 280_000, .. })
        ));
        // A server stuck before NU7 that reports a tip past it.
        let stale_branch = LightdInfo {
            consensus_branch_id: "37a5165b".into(),
            ..good.clone()
        };
        assert_eq!(
            check(&stale_branch, Network::Mainnet, None),
            Err(WalletProblem::WrongBranch {
                got: "37a5165b".into(),
                want: "77190ad9".into()
            })
        );
        assert_eq!(
            check(&good, Network::Mainnet, Some(3_544_815)),
            Err(WalletProblem::HeightOff {
                got: 3_544_812,
                want: 3_544_815
            })
        );
        // Upper-case hex is the same id.
        let upper = LightdInfo {
            consensus_branch_id: "77190AD9".into(),
            ..good
        };
        assert_eq!(check(&upper, Network::Mainnet, None), Ok(()));
    }
}
