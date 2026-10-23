//! The launcher's own log: `launcher.<date>.log` in the log directory, rotated
//! daily, plus stderr in debug builds.
//!
//! The filter comes from `RUST_LOG` and defaults to `info`. Writes are
//! synchronous, so nothing is lost when the process exits right after logging.

use std::path::Path;

use tracing_appender::rolling::{Builder as RollingBuilder, Rotation};
use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// How many daily files to keep.
const KEEP_FILES: usize = 14;

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
/// extension become the prefix and suffix of the dated files.
///
/// Also routes panics to the log, so a crash leaves a trace for support.
pub fn init(log_file: &Path) -> Result<(), LoggingError> {
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
    tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer)
        .try_init()?;

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "the launcher panicked");
        previous(info);
    }));
    Ok(())
}
