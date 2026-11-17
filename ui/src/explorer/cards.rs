//! Hover cards: a few facts about the block or transaction a link leads to, once the pointer or
//! focus rests on it. The facts are asked for after 120 ms, from one cheap read on the
//! explorer's prefetch lane, which also fills its cache for the page the link opens; the card
//! shows at 300 ms. One card at a time, drawn by the shell under the link. It stays while the
//! pointer moves onto it, and goes 300 ms after the pointer or focus leaves both the link and
//! the card, at once on Escape wherever focus is, and when anything scrolls. The facts are kept while the chain they were read on holds:
//! another network or a reorganisation drops them all, and a new tip those of transactions
//! not in a block yet. A read answered once the chain has changed may be of the old one: it is
//! not kept, and is asked for again while its hover lasts.

use std::{collections::HashMap, time::Duration};

use launcher_types::{
    Network, NodeId,
    explorer::{
        Card, CardTarget, ExplorerError, ExplorerErrorKind, ExplorerReply, ExplorerRequest,
        ExplorerResponse, TxKind, TxStatus,
    },
};
use leptos::{ev, prelude::*, task::spawn_local};
use wasm_bindgen::{JsCast, closure::Closure};

use super::text;
use crate::{
    api,
    app::AppState,
    format::{bytes, relative, thousands},
};

/// How long the pointer or focus rests before the card's facts are asked for...
const ASK_AFTER: Duration = Duration::from_millis(120);
/// ... and before the card shows...
const SHOW_AFTER: Duration = Duration::from_millis(300);
/// ... and how long it stays once the pointer or focus has left the link, so the pointer can
/// move onto it.
const HIDE_AFTER: Duration = Duration::from_millis(300);
/// The card's width, to keep it inside the window.
const CARD_WIDTH: f64 = 280.0;
/// The id the links point their `aria-describedby` at.
pub const CARD_ID: &str = "ex-hover-card";

