//! The header row: the product name, the tab bar, the network and the launcher version.

use launcher_types::Network;
use leptos::prelude::*;

use crate::{
    app::{AppState, Badge, Tab},
    format::{shortcut_label, version_tag},
};

/// True on macOS, where the tab shortcuts use the Command key.
fn is_mac() -> bool {
    web_sys::window()
        .and_then(|window| window.navigator().platform().ok())
        .is_some_and(|platform| platform.contains("Mac"))
}

#[component]
pub fn Nav() -> impl IntoView {
    let state = AppState::get();
    let mac = is_mac();
    let network = move || {
        state
            .settings
            .with(|settings| settings.as_ref().map(|s| s.network))
    };
    let version = move || {
        state
            .update
            .with(|info| info.as_ref().map(|info| version_tag(&info.current_version)))
    };
    view! {
        <div class="nav-bar">
            <div class="brand">
                <img class="brand-icon" src="/32x32.png" alt="" width="32" height="32" />
                <span class="brand-name">"Zebra Launcher"</span>
            </div>
            <nav class="tabs" role="tablist" aria-label="Pages">
                {Tab::ALL
                    .into_iter()
                    .map(|tab| {
                        let selected = move || state.tab.get() == tab;
                        view! {
                            <button
                                role="tab"
                                class="tab"
                                class:active=selected
                                aria-selected=move || selected().to_string()
                                title=shortcut_label(tab.number(), mac)
                                on:click=move |_| state.tab.set(tab)
                            >
                                {tab.label()}
                                {move || {
                                    tab.badge(&state)
                                        .map(|badge| {
                                            view! {
                                                <span
                                                    class=badge_class(badge)
                                                    role="img"
                                                    aria-label=badge.label()
                                                >
                                                    {badge.text()}
                                                </span>
                                            }
                                        })
                                }}
                            </button>
                        }
                    })
                    .collect_view()}
            </nav>
            <div class="nav-meta">
                {move || {
                    network()
                        .map(|network| {
                            view! {
                                <span class=network_class(network) title="The network zebrad runs on">
                                    {network.as_str()}
                                </span>
                            }
                        })
                }}
                {move || {
                    version()
                        .map(|version| {
                            view! {
                                <span class="launcher-version muted" title="Zebra Launcher version">
                                    {version}
                                </span>
                            }
                        })
                }}
            </div>
        </div>
    }
}

/// The classes of a tab badge: a dot, or a pill with a number.
fn badge_class(badge: Badge) -> &'static str {
    match badge {
        Badge::Dot => "tab-badge tab-badge-dot",
        Badge::Count(_) => "tab-badge tab-badge-count",
    }
}

/// The classes of the network pill: Testnet stands out in the warning tone.
fn network_class(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "network-pill network-mainnet",
        Network::Testnet => "network-pill network-testnet",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_classes() {
        assert_eq!(
            network_class(Network::Testnet),
            "network-pill network-testnet"
        );
        assert_eq!(badge_class(Badge::Dot), "tab-badge tab-badge-dot");
        assert_eq!(badge_class(Badge::Count(2)), "tab-badge tab-badge-count");
    }
}
