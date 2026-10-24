//! A value shown for a while: a copy note, a "Saved" notice, a flashed line.
//!
//! Every page used to keep a generation counter beside a signal and a `set_timeout` that
//! cleared the signal only if nothing newer had been shown since. [`Transient`] holds that
//! rule once. The value and its generation live in one signal, so a page disposed while a
//! timer runs is no trouble: the timer finds the signal gone and does nothing.
//!
//! Used by the copy notes on the Logs and Grafana pages. The second phase moves the config
//! editor's notices (`show` for `Notice::Saved`, `hold` for the rest, in place of
//! `show_notice` and `SAVED_FOR`), the Configure page's line flash (`show_for(number,
//! FLASH_FOR)`) and the Settings outcomes (`report`) onto it.

use std::time::Duration;

use leptos::prelude::*;

/// How long a transient value stays unless the caller says otherwise: enough to read a
/// sentence. The one policy for success notes across the pages.
pub const DEFAULT_DURATION: Duration = Duration::from_secs(5);

/// A signal whose value clears itself after a while. `Copy` for any `T`, as the signal is.
pub struct Transient<T: Send + Sync + 'static> {
    /// The value with the generation that set it. A timer clears the value only while the
    /// generation is still its own.
    inner: RwSignal<(u64, Option<T>)>,
}

impl<T: Send + Sync + 'static> Transient<T> {
    pub fn new() -> Self {
        Self {
            inner: RwSignal::new((0, None)),
        }
    }

    /// Shows `value` for [`DEFAULT_DURATION`].
    pub fn show(self, value: T) {
        self.show_for(value, DEFAULT_DURATION);
    }

    /// Shows `value` for `duration`, then clears it unless something newer was shown since.
    pub fn show_for(self, value: T, duration: Duration) {
        let Some(mine) = self.replace(Some(value)) else {
            return;
        };
        set_timeout(
            move || {
                let still_mine = self
                    .inner
                    .try_with_untracked(|(generation, _)| *generation == mine);
                if still_mine == Some(true) {
                    self.inner.try_update(|(_, current)| *current = None);
                }
            },
            duration,
        );
    }

    /// Shows `value` until it is cleared or replaced: an error that must be read.
    pub fn hold(self, value: T) {
        self.replace(Some(value));
    }

    /// Clears the value now: the Settings form drops its "Saved" on the next edit.
    #[expect(dead_code, reason = "adopted by the Settings page in the second phase")]
    pub fn clear(self) {
        self.replace(None);
    }

    /// Stores `value` under a new generation, which an earlier timer no longer matches.
    /// `None` when the signal is disposed.
    fn replace(self, value: Option<T>) -> Option<u64> {
        self.inner.try_update(|(generation, current)| {
            *generation += 1;
            *current = value;
            *generation
        })
    }

    /// The value, tracked.
    pub fn get(self) -> Option<T>
    where
        T: Clone,
    {
        self.inner.with(|(_, value)| value.clone())
    }

    /// The value as a signal, for a component prop.
    pub fn signal(self) -> Signal<Option<T>>
    where
        T: Clone,
    {
        Signal::derive(move || self.get())
    }
}

impl<T: Send + Sync + 'static, E: Send + Sync + 'static> Transient<Result<T, E>> {
    /// Shows a success for the default time and holds a failure until the next outcome.
    pub fn report(self, outcome: Result<T, E>) {
        if outcome.is_ok() {
            self.show(outcome);
        } else {
            self.hold(outcome);
        }
    }
}

impl<T: Send + Sync + 'static> Default for Transient<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Send + Sync + 'static> Clone for Transient<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Send + Sync + 'static> Copy for Transient<T> {}