/// The shell's hover card, for the node shown. `Copy`.
#[derive(Clone, Copy)]
pub struct HoverCards {
    node: Signal<NodeId>,
    /// The node's tip as its explorer last said: its epoch and height.
    tip: Signal<Option<(u64, u32)>>,
    /// The card shown: what about, and where (left, top) in the window.
    shown: RwSignal<Option<(CardTarget, (f64, f64))>>,
    /// The facts read, per node, on the chain the node has now.
    cards: RwSignal<HashMap<(NodeId, CardTarget), Card>>,
    /// A read that failed, with the hover it was for: it shows on that hover only, and the next
    /// one asks again.
    failed: RwSignal<Option<(u64, CardTarget, ExplorerErrorKind)>>,
    generation: StoredValue<u64>,
    /// The chain the cards are for, counted: one more at each new network, epoch or tip, so a
    /// read answered across one is not kept.
    revision: StoredValue<u64>,
    /// The pointer is on the card.
    on_card: StoredValue<bool>,
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
            tip,
            failed: RwSignal::new(None),
            on_card: StoredValue::new(false),
            shown: RwSignal::new(None),
            cards: RwSignal::new(HashMap::new()),
            generation: StoredValue::new(0),
            revision: StoredValue::new(0),
        };
        // Facts of another chain do not hold: another network, or the epoch a reorganisation
        // bumps, drops them all. At a new tip, a transaction not yet in a block may be in one.
        Effect::new(move |previous: Option<(Network, Option<(u64, u32)>)>| {
            let now = (network.get(), tip.get());
            if previous.is_some_and(|previous| previous != now) {
                cards.revision.update_value(|revision| *revision += 1);
            }
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
                let call = async move |request| api::explorer(&node, request).await;
                spawn_local(self.read(call, key, wanted, mine));
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

    /// Reads the card `key` names for the hover `mine`, with `call` (the node's explorer). An
    /// answer that comes once the cards are for another chain (a new network, epoch or tip
    /// meanwhile) may be of the old one: it is not kept, and is asked for again while the hover
    /// lasts.
    async fn read(
        self,
        call: impl AsyncFn(ExplorerRequest) -> Result<ExplorerReply, ExplorerError>,
        key: (NodeId, CardTarget),
        wanted: CardTarget,
        mine: u64,
    ) {
        loop {
            let Some(asked_on) = self.revision.try_get_value() else {
                return;
            };
            let request = ExplorerRequest::Card {
                target: wanted.clone(),
            };
            let answer = call(request).await;
            if self.revision.try_get_value() != Some(asked_on) {
                if self.generation.try_get_value() == Some(mine) {
                    continue;
                }
                return;
            }
            match answer {
                Ok(reply) => {
                    if let ExplorerResponse::Card(card) = reply.response {
                        self.cards.try_update(|cards| {
                            cards.insert(key, card);
                        });
                    }
                }
                Err(error) => {
                    if self.generation.try_get_value() == Some(mine) {
                        self.failed.try_set(Some((mine, wanted, error.kind)));
                    }
                }
            }
            return;
        }
    }

    /// The card was dismissed, or what it was under has moved: it goes now.
    pub fn leave(self) {
        self.generation.set_value(self.generation.get_value() + 1);
        self.on_card.set_value(false);
        if self.shown.with_untracked(Option::is_some) {
            self.shown.set(None);
        }
    }

    /// The pointer or focus left the link, or the pointer left the card: a card not shown yet
    /// is not shown, and one shown goes in a moment, unless the pointer is on it by then or has
    /// rested on another link.
    pub fn leave_soon(self) {
        if self.shown.with_untracked(Option::is_none) {
            self.leave();
            return;
        }
        let mine = self.generation.get_value();
        set_timeout(
            move || {
                if self.generation.try_get_value() == Some(mine)
                    && self.on_card.try_get_value() == Some(false)
                {
                    self.leave();
                }
            },
            HIDE_AFTER,
        );
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

/// The handlers a link with a hover card takes: rest and leave. `target` names what the link
/// leads to when it is entered (a table row's link changes as the table scrolls).
pub fn hover_handlers(
    target: impl Fn() -> Option<CardTarget> + Copy + 'static,
) -> (
    impl Fn(web_sys::Event) + Copy + 'static,
    impl Fn(web_sys::Event) + Copy + 'static,
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
            cards.leave_soon();
        }
    };
    (enter, leave)
}

/// The card itself, where the shell draws it: under the link, above the page.
#[component]
pub fn HoverCard() -> impl IntoView {
    let app = AppState::get();
    let Some(cards) = HoverCards::get() else {
        return ().into_any();
    };
    // Escape dismisses the card wherever focus is (WCAG 1.4.13), and the wheel moves what it
    // is under.
    let escape = window_event_listener(ev::keydown, move |event| {
        if event.key() == "Escape" && cards.shown.with_untracked(Option::is_some) {
            cards.leave();
        }
    });
    let wheel = window_event_listener(ev::wheel, move |_| cards.leave());
    on_cleanup(move || {
        escape.remove();
        wheel.remove();
    });
    // So does a scroll of anything, by keys included: scroll events do not bubble, so this one
    // listens as they go down. The Explorer is mounted once for the app's lifetime, so the
    // closure is deliberately leaked, as the theme's media-query listener is.
    if let Some(window) = web_sys::window() {
        let scrolled = Closure::<dyn Fn()>::new(move || {
            if cards.shown.try_with_untracked(Option::is_some) == Some(true) {
                cards.leave();
            }
        });
        let _ = window.add_event_listener_with_callback_and_bool(
            "scroll",
            scrolled.as_ref().unchecked_ref(),
            true,
        );
        scrolled.forget();
    }
    view! {
        {move || {
            let (target, (left, top)) = cards.shown.get()?;
            let node = cards.node.get_untracked();
            let card = cards
                .cards
                .with(|known| known.get(&(node, target.clone())).cloned());
            // A failed read shows for the hover it was made for, never the next one.
            let hover = cards.generation.try_get_value();
            let failure = cards.failed.with(|failed| {
                failed
                    .as_ref()
                    .filter(|(made_for, about, _)| Some(*made_for) == hover && *about == target)
                    .map(|(_, _, kind)| kind.clone())
            });
            let lines = match (card, failure) {
                (None, Some(kind)) => {
                    let tip = cards.tip.get_untracked().map(|(_, height)| height);
                    vec![text::card_failure_text(&target, &kind, tip)]
                }
                (None, None) => vec!["Reading from your node…".to_owned()],
                (Some(Card::Block { height, time, tx_count, size, .. }), _) => {
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
                (Some(Card::Tx { kind, status, inputs, outputs, pools, size, .. }), _) => {
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
                    on:mouseenter=move |_| cards.on_card.set_value(true)
                    on:mouseleave=move |_| {
                        cards.on_card.set_value(false);
                        cards.leave_soon();
                    }
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
    use std::{
        cell::Cell,
        pin::pin,
        task::{Context, Waker},
    };

    use launcher_types::explorer::{Hash32, TxKind};

    use super::*;

    fn tx_card(status: TxStatus) -> Card {
        Card::Tx {
            txid: Hash32([2; 32]),
            kind: TxKind::Transparent,
            status,
            inputs: 1,
            outputs: 2,
            pools: Vec::new(),
            size: Some(226),
        }
    }

    fn hover_cards() -> HoverCards {
        HoverCards {
            node: Signal::stored(NodeId::new("zebrad-mainnet").unwrap()),
            tip: Signal::stored(Some((0, 3_550_990))),
            shown: RwSignal::new(None),
            cards: RwSignal::new(HashMap::new()),
            failed: RwSignal::new(None),
            generation: StoredValue::new(1),
            revision: StoredValue::new(0),
            on_card: StoredValue::new(false),
        }
    }

    fn key() -> (NodeId, CardTarget) {
        let target = CardTarget::Tx {
            txid: Hash32([2; 32]),
        };
        (NodeId::new("zebrad-mainnet").unwrap(), target)
    }

    /// Reads the card of transaction 2 for hover 1 with `call`, which answers at once.
    fn read(
        cards: HoverCards,
        call: impl AsyncFn(ExplorerRequest) -> Result<ExplorerReply, ExplorerError>,
    ) {
        let (node, target) = key();
        let mut reading = pin!(cards.read(call, (node, target.clone()), target, 1));
        let ready = reading
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
        assert!(ready.is_ready());
    }

    #[test]
    fn a_card_answered_across_a_change_of_chain_is_asked_for_again() {
        let owner = Owner::new();
        owner.set();
        let mined = tx_card(TxStatus::Mined {
            block: Hash32([1; 32]),
            height: 3_550_990,
            time: None,
        });
        let reply = |card: &Card| {
            Ok(ExplorerReply {
                chain: None,
                response: ExplorerResponse::Card(card.clone()),
                sources: None,
            })
        };
        let card = |cards: HoverCards| {
            cards
                .cards
                .with_untracked(|known| known.get(&key()).cloned())
        };
        // A reorganisation while the node reads: the transaction it found mined is back in the
        // mempool. The first answer is not kept; the hover asks again.
        let cards = hover_cards();
        let calls = Cell::new(0);
        read(cards, async |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                cards.revision.update_value(|revision| *revision += 1);
                return reply(&mined);
            }
            reply(&tx_card(TxStatus::Mempool))
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(card(cards), Some(tx_card(TxStatus::Mempool)));
        // A failure across it is not shown either.
        let cards = hover_cards();
        let calls = Cell::new(0);
        read(cards, async |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                cards.revision.update_value(|revision| *revision += 1);
                return Err(ExplorerError {
                    kind: ExplorerErrorKind::NotFound,
                    message: "not found".into(),
                });
            }
            reply(&mined)
        });
        assert_eq!(calls.get(), 2);
        assert_eq!(cards.failed.get_untracked(), None);
        assert_eq!(card(cards), Some(mined.clone()));
        // Once the pointer has gone on, nothing is kept and nothing asked again.
        let cards = hover_cards();
        let calls = Cell::new(0);
        read(cards, async |_| {
            calls.set(calls.get() + 1);
            cards.revision.update_value(|revision| *revision += 1);
            cards.generation.set_value(2);
            reply(&mined)
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(card(cards), None);
        // On the same chain, the answer is kept.
        let cards = hover_cards();
        read(cards, async |_| reply(&mined));
        assert_eq!(card(cards), Some(mined));
    }

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
