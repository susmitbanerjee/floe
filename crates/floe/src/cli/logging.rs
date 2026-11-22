//! Log lines: tracing to stderr, as text or JSON, never with escape codes (the subscriber is
//! built without its ANSI feature).

use anyhow::Context;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

use super::args::{LogFormat, LogLevel};

/// Starts the process-wide logger. `level` is the default; RUST_LOG, when set, wins.
///
/// # Errors
///
/// If a logger was already started, which only a bug could do.
pub(crate) fn init(format: LogFormat, level: LogLevel) -> anyhow::Result<()> {
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::from(level).into())
        .from_env_lossy();
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    let started = match format {
        LogFormat::Text => builder.try_init(),
        LogFormat::Json => builder.json().try_init(),
    };
    started
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("starting the logger")
}
