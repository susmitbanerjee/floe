//! The command line: `floe serve`, `floe config check`, and the global logging flags.

use std::path::PathBuf;

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use tracing::level_filters::LevelFilter;

use super::failure::EXIT_CODES_HELP;
use crate::config::DEFAULT_CONFIG_PATH;

/// Everything `floe` accepts on its command line.
#[derive(Debug, Parser)]
#[command(name = "floe", version, about, after_help = EXIT_CODES_HELP)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Command,
    /// Log line format. Logs go to stderr.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    pub(crate) log_format: LogFormat,
    /// Least important logs shown. RUST_LOG, when set, wins.
    #[arg(long, global = true, value_enum, default_value_t = LogLevel::Info)]
    pub(crate) log_level: LogLevel,
}

/// What `floe` was asked to do.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Serve the catalog's tables to Postgres clients until Ctrl-C or SIGTERM.
    Serve(ConfigFile),
    /// Work with floe.toml.
    #[command(subcommand)]
    Config(ConfigCommand),
}

/// `floe config …`.
#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Check floe.toml without connecting to anything.
    Check(ConfigFile),
}

/// Which config file to read.
#[derive(Debug, ClapArgs)]
pub(crate) struct ConfigFile {
    /// The config file.
    #[arg(long, value_name = "PATH", default_value = DEFAULT_CONFIG_PATH)]
    pub(crate) config: PathBuf,
}

/// How log lines are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LogFormat {
    /// One readable line per event.
    Text,
    /// One JSON object per event, for log collectors.
    Json,
}

/// The least important logs shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum LogLevel {
    /// Only what needs a person.
    Error,
    /// Also degraded but working.
    Warn,
    /// Also lifecycle: startup, shutdown, one line per statement.
    Info,
    /// Also detail, including full SQL.
    Debug,
    /// Everything.
    Trace,
}

impl From<LogLevel> for LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::ERROR,
            LogLevel::Warn => Self::WARN,
            LogLevel::Info => Self::INFO,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Trace => Self::TRACE,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_tree_is_consistent() {
        Args::command().debug_assert();
    }

    #[test]
    fn defaults_to_floe_toml_text_logs_and_info() -> Result<(), clap::Error> {
        let args = Args::try_parse_from(["floe", "serve"])?;
        assert_eq!(args.log_format, LogFormat::Text);
        assert_eq!(args.log_level, LogLevel::Info);
        let Command::Serve(file) = args.command else {
            return Err(clap::Error::new(clap::error::ErrorKind::InvalidSubcommand));
        };
        assert_eq!(file.config, PathBuf::from("./floe.toml"));
        Ok(())
    }

    #[test]
    fn config_check_takes_a_path_and_global_flags_go_anywhere() -> Result<(), clap::Error> {
        let args = Args::try_parse_from([
            "floe",
            "config",
            "check",
            "--config",
            "/etc/floe/floe.toml",
            "--log-format",
            "json",
        ])?;
        assert_eq!(args.log_format, LogFormat::Json);
        let Command::Config(ConfigCommand::Check(file)) = args.command else {
            return Err(clap::Error::new(clap::error::ErrorKind::InvalidSubcommand));
        };
        assert_eq!(file.config, PathBuf::from("/etc/floe/floe.toml"));
        Ok(())
    }
}
