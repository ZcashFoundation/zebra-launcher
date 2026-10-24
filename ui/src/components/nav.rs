//! The header row: the product name, the tab bar, the network and the launcher version.

use launcher_types::Network;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

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
                        // The ARIA tabs pattern: Tab reaches the active tab only (roving
                        // tabindex); the arrow keys, Home and End move within the bar and show
                        // the tab they land on.
                        let keydown = move |event: web_sys::KeyboardEvent| {
                            let Some(next) = tab_for_key(tab, &event.key()) else {
                                return;
                            };
                            event.prevent_default();
                            state.tab.set(next);
                            focus_tab(next);
                        };
                        view! {
                            <button
                                role="tab"
                                id=tab.button_id()
                                class="tab"
                                class:active=selected
                                aria-selected=move || selected().to_string()
                                aria-controls=tab.page_id()
                                tabindex=move || if selected() { "0" } else { "-1" }
                                title=shortcut_label(tab.number(), mac)
                                on:click=move |_| state.tab.set(tab)
                                on:keydown=keydown
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
                                // The label is read out but not shown: the pill speaks for
                                // itself visually, and `title` only reaches mouse users.
                                <span class=network_class(network) title="The network zebrad runs on">
                                    <span class="visually-hidden">"Network: "</span>
                                    {network.as_str()}
                                </span>
                            }
                        })
                }}
                {move || {
                    version()
                        .map(|version| {
                            view! {
                                // Tells the launcher's version apart from zebrad's in the strip.
                                <span class="launcher-version muted" title="Zebra Launcher version">
                                    <span class="visually-hidden">"Launcher version "</span>
                                    {version}
                                </span>
                            }
                        })
                }}
            </div>
        </div>
    }
}

/// The tab a key moves to from `current` in the tab bar: the neighbours for the arrow keys
/// (wrapping around), the first and last tab for Home and End; `None` for any other key.
fn tab_for_key(current: Tab, key: &str) -> Option<Tab> {
    Some(match key {
        "ArrowRight" => current.step(1),
        "ArrowLeft" => current.step(-1),
        "Home" => Tab::ALL[0],
        "End" => Tab::ALL[Tab::ALL.len() - 1],
        _ => return None,
    })
}

/// Moves keyboard focus to `tab`'s button.
fn focus_tab(tab: Tab) {
    let button = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(&tab.button_id()))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(button) = button {
        let _ = button.focus();
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
    fn arrow_keys_step_and_home_end_jump() {
        assert_eq!(tab_for_key(Tab::Dashboard, "ArrowRight"), Some(Tab::Logs));
        assert_eq!(tab_for_key(Tab::Logs, "ArrowLeft"), Some(Tab::Dashboard));
        // From the ends, the arrows wrap around the bar.
        assert_eq!(
            tab_for_key(Tab::Settings, "ArrowRight"),
            Some(Tab::Dashboard)
        );
        assert_eq!(
            tab_for_key(Tab::Dashboard, "ArrowLeft"),
            Some(Tab::Settings)
        );
        assert_eq!(tab_for_key(Tab::Grafana, "Home"), Some(Tab::Dashboard));
        assert_eq!(tab_for_key(Tab::Grafana, "End"), Some(Tab::Settings));
        // Other keys are left to the browser (Enter and Space activate the button).
        assert_eq!(tab_for_key(Tab::Grafana, "ArrowDown"), None);
        assert_eq!(tab_for_key(Tab::Grafana, "Enter"), None);
        assert_eq!(tab_for_key(Tab::Grafana, "Tab"), None);
    }

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
