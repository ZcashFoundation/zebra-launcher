//! Hover cards: a few facts about the block or transaction a link leads to, once the pointer or
//! focus rests on it. The facts are asked for after 120 ms, from one cheap read on the
//! explorer's prefetch lane, which also fills its cache for the page the link opens; the card
//! shows at 300 ms. One card at a time, drawn by the shell under the link; leaving the link,
//! Escape or the wheel hides it. The facts are kept while the chain they were read on holds:
//! another network or a reorganisation drops them all, and a new tip those of transactions
//! not in a block yet.

use std::{collections::HashMap, time::Duration};

use launcher_types::{
    Network, NodeId,
    explorer::{Card, CardTarget, ExplorerRequest, ExplorerResponse, TxKind, TxStatus},
};
use leptos::{ev, prelude::*, task::spawn_local};
use wasm_bindgen::JsCast;

use super::text;
use crate::{
    api,
    app::AppState,
    format::{bytes, relative, thousands},
};

/// How long the pointer or focus rests before the card's facts are asked for...
const ASK_AFTER: Duration = Duration::from_millis(120);
/// ... and before the card shows.
const SHOW_AFTER: Duration = Duration::from_millis(300);
/// The card's width, to keep it inside the window.
const CARD_WIDTH: f64 = 280.0;
/// The id the links point their `aria-describedby` at.
pub const CARD_ID: &str = "ex-hover-card";

/// The shell's hover card, for the node shown. `Copy`.
#[derive(Clone, Copy)]
pub struct HoverCards {
    node: Signal<NodeId>,
    /// The card shown: what about, and where (left, top) in the window.
    shown: RwSignal<Option<(CardTarget, (f64, f64))>>,
    /// The facts read, per node, on the chain the node has now.
    cards: RwSignal<HashMap<(NodeId, CardTarget), Card>>,
    generation: StoredValue<u64>,
}

impl HoverCards {
    /// Gives the Explorer's links their cards, for the node `node` names, on its `network`,
    /// at its explorer's `tip` (epoch and height).
    pub fn provide(
        node: Signal<NodeId>,
        network: Signal<Network>,
        tip: Signal<Option<(u64, u32)>>,
    ) -> Self {
        let cards = HoverCards {
            node,
            shown: RwSignal::new(None),
            cards: RwSignal::new(HashMap::new()),
            generation: StoredValue::new(0),
        };
        // Facts of another chain do not hold: another network, or the epoch a reorganisation
        // bumps, drops them all. At a new tip, a transaction not yet in a block may be in one.
        Effect::new(move |previous: Option<(Network, Option<(u64, u32)>)>| {
            let now = (network.get(), tip.get());
            if let Some((was_network, was_tip)) = previous {
                let epoch = |tip: Option<(u64, u32)>| tip.map(|(epoch, _)| epoch);
                if was_network != now.0 || epoch(was_tip) != epoch(now.1) {
                    cards.cards.update(HashMap::clear);
                } else if was_tip != now.1 {
                    cards
                        .cards
                        .update(|known| known.retain(|_, card| holds_at_a_new_tip(card)));
                }
            }
            now
        });
        provide_context(cards);
        cards
    }

    /// The cards, inside the Explorer.
    pub fn get() -> Option<Self> {
        use_context()
    }

    /// The pointer or focus rests on `element`, a link to `target`.
    pub fn enter(self, target: CardTarget, element: web_sys::Element) {
        let mine = self.generation.get_value() + 1;
        self.generation.set_value(mine);
        let node = self.node.get_untracked();
        let key = (node.clone(), target.clone());
        let wanted = target.clone();
        set_timeout(
            move || {
                if self.generation.try_get_value() != Some(mine)
                    || self.cards.with_untracked(|cards| cards.contains_key(&key))
                {
                    return;
                }
                spawn_local(async move {
                    let request = ExplorerRequest::Card { target: wanted };
                    if let Ok(reply) = api::explorer(&node, request).await
                        && let ExplorerResponse::Card(card) = reply.response
                    {
                        self.cards.try_update(|cards| {
                            cards.insert(key, card);
                        });
                    }
                });
            },
            ASK_AFTER,
        );
        set_timeout(
            move || {
                if self.generation.try_get_value() != Some(mine) || !element.is_connected() {
                    return;
                }
                let rect = element.get_bounding_client_rect();
                let room = web_sys::window()
                    .and_then(|window| window.inner_width().ok())
                    .and_then(|width| width.as_f64())
                    .unwrap_or(1_200.0);
                let left = rect.left().min(room - CARD_WIDTH - 8.0).max(8.0);
                self.shown
                    .try_set(Some((target, (left, rect.bottom() + 6.0))));
            },
            SHOW_AFTER,
        );
    }

    /// The pointer or focus left the link, or the card was dismissed.
    pub fn leave(self) {
        self.generation.set_value(self.generation.get_value() + 1);
        if self.shown.with_untracked(Option::is_some) {
            self.shown.set(None);
        }
    }
}

