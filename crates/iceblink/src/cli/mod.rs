//! The command-line glue: each command's steps, in order, with one line per step and a
//! [`Failure`] (exit code, what, fix) when a step fails. anyhow is allowed here and in
//! `main.rs` only.

mod args;
mod failure;
mod logging;
pub(crate) mod output;
mod signals;

use std::path::Path;

use anyhow::Context;
use tokio_util::sync::CancellationToken;

use crate::app::{self, Deps};
use crate::config::{Config, DEFAULT_CONFIG_PATH, ValidConfig};
pub(crate) use args::{Args, Command, ConfigCommand};
pub(crate) use failure::Failure;
pub(crate) use logging::init as init_logging;
use signals::Signals;

/// Runs `command` to the end.
///
/// # Errors
///
/// The [`Failure`] to report: its line and exit code.
pub(crate) async fn run(command: Command) -> Result<(), Failure> {
    match command {
        Command::Serve(file) => serve(&file.config).await,
        Command::Config(ConfigCommand::Check(file)) => check(&file.config),
    }
}

/// `floe config check`: read and check the file, touching nothing else.
fn check(path: &Path) -> Result<(), Failure> {
    load(path)?;
    output::next(&serve_command(path));
    Ok(())
}

/// `floe serve`: config, catalog, listener, then serve until a signal, and drain.
async fn serve(path: &Path) -> Result<(), Failure> {
    let config = load(path)?;
    tracing::info!(
        listen = %config.server.listen,
        // The host only: the full URI may carry a password.
        catalog_host = config.catalog.uri.host_str().unwrap_or_default(),
        warehouse = %config.catalog.warehouse,
        region = %config.storage.region,
        "configuration loaded"
    );
    let deps = Deps::connect(&config)
        .await
        .map_err(|error| Failure::catalog(&error))?;
    let signals = Signals::install()
        .context("listening for shutdown signals")
        .map_err(|error| Failure::internal(&error))?;
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(signals.cancel_on_signal(shutdown.clone()));
    let served = app::run(config, deps, shutdown.clone()).await;
    // Stops the signal watcher if the server ended on its own.
    shutdown.cancel();
    if let Err(error) = watcher.await {
        tracing::warn!(%error, "the signal watcher stopped unexpectedly");
    }
    served.map(drop).map_err(|error| Failure::app(&error))
}

/// Reads and checks the config, and says so.
fn load(path: &Path) -> Result<ValidConfig, Failure> {
    let config = Config::load(path).map_err(|error| Failure::config(&error))?;
    output::ok("config", &path.display().to_string(), None);
    Ok(config)
}

/// `floe serve`, with `--config` when the file isn't the default.
fn serve_command(path: &Path) -> String {
    if path == Path::new(DEFAULT_CONFIG_PATH) {
        "floe serve".to_owned()
    } else {
        format!("floe serve --config {}", path.display())
    }
}
