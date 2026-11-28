//! Iceblink as a library: start it in-process, as the end-to-end suites do, or run the command
//! line ([`run_cli`]), which the `iceblink` binary does.
//!
//! Iceblink serves Apache Iceberg tables over the Postgres wire protocol, read-only.

mod app;
mod cli;
mod config;

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;

pub use app::{AppError, Deps, Started, run, start};
pub use config::{ConfigError, ValidConfig};
pub use iceblink_pg::ShutdownSummary;

/// Parses and checks iceblink.toml text, reading `env:NAME` secrets through `env`.
///
/// # Errors
///
/// The [`ConfigError`] for the first rule the text breaks.
pub fn parse_config(
    text: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ValidConfig, ConfigError> {
    config::from_toml(text, env)
}

/// Reads and checks the config file at `path`, reading secrets from the process environment.
///
/// # Errors
///
/// The [`ConfigError`] for an unreadable or oversized file, bad TOML, or a broken rule.
pub fn load_config(path: &Path) -> Result<ValidConfig, ConfigError> {
    config::Config::load(path)
}

/// The `iceblink` command: parses arguments, starts logging and the async runtime, runs the
/// command, and turns the outcome into an exit code (see `iceblink --help`).
pub fn run_cli() -> ExitCode {
    // clap prints --help and --version itself, and exits 2 on bad usage.
    let args = cli::Args::parse();
    if let Err(error) = cli::init_logging(args.log_format, args.log_level) {
        return cli::Failure::internal(&error).report();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime");
    let runtime = match runtime {
        Ok(runtime) => runtime,
        Err(error) => return cli::Failure::internal(&error).report(),
    };
    match runtime.block_on(cli::run(args.command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => failure.report(),
    }
}
