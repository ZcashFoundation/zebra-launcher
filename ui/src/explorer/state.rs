//! Each node's Explorer: where it is, where it has been and what its explorer publishes, kept
//! while the tab is not shown, so a node keeps its place. And the loader the pages read their
//! answers with.

use std::collections::HashMap;

use launcher_types::{
    NodeId,
    explorer::{
        ExplorerError, ExplorerLive, ExplorerReply, ExplorerRequest, ExplorerRoute, LiveState,
    },
};
use leptos::{prelude::*, task::spawn_local};

use super::history::NavHistory;
use crate::api;

/// One node's Explorer. `Copy`: every field is a signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeExplorer {
    pub history: RwSignal<NavHistory>,
    /// The explorer's own news: its tip, whether the node syncs, the last reorganisation.
    pub live: RwSignal<Option<ExplorerLive>>,
    /// Why the explorer would not say how it is, until it publishes its news: the node's
    /// zebrad runs on the other network until its next start, say.
    pub unavailable: RwSignal<Option<ExplorerError>>,
}

impl NodeExplorer {
    /// The route shown, tracked.
    pub fn route(&self) -> ExplorerRoute {
        self.history.with(|history| history.current().clone())
    }

    /// Goes to `route`.
    pub fn go(&self, route: ExplorerRoute) {
        self.history.update(|history| history.push(route));
    }

    /// Shows `route` in place of the current one, so going back skips it.
    pub fn replace(&self, route: ExplorerRoute) {
        self.history.update(|history| history.replace(route));
    }

    pub fn back(&self) {
        self.history.update(|history| {
            history.back();
        });
    }

    pub fn forward(&self) {
        self.history.update(|history| {
            history.forward();
        });
    }
}

/// Every node's Explorer, made on first use and owned by the Explorer tab, so switching nodes
/// keeps each one's place.
#[derive(Clone, Copy)]
pub struct Explorers {
    owner: StoredValue<Owner>,
    by_node: StoredValue<HashMap<NodeId, NodeExplorer>>,
}

impl Explorers {
    /// Made where the Explorer tab is: its signals live as long as the tab.
    pub fn new() -> Self {
        Self {
            owner: StoredValue::new(Owner::current().unwrap_or_default()),
            by_node: StoredValue::new(HashMap::new()),
        }
    }

    /// `node`'s Explorer.
    pub fn of(&self, node: &NodeId) -> NodeExplorer {
        if let Some(found) = self.by_node.with_value(|map| map.get(node).copied()) {
            return found;
        }
        let made = self.owner.with_value(|owner| {
            owner.with(|| NodeExplorer {
                history: RwSignal::new(NavHistory::default()),
                live: RwSignal::new(None),
                unavailable: RwSignal::new(None),
            })
        });
        self.by_node.update_value(|map| {
            map.insert(node.clone(), made);
        });
        made
    }
}

/// A page's answer: on its way, arrived, or failed.
#[derive(Clone, Debug, PartialEq)]
pub enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(ExplorerError),
}

/// A page's answer and whether a newer one is on its way, while the older one stays on screen.
#[derive(Clone, Copy)]
pub struct Load {
    pub value: RwSignal<Loaded<ExplorerReply>>,
    pub refreshing: RwSignal<bool>,
}

/// Asks `node`'s explorer for `request()` now and again whenever its tracked inputs change the
/// request or its revision (a page that follows the tip passes the tip as the revision), and
/// when the node becomes reachable or stops being so. An answer to an older request is
/// dropped. `None` asks nothing.
pub fn load(
    node: NodeId,
    nav: NodeExplorer,
    request: impl Fn() -> Option<(ExplorerRequest, u64)> + 'static,
) -> Load {
    let load = Load {
        value: RwSignal::new(Loaded::Loading),
        refreshing: RwSignal::new(false),
    };
    let generation = StoredValue::new(0u64);
    Effect::new(
        move |previous: Option<(bool, Option<(ExplorerRequest, u64)>)>| {
            let reachable = nav.live.with(|live| {
                live.as_ref()
                    .is_some_and(|live| live.state != LiveState::NotRunning)
            });
            let next = (reachable, request());
            if previous.as_ref() == Some(&next) {
                return next;
            }
            let mine = generation.get_value() + 1;
            generation.set_value(mine);
            if let (_, Some((request, _))) = next.clone() {
                if matches!(load.value.get_untracked(), Loaded::Ready(_)) {
                    load.refreshing.set(true);
                } else {
                    load.value.set(Loaded::Loading);
                }
                let node = node.clone();
                spawn_local(async move {
                    let result = api::explorer(&node, request).await;
                    if generation.try_get_value() != Some(mine) {
                        return;
                    }
                    // The page may be gone by now: then there is nothing to set.
                    load.refreshing.try_set(false);
                    load.value.try_set(match result {
                        Ok(reply) => Loaded::Ready(reply),
                        Err(error) => Loaded::Failed(error),
                    });
                });
            }
            next
        },
    );
    load
}
