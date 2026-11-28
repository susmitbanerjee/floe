//! What a failed command tells a person and the shell: one `fail` line with a fix, and an exit
//! code from sysexits.h, so scripts and service managers can tell failures apart.

use std::process::ExitCode;

use iceblink_catalog::CatalogError;

use super::output;
use crate::app::AppError;
use crate::config::ConfigError;

/// The catalog couldn't be reached, or refused Iceblink (EX_UNAVAILABLE).
pub(crate) const EXIT_CATALOG: u8 = 69;
/// A bug in Iceblink (EX_SOFTWARE).
pub(crate) const EXIT_INTERNAL: u8 = 70;
/// The Postgres listener couldn't bind (EX_OSERR).
pub(crate) const EXIT_BIND: u8 = 71;
/// iceblink.toml is missing or invalid (EX_CONFIG).
pub(crate) const EXIT_CONFIG: u8 = 78;

/// The exit-code table at the end of `iceblink --help`. clap itself exits 2 on bad usage.
pub(crate) const EXIT_CODES_HELP: &str = "Exit codes:
  0   ok
  2   bad usage
  69  catalog unreachable or unauthorised
  70  internal error (a bug in Iceblink)
  71  cannot bind the Postgres listener
  78  config invalid";

const REPORT_BUG: &str = "this is a bug in Iceblink: rerun with --log-level debug and report it \
                          with the log";

/// A failed command: what failed, why, the fix, and the exit code.
#[derive(Debug)]
pub(crate) struct Failure {
    code: u8,
    label: &'static str,
    what: String,
    fix: String,
}

impl Failure {
    pub(crate) fn config(error: &ConfigError) -> Self {
        Self {
            code: EXIT_CONFIG,
            label: "config",
            what: error.to_string(),
            fix: error.fix(),
        }
    }

    pub(crate) fn catalog(error: &CatalogError) -> Self {
        Self {
            code: EXIT_CATALOG,
            label: "catalog",
            what: error.to_string(),
            fix: error.fix(),
        }
    }

    pub(crate) fn app(error: &AppError) -> Self {
        match error {
            AppError::Catalog(error) => Self::catalog(error),
            AppError::Bind { .. } => Self {
                code: EXIT_BIND,
                label: "postgres",
                what: error.to_string(),
                fix: error.fix(),
            },
            AppError::Server(_) => Self {
                code: EXIT_INTERNAL,
                label: "postgres",
                what: error.to_string(),
                fix: REPORT_BUG.to_owned(),
            },
        }
    }

    /// Anything else: Iceblink couldn't do something it always should be able to.
    pub(crate) fn internal(error: &anyhow::Error) -> Self {
        Self {
            code: EXIT_INTERNAL,
            label: "iceblink",
            what: format!("{error:#}"),
            fix: REPORT_BUG.to_owned(),
        }
    }

    /// The exit code this failure ends the process with.
    #[cfg(test)]
    pub(crate) fn code(&self) -> u8 {
        self.code
    }

    /// The fix line's text.
    #[cfg(test)]
    pub(crate) fn fix(&self) -> &str {
        &self.fix
    }

    /// Prints the `fail` line and its fix, and returns the exit code.
    pub(crate) fn report(&self) -> ExitCode {
        output::fail(self.label, &self.what, &self.fix);
        ExitCode::from(self.code)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::net::SocketAddr;

    use super::*;

    #[test]
    fn each_kind_of_failure_has_its_exit_code() -> Result<(), std::net::AddrParseError> {
        let listen: SocketAddr = "127.0.0.1:5432".parse()?;
        let config = Failure::config(&ConfigError::NotLoopback {
            listen: "0.0.0.0:5432".parse()?,
        });
        assert_eq!(config.code(), 78);
        assert!(config.fix().contains("127.0.0.1:5432"), "{}", config.fix());

        let catalog = Failure::catalog(&CatalogError::Unreachable {
            uri: "http://127.0.0.1:8181/catalog".to_owned(),
        });
        assert_eq!(catalog.code(), 69);

        let bind = Failure::app(&AppError::Bind {
            listen,
            source: io::Error::from(io::ErrorKind::AddrInUse),
        });
        assert_eq!(bind.code(), 71);
        assert!(bind.fix().contains("port 5432"), "{}", bind.fix());

        let internal = Failure::internal(&anyhow::anyhow!("no runtime"));
        assert_eq!(internal.code(), 70);
        Ok(())
    }

    #[test]
    fn the_help_text_lists_every_exit_code() {
        for code in [0, 2, EXIT_CATALOG, EXIT_INTERNAL, EXIT_BIND, EXIT_CONFIG] {
            assert!(
                EXIT_CODES_HELP.contains(&format!("\n  {code} ")),
                "{code} is missing from --help"
            );
        }
    }
}
