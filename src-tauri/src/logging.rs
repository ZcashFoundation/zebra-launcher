//! The launcher's own log: `launcher.<date>.log` in the log directory, rotated
//! daily, plus stderr in debug builds, and a copy of its INFO and louder
//! events for the Logs tab, through the supervisor's log ring.
//!
//! The filter for the file comes from `RUST_LOG` and defaults to `info`.
//! Writes are synchronous, so nothing is lost when the process exits right
//! after logging.
//!
//! The copy for the UI is a third layer that turns each event into a
//! [`Note`] and queues it without waiting ([`NOTE_QUEUE`] deep); [`init`]
//! returns the queue and [`forward_notes`] drains it into the supervisor once
//! that exists, so the events logged during startup are kept. Targets that
//! log per batch forwarded to the UI ([`EXCLUDED_TARGETS`]) are left out, or
//! each line would log another one.

use std::{path::Path, time::SystemTime};

use launcher_core::supervisor::{Note, SupervisorHandle};
use launcher_types::LogLevel;
use tokio::sync::mpsc;
use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
};
use tracing_appender::rolling::{Builder as RollingBuilder, Rotation};
use tracing_subscriber::{
    EnvFilter, Layer, filter, fmt,
    layer::{Context, SubscriberExt},
    util::SubscriberInitExt,
};

/// How many daily files to keep.
const KEEP_FILES: usize = 14;

/// How many launcher events wait for the supervisor, or for a busy forwarder,
/// before the newest are dropped.
pub const NOTE_QUEUE: usize = 256;

/// Targets whose events never go to the log ring, with their submodules: they
/// log per line or batch forwarded to the UI, so a note from them would log
/// again, without end. The logging module itself is listed for the same
/// reason.
pub const EXCLUDED_TARGETS: &[&str] = &["zebra_launcher::bridge", "zebra_launcher::logging"];

/// `RUST_LOG`, or `info` when it is unset or invalid.
fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
}

/// The launcher log could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    /// The log file could not be opened.
    #[error("could not open the launcher log: {0}")]
    File(#[from] tracing_appender::rolling::InitError),
    /// A global subscriber was already installed.
    #[error("could not install the log subscriber: {0}")]
    Init(#[from] tracing_subscriber::util::TryInitError),
}

/// Installs the global subscriber writing to `log_file`, whose stem and
/// extension become the prefix and suffix of the dated files, and returns
/// the queue of launcher events for the log ring (see [`forward_notes`]).
///
/// Also routes panics to the log, so a crash leaves a trace for support.
pub fn init(log_file: &Path) -> Result<mpsc::Receiver<Note>, LoggingError> {
    let dir = log_file.parent().unwrap_or_else(|| Path::new("."));
    let prefix = log_file
        .file_stem()
        .map_or_else(|| "launcher".into(), |s| s.to_string_lossy().into_owned());
    let suffix = log_file
        .extension()
        .map_or_else(|| "log".into(), |s| s.to_string_lossy().into_owned());
    let file = RollingBuilder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .filename_suffix(suffix)
        .max_log_files(KEEP_FILES)
        .build(dir)?;

    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_writer(file)
        .with_filter(filter());
    let stderr_layer = cfg!(debug_assertions).then(|| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(filter())
    });
    let (notes_tx, notes_rx) = mpsc::channel(NOTE_QUEUE);
    let notes_layer = NoteLayer { tx: notes_tx }.with_filter(filter::filter_fn(forwarded));
    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .with(notes_layer)
        .try_init()?;

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "the launcher panicked");
        previous(info);
    }));
    Ok(notes_rx)
}

/// Moves the queued launcher events into the supervisor's log ring, for the
/// rest of the process. Each one is handed over with
/// [`SupervisorHandle::note_at`], which never waits: when the actor is behind
/// or gone the event is dropped, as a log line is not worth stalling for.
pub fn forward_notes(mut notes: mpsc::Receiver<Note>, supervisor: SupervisorHandle) {
    tauri::async_runtime::spawn(async move {
        while let Some(note) = notes.recv().await {
            supervisor.note_at(note);
        }
    });
}

/// Whether an event with `meta` goes to the log ring: INFO and louder, from
/// a target outside [`EXCLUDED_TARGETS`].
pub fn forwarded(meta: &Metadata<'_>) -> bool {
    *meta.level() <= Level::INFO
        && !EXCLUDED_TARGETS.iter().any(|excluded| {
            meta.target()
                .strip_prefix(excluded)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
        })
}