/// Whether a card read before a new tip still holds: anything but a transaction not yet in a
/// block of the best chain.
fn holds_at_a_new_tip(card: &Card) -> bool {
    match card {
        Card::Block { .. } => true,
        Card::Tx { status, .. } => matches!(status, TxStatus::Mined { .. }),
    }
}

/// The handlers a link with a hover card takes: rest, leave and Escape. `target` names what
/// the link leads to when it is entered (a table row's link changes as the table scrolls).
pub fn hover_handlers(
    target: impl Fn() -> Option<CardTarget> + Copy + 'static,
) -> (
    impl Fn(web_sys::Event) + Copy + 'static,
    impl Fn(web_sys::Event) + Copy + 'static,
    impl Fn(web_sys::KeyboardEvent) + Copy + 'static,
) {
    let cards = HoverCards::get();
    let enter = move |event: web_sys::Event| {
        if let (Some(cards), Some(target), Some(element)) = (
            cards,
            target(),
            event
                .current_target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok()),
        ) {
            cards.enter(target, element);
        }
    };
    let leave = move |_: web_sys::Event| {
        if let Some(cards) = cards {
            cards.leave();
        }
    };
    let escape = move |event: web_sys::KeyboardEvent| {
        if event.key() == "Escape"
            && let Some(cards) = cards
        {
            cards.leave();
        }
    };
    (enter, leave, escape)
}

/// The card itself, where the shell draws it: under the link, above the page.
#[component]
pub fn HoverCard() -> impl IntoView {
    let app = AppState::get();
    let Some(cards) = HoverCards::get() else {
        return ().into_any();
    };
    let wheel = window_event_listener(ev::wheel, move |_| cards.leave());
    on_cleanup(move || wheel.remove());
    view! {
        {move || {
            let (target, (left, top)) = cards.shown.get()?;
            let node = cards.node.get_untracked();
            let card = cards
                .cards
                .with(|known| known.get(&(node, target)).cloned());
            let lines = match card {
                None => vec!["Reading from your node…".to_owned()],
                Some(Card::Block { height, time, tx_count, size, .. }) => {
                    let mined = std::time::UNIX_EPOCH + Duration::from_secs(time);
                    vec![
                        format!("Block {}", thousands(u64::from(height))),
                        format!(
                            "{} transaction{}{}",
                            thousands(u64::from(tx_count)),
                            if tx_count == 1 { "" } else { "s" },
                            size.map(|size| format!(" · {}", bytes(u64::from(size)))).unwrap_or_default()
                        ),
                        format!("mined {}", relative(mined, app.now.get())),
                    ]
                }
                Some(Card::Tx { kind, status, inputs, outputs, pools, size, .. }) => {
                    let place = match status {
                        TxStatus::Mined { height, .. } => {
                            format!("in block {}", thousands(u64::from(height)))
                        }
                        TxStatus::Mempool => "waiting in your node's mempool".to_owned(),
                        TxStatus::SideChain { .. } => "in a block outside the best chain".to_owned(),
                    };
                    let coinbase = matches!(kind, TxKind::Coinbase { .. });
                    let mut parts = text::card_counts(inputs, outputs, &pools, coinbase);
                    if let Some(size) = size {
                        parts.push(bytes(u64::from(size)));
                    }
                    vec![text::kind_label(&kind), place, parts.join(" · ")]
                }
            };
            Some(view! {
                <div
                    class="ex-hovercard"
                    role="tooltip"
                    id=CARD_ID
                    style:left=format!("{left:.0}px")
                    style:top=format!("{top:.0}px")
                >
                    {lines
                        .into_iter()
                        .enumerate()
                        .map(|(index, line)| view! {
                            <p class:ex-hovercard-title=index == 0>{line}</p>
                        })
                        .collect_view()}
                </div>
            })
        }}
    }
    .into_any()
}

#[cfg(test)]
mod tests {
    use launcher_types::explorer::{Hash32, TxKind};

    use super::*;

    #[test]
    fn a_new_tip_drops_the_cards_of_transactions_not_yet_in_a_block() {
        let block = Card::Block {
            height: 3_550_990,
            hash: Hash32([1; 32]),
            time: 1_800_000_000,
            tx_count: 12,
            size: Some(40_000),
        };
        let tx = |status| Card::Tx {
            txid: Hash32([2; 32]),
            kind: TxKind::Transparent,
            status,
            inputs: 1,
            outputs: 2,
            pools: Vec::new(),
            size: Some(226),
        };
        assert!(holds_at_a_new_tip(&block));
        assert!(holds_at_a_new_tip(&tx(TxStatus::Mined {
            block: Hash32([1; 32]),
            height: 3_550_990,
            time: None
        })));
        assert!(!holds_at_a_new_tip(&tx(TxStatus::Mempool)));
        assert!(!holds_at_a_new_tip(&tx(TxStatus::SideChain {
            block: None
        })));
    }
}
