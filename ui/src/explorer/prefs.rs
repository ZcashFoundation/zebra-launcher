//! The Explorer's display choices: amounts in the network's coin or in zatoshis, times in the
//! local zone, in UTC or as Unix seconds, and whether pages list the reads they took (Show
//! RPC). They apply to every node's Explorer and are kept in `localStorage`, as the Grafana
//! page keeps its pickers; where storage is unavailable they last the session.

use launcher_types::Network;
use leptos::prelude::*;

use super::text::{TimeFormat, Unit};

/// `localStorage` keys.
const STORED_UNIT: &str = "explorer.unit";
const STORED_TIME: &str = "explorer.time";
const STORED_RPC: &str = "explorer.rpc";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// A remembered choice, `None` when storage is unavailable or has none.
fn remembered(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

/// Remembers a choice. Failures (storage disabled or full) are ignored: the choice still
/// applies for this session.
fn remember(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

/// The choices, as signals the pages read. `Copy`.
#[derive(Clone, Copy, Debug)]
pub struct Prefs {
    /// Amounts in zatoshis rather than the network's coin.
    pub zat: RwSignal<bool>,
    pub time: RwSignal<TimeFormat>,
    /// Show RPC: pages list the reads their answers took.
    pub rpc: RwSignal<bool>,
}

impl Prefs {
    /// Reads the remembered choices and gives them to the Explorer's pages.
    pub fn provide() -> Self {
        let prefs = Prefs {
            zat: RwSignal::new(remembered(STORED_UNIT).as_deref() == Some("zat")),
            time: RwSignal::new(
                remembered(STORED_TIME)
                    .as_deref()
                    .and_then(TimeFormat::from_id)
                    .unwrap_or_default(),
            ),
            rpc: RwSignal::new(remembered(STORED_RPC).as_deref() == Some("on")),
        };
        provide_context(prefs);
        prefs
    }

    /// The choices, inside the Explorer.
    pub fn get() -> Self {
        expect_context()
    }

    /// The unit amounts read in on `network`, tracked.
    pub fn unit(self, network: Network) -> Unit {
        Unit::new(network, self.zat.get())
    }

    pub fn set_zat(self, zat: bool) {
        self.zat.set(zat);
        remember(STORED_UNIT, if zat { "zat" } else { "coin" });
    }

    pub fn set_time(self, time: TimeFormat) {
        self.time.set(time);
        remember(STORED_TIME, time.id());
    }

    pub fn set_rpc(self, on: bool) {
        self.rpc.set(on);
        remember(STORED_RPC, if on { "on" } else { "off" });
    }
}
