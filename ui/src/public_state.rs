//! The public services' status in the UI: one signal, fed by the `public_status` event and a
//! snapshot in the shell's order (subscribe first, then fetch, and keep the snapshot only when
//! no event came first).

use launcher_types::public::PublicStatus;
use leptos::{prelude::*, task::spawn_local};

use crate::api;

/// Provided as context by the shell.
#[derive(Clone, Copy)]
pub struct PublicContext {
    /// One entry per service set up; empty while none is.
    pub statuses: RwSignal<Vec<PublicStatus>>,
    /// The last error from loading or a command.
    pub error: RwSignal<Option<String>>,
}

impl PublicContext {
    pub fn get() -> Self {
        expect_context()
    }

    /// Creates the context, subscribes to the event and fetches the snapshot.
    pub fn provide() {
        let context = PublicContext {
            statuses: RwSignal::new(Vec::new()),
            error: RwSignal::new(None),
        };
        provide_context(context);
        spawn_local(async move {
            let seen = StoredValue::new(false);
            let result = api::public::on_public_status(
                move |status| {
                    seen.set_value(true);
                    context.record(status);
                },
                move |message| context.error.set(Some(message)),
            )
            .await;
            if let Err(error) = result {
                context
                    .error
                    .set(Some(format!("Listening for public services: {error}")));
            }
            match api::public::get_public_status().await {
                Ok(statuses) if !seen.get_value() => context.statuses.set(statuses),
                Ok(_) => {}
                Err(error) => context
                    .error
                    .set(Some(format!("Reading public services: {error}"))),
            }
        });
    }

    /// An event: the service's status, replacing the one for the same node.
    fn record(self, status: PublicStatus) {
        self.statuses.update(|statuses| {
            match statuses.iter_mut().find(|old| old.node == status.node) {
                Some(old) => *old = status,
                None => statuses.push(status),
            }
        });
    }

    /// Fetches the snapshot again, after a command that may remove a service.
    pub fn refresh(self) {
        spawn_local(async move {
            match api::public::get_public_status().await {
                Ok(statuses) => self.statuses.set(statuses),
                Err(error) => self.error.set(Some(error.to_string())),
            }
        });
    }
}