/// The wire level for a `tracing` level.
fn log_level(level: Level) -> LogLevel {
    if level == Level::ERROR {
        LogLevel::Error
    } else if level == Level::WARN {
        LogLevel::Warn
    } else if level == Level::INFO {
        LogLevel::Info
    } else if level == Level::DEBUG {
        LogLevel::Debug
    } else {
        LogLevel::Trace
    }
}

/// Turns each event into a [`Note`] on the queue, without waiting.
struct NoteLayer {
    tx: mpsc::Sender<Note>,
}

impl<S: Subscriber> Layer<S> for NoteLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let mut fields = Fields::default();
        event.record(&mut fields);
        // A full queue drops the newest event: a logging call never blocks.
        let _ = self.tx.try_send(Note {
            at: SystemTime::now(),
            level: log_level(*meta.level()),
            target: meta.target().to_string(),
            message: fields.finish(),
        });
    }
}

/// Collects an event's fields the way the `Full` format prints them: the
/// message, then `name=value` pairs in declaration order.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    rest: Vec<String>,
}

impl Fields {
    fn finish(self) -> String {
        let mut out = self.message.unwrap_or_default();
        for field in self.rest {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&field);
        }
        out
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        } else {
            self.rest.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.rest.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.rest.push(format!("{}={value}", field.name()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The notes `f`'s events produce.
    fn capture(queue: usize, f: impl FnOnce()) -> Vec<Note> {
        let (tx, mut rx) = mpsc::channel(queue);
        let subscriber = tracing_subscriber::registry()
            .with(NoteLayer { tx }.with_filter(filter::filter_fn(forwarded)));
        tracing::subscriber::with_default(subscriber, f);
        let mut notes = Vec::new();
        while let Ok(note) = rx.try_recv() {
            notes.push(note);
        }
        notes
    }

    #[test]
    fn info_and_louder_events_become_notes_with_their_fields() {
        let before = SystemTime::now();
        let notes = capture(NOTE_QUEUE, || {
            tracing::info!(
                target: "zebra_launcher::updater",
                version = "0.2.0",
                "update available"
            );
            tracing::debug!(target: "zebra_launcher::updater", "too quiet to forward");
            let err = std::io::Error::other("offline");
            tracing::warn!(target: "zebra_launcher::updater", %err, attempt = 3, "update check failed");
            tracing::error!(target: "launcher_core::apply", "zebrad refused the new configuration");
            tracing::info!(target: "zebra_launcher::exit", "quitting");
        });
        assert_eq!(notes.len(), 4, "{notes:?}");
        assert_eq!(notes[0].level, LogLevel::Info);
        assert_eq!(notes[0].target, "zebra_launcher::updater");
        assert_eq!(notes[0].message, "update available version=\"0.2.0\"");
        assert!(before <= notes[0].at && notes[0].at <= SystemTime::now());
        assert_eq!(notes[1].level, LogLevel::Warn);
        assert_eq!(
            notes[1].message,
            "update check failed err=offline attempt=3"
        );
        assert_eq!(notes[2].level, LogLevel::Error);
        assert_eq!(notes[2].target, "launcher_core::apply");
        assert_eq!(notes[2].message, "zebrad refused the new configuration");
        assert_eq!(notes[3].message, "quitting");
    }

    #[test]
    fn the_bridge_and_this_module_are_left_out() {
        let notes = capture(NOTE_QUEUE, || {
            tracing::warn!(target: "zebra_launcher::bridge", "could not emit an event");
            tracing::warn!(target: "zebra_launcher::bridge::logs", "log channel closed");
            tracing::error!(target: "zebra_launcher::logging", "about itself");
            tracing::warn!(target: "zebra_launcher::bridges", "a different module");
            tracing::warn!("this test module is under logging");
        });
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].target, "zebra_launcher::bridges");
    }

    #[test]
    fn a_full_queue_drops_the_newest_without_blocking() {
        let notes = capture(1, || {
            tracing::info!(target: "zebra_launcher::x", "first");
            tracing::info!(target: "zebra_launcher::x", "second");
        });
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].message, "first");
    }

    #[test]
    fn levels_map_one_to_one() {
        assert_eq!(log_level(Level::ERROR), LogLevel::Error);
        assert_eq!(log_level(Level::WARN), LogLevel::Warn);
        assert_eq!(log_level(Level::INFO), LogLevel::Info);
        assert_eq!(log_level(Level::DEBUG), LogLevel::Debug);
        assert_eq!(log_level(Level::TRACE), LogLevel::Trace);
    }
}
