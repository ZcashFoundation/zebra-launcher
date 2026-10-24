//! Helpers shared by the integration tests, included with
//! `#[path = "support/helpers.rs"] mod support;` (a file directly under
//! `tests/` would be a test target of its own).

use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};

use launcher_types::LogLine;
use tokio::time::timeout;

pub const LIMIT: Duration = Duration::from_secs(20);

pub async fn within<F: Future>(future: F) -> F::Output {
    timeout(LIMIT, future)
        .await
        .expect("operation finished within the test limit")
}

pub fn fake_zebrad() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake-zebrad"))
}

pub fn messages(lines: &[Arc<LogLine>]) -> Vec<String> {
    lines.iter().map(|l| l.message.clone()).collect()
}

pub fn contains(lines: &[Arc<LogLine>], needle: &str) -> bool {
    lines.iter().any(|l| l.message.contains(needle))
}
