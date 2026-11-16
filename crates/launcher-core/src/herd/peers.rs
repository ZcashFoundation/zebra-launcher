//! Each node's own peer cache. zebrad's default `network.cache_dir = true` shares one peer
//! file among every zebrad the user runs, so they would all start by dialling the same
//! addresses. Zeal sets the key to each node's own folder ([`PeerCache::Own`]), and a node
//! saved before that is moved to its own cache once, with a copy of the shared file
//! ([`adopt`]).

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use launcher_types::Network;

use crate::{
    config::{self, PeerCacheKey},
    nodes::{NodeSpec, PeerCache},
    paths::Paths,
};

/// zebrad's peer file in `cache_dir` for `network`: zebra-network appends
/// `network/<network>.peers` to the cache folder (`peer_cache_file_path` in
/// `zebra-network/src/config/cache_dir.rs` at v7.0.0).
pub fn peer_file(cache_dir: &Path, network: Network) -> PathBuf {
    cache_dir
        .join("network")
        .join(format!("{}.peers", network.as_str().to_ascii_lowercase()))
}

/// Decides who sets the `network.cache_dir` of `node`, saved before Zeal set the key, from
/// what its `zebrad.toml` says: `false` or a folder of the user's stays the user's
/// ([`PeerCache::Kept`]); no key or `true` becomes the node's own folder ([`PeerCache::Own`]),
/// starting from a copy of the shared peer file in `zebra_cache_dir` (zebrad's default cache
/// folder) when there is one and the node has none yet. A file that cannot be read leaves
/// the node [`PeerCache::Undecided`], to be decided at the next launch, and so does a
/// configuration change that a launcher exit interrupted: its file is the abandoned one, and
/// the runtime restores the previous one ([`config::recover`]) only after the list loads. A
/// copy that fails is logged: the node then finds its peers from the DNS seeders.
pub fn adopt(node: &NodeSpec, paths: &Paths, zebra_cache_dir: Option<&Path>) -> PeerCache {
    if node.peer_cache != PeerCache::Undecided {
        return node.peer_cache;
    }
    let files = node.paths(paths);
    if files.pending_apply_marker().exists() {
        tracing::info!(node = %node.id, "a configuration change is to be rolled back; deciding the peer cache at the next launch");
        return PeerCache::Undecided;
    }
    match config::read_peer_cache(&files) {
        Ok(PeerCacheKey::Shared) => {
            let own = files.data_dir().to_path_buf();
            if let Some(shared) = zebra_cache_dir
                && let Err(err) = copy_shared(shared, &own, node.network)
            {
                tracing::warn!(node = %node.id, %err, "could not copy the shared peer cache");
            }
            PeerCache::Own
        }
        Ok(PeerCacheKey::Off | PeerCacheKey::Custom(_)) => PeerCache::Kept,
        Err(err) => {
            tracing::warn!(node = %node.id, %err, "could not read network.cache_dir; deciding at the next launch");
            PeerCache::Undecided
        }
    }
}

/// Copies the peer file of `network` from the shared cache folder `shared` into `own`,
/// unless `own` already has one or `shared` has none.
fn copy_shared(shared: &Path, own: &Path, network: Network) -> io::Result<()> {
    let from = peer_file(shared, network);
    let to = peer_file(own, network);
    if to.exists() || !from.is_file() {
        return Ok(());
    }
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::copy(&from, &to)?;
    tracing::info!(to = %to.display(), "copied the shared peer cache into the node's own folder");
    Ok(())
}

#[cfg(test)]
mod tests {
    use launcher_types::NodeId;

    use super::*;
    use crate::nodes::NodeLayout;

    #[test]
    fn the_peer_file_is_where_zebra_network_puts_it() {
        assert_eq!(
            peer_file(Path::new("/data/nodes/zebrad-mainnet-2"), Network::Mainnet),
            PathBuf::from("/data/nodes/zebrad-mainnet-2/network/mainnet.peers")
        );
        assert_eq!(
            peer_file(Path::new("/data"), Network::Testnet),
            PathBuf::from("/data/network/testnet.peers")
        );
    }

    fn undecided(network: Network, id: &str, layout: NodeLayout) -> NodeSpec {
        NodeSpec {
            id: NodeId::new(id).unwrap(),
            layout,
            peer_cache: PeerCache::Undecided,
            ..NodeSpec::first(network)
        }
    }

    #[test]
    fn a_node_saved_before_moves_to_its_own_cache_unless_its_file_says_otherwise() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(root.path().join("zeal"));
        let shared = root.path().join("cache/zebra");
        fs::create_dir_all(shared.join("network")).expect("mkdir");
        fs::write(peer_file(&shared, Network::Mainnet), "[\"192.0.2.1:8233\"]").expect("peers");

        // No key: its own folder, starting from the shared file, copied once.
        let first = undecided(Network::Mainnet, "zebrad-mainnet", NodeLayout::Launcher);
        assert_eq!(adopt(&first, &paths, Some(&shared)), PeerCache::Own);
        let own = peer_file(paths.data_dir(), Network::Mainnet);
        assert_eq!(
            fs::read_to_string(&own).expect("copied"),
            "[\"192.0.2.1:8233\"]"
        );
        assert!(
            peer_file(&shared, Network::Mainnet).exists(),
            "copied, not moved"
        );
        fs::write(&own, "[]").expect("the node's own file");
        let again = NodeSpec {
            peer_cache: PeerCache::Undecided,
            ..first.clone()
        };
        adopt(&again, &paths, Some(&shared));
        assert_eq!(
            fs::read_to_string(&own).expect("kept"),
            "[]",
            "never overwritten"
        );

        // `true`, and a node without a shared file: its own folder, nothing copied.
        let testnet = undecided(Network::Testnet, "zebrad-testnet", NodeLayout::Folder);
        let files = testnet.paths(&paths);
        files.ensure_dirs().expect("dirs");
        fs::write(files.zebrad_config(), "[network]\ncache_dir = true\n").expect("config");
        assert_eq!(adopt(&testnet, &paths, Some(&shared)), PeerCache::Own);
        assert!(!peer_file(files.data_dir(), Network::Testnet).exists());

        // `false` and a folder of the user's stay theirs.
        for value in ["false", "\"/srv/peers\""] {
            fs::write(
                files.zebrad_config(),
                format!("[network]\ncache_dir = {value}\n"),
            )
            .expect("config");
            assert_eq!(
                adopt(&testnet, &paths, Some(&shared)),
                PeerCache::Kept,
                "{value}"
            );
        }

        // A file that does not parse decides nothing yet.
        fs::write(files.zebrad_config(), "[network\n").expect("config");
        assert_eq!(adopt(&testnet, &paths, None), PeerCache::Undecided);

        // A decided node stays as it is.
        let decided = NodeSpec {
            peer_cache: PeerCache::Kept,
            ..testnet
        };
        assert_eq!(adopt(&decided, &paths, Some(&shared)), PeerCache::Kept);
    }

    #[test]
    fn a_node_with_an_unfinished_apply_is_decided_from_the_file_it_gets_back() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths = Paths::under(root.path().join("zeal"));
        let node = undecided(Network::Mainnet, "zebrad-mainnet-2", NodeLayout::Folder);
        let files = node.paths(&paths);
        files.ensure_dirs().expect("dirs");
        // The user removed their cache_dir in the editor, and Zeal quit before the change
        // was committed: the backup has it, the abandoned file does not.
        fs::write(
            files.zebrad_config_backup(),
            "[network]\ncache_dir = \"/srv/peers\"\n",
        )
        .expect("backup");
        fs::write(files.pending_apply_marker(), "").expect("marker");
        fs::write(files.zebrad_config(), "[network]\n").expect("config");
        assert_eq!(adopt(&node, &paths, None), PeerCache::Undecided);

        // The runtime rolls the change back and leaves the restored key alone.
        crate::runtime::prepare_files(&paths, &node);
        assert_eq!(
            config::read_peer_cache(&files).expect("read"),
            PeerCacheKey::Custom("/srv/peers".into())
        );
        // The next launch decides from it.
        assert_eq!(adopt(&node, &paths, None), PeerCache::Kept);
    }
}
