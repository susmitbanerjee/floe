//! Loads iceblink.toml, Iceblink's only config file: read it, parse it, check every rule, resolve
//! `env:NAME` secrets, and hand each library crate its own settings.
//!
//! Phase 1 has no authentication, so these rules are what keep Iceblink on the local machine.

mod file;
mod secret;
mod validate;

#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use iceblink_catalog::CatalogSettings;
use iceblink_pg::ServerSettings;
use iceblink_scan::StorageSettings;

pub(crate) use file::Config;

/// Where Iceblink looks for its config unless `--config` names another file.
pub(crate) const DEFAULT_CONFIG_PATH: &str = "./iceblink.toml";

/// iceblink.toml is a few hundred bytes. Anything over 64 KiB is the wrong file.
pub(crate) const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// An iceblink.toml that passed every rule, split into the settings each crate needs.
#[derive(Debug)]
pub struct ValidConfig {
    /// The Postgres listener, for iceblink-pg.
    pub(crate) server: ServerSettings,
    /// The Iceberg REST catalog, for iceblink-catalog. Holds the resolved credential, if any.
    pub(crate) catalog: CatalogSettings,
    /// Table storage and the locations Iceblink may read, for iceblink-scan.
    pub(crate) storage: StorageSettings,
}

/// Why iceblink.toml was refused. Display says what is wrong and why;
/// [`fix`](ConfigError::fix) says what to write instead. Neither contains a secret's value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The file couldn't be opened or read, or isn't UTF-8.
    #[error("could not read {}: {source}", .path.display())]
    Read {
        /// The file Iceblink tried to read.
        path: PathBuf,
        /// What the operating system reported.
        source: io::Error,
    },
    /// The file is over 64 KiB.
    #[error("{} is larger than 64 KiB, far more than iceblink.toml needs", .path.display())]
    TooLarge {
        /// The file Iceblink tried to read.
        path: PathBuf,
    },
    /// The TOML didn't parse, a value had the wrong type, or a key is unknown (rule 7).
    #[error("iceblink.toml line {line}, column {column}: {message}")]
    Parse {
        /// 1-based line of the problem.
        line: usize,
        /// 1-based column of the problem, in characters.
        column: usize,
        /// What the parser reported. Never includes the source line.
        message: String,
    },
    /// Rule 1: `pg_listen` isn't a loopback address.
    #[error(
        "pg_listen is {listen}, but this build has no authentication, so Iceblink only listens on \
         loopback"
    )]
    NotLoopback {
        /// The address from the file.
        listen: SocketAddr,
    },
    /// Rule 2: a number or duration outside its allowed range.
    #[error("{key} is {value}, outside the allowed {min} to {max}")]
    OutOfRange {
        /// The key, as written in the file.
        key: &'static str,
        /// The value, as written in the file.
        value: String,
        /// The smallest value allowed.
        min: &'static str,
        /// The largest value allowed.
        max: &'static str,
    },
    /// Rule 2: a duration Iceblink can't read.
    #[error("{key} is \"{value}\", which is not a duration")]
    InvalidDuration {
        /// The key, as written in the file.
        key: &'static str,
        /// The value, as written in the file.
        value: String,
    },
    /// Rule 3: not an http or https URL, or http where https is required.
    #[error("{key} {reason}")]
    InvalidUrl {
        /// The key, as written in the file. The URL itself isn't repeated.
        key: &'static str,
        /// What is wrong.
        reason: String,
        /// What to write instead.
        fix: String,
    },
    /// Rule 4: a SigV4 service Iceblink can't sign for.
    #[error("catalog.sigv4.service is \"{value}\", which Iceblink can't sign for")]
    InvalidSigV4Service {
        /// The service from the file.
        value: String,
    },
    /// Rule 4: not an AWS region name.
    #[error("{key} is \"{value}\", which is not an AWS region name")]
    InvalidRegion {
        /// The key, as written in the file.
        key: &'static str,
        /// The value, as written in the file.
        value: String,
    },
    /// Rule 4: `catalog.sigv4.profile` is present but blank.
    #[error("catalog.sigv4.profile is empty")]
    InvalidAwsProfile,
    /// Both a bearer credential and SigV4 signing are configured.
    #[error(
        "catalog.credential and [catalog.sigv4] are both set, but a catalog uses one or the \
         other"
    )]
    ConflictingCatalogAuth,
    /// Rule 5: a secret written in the file instead of `env:NAME`.
    #[error("{key} looks like a plain secret, and secrets never go in iceblink.toml")]
    PlainSecret {
        /// The key holding the secret.
        key: &'static str,
    },
    /// Rule 5: `env:` followed by something that isn't a variable name. Not repeated, since it
    /// may be the secret.
    #[error(
        "{key} must be env:NAME, where NAME uses uppercase letters, digits and underscores. \
         The value isn't shown in case it is the secret itself"
    )]
    InvalidSecretReference {
        /// The key holding the reference.
        key: &'static str,
    },
    /// Rule 5: the named variable is unset or empty. Reported by name, never by value.
    #[error("{key} refers to {variable}, which is not set (or is empty)")]
    MissingSecret {
        /// The key holding the reference.
        key: &'static str,
        /// The environment variable's name.
        variable: String,
    },
    /// Rule 6: an allowed prefix that isn't `s3://bucket/prefix/`.
    #[error("storage.allowed_prefixes entry \"{prefix}\" {reason}")]
    InvalidPrefix {
        /// The prefix from the file.
        prefix: String,
        /// What is wrong with it.
        reason: String,
    },
    /// Rule 6: no allowed prefixes at all.
    #[error("storage.allowed_prefixes is empty, so Iceblink could read nothing")]
    NoAllowedPrefixes,
}

impl ConfigError {
    /// What to change, as one instruction.
    pub fn fix(&self) -> String {
        match self {
            Self::Read { .. } => "copy iceblink.example.toml to ./iceblink.toml and edit it, or \
                                  pass --config with the file's path"
                .to_owned(),
            Self::TooLarge { .. } => "check that --config points at iceblink.toml".to_owned(),
            Self::Parse { .. } => {
                "correct the TOML at that position; iceblink.example.toml shows every key"
                    .to_owned()
            }
            Self::NotLoopback { listen } => format!(
                "use pg_listen = \"127.0.0.1:{}\" (password login arrives in v0.1)",
                listen.port()
            ),
            Self::OutOfRange { key, min, max, .. } => {
                format!("set {key} between {min} and {max} in [server]")
            }
            Self::InvalidDuration { .. } => "write one whole number and a unit (ms, s, m or h), \
                                             such as \"90m\" rather than \"1h 30m\""
                .to_owned(),
            Self::InvalidUrl { fix, .. } => fix.clone(),
            Self::InvalidSigV4Service { .. } => "use \"glue\" or \"s3tables\"".to_owned(),
            Self::InvalidRegion { .. } => {
                "use a region name such as ap-south-1 or us-east-1".to_owned()
            }
            Self::InvalidAwsProfile => "name an AWS profile from ~/.aws/config, or remove the \
                                        key to follow AWS_PROFILE and the default credential \
                                        chain"
                .to_owned(),
            Self::ConflictingCatalogAuth => "keep [catalog.sigv4] for AWS endpoints (Glue, S3 \
                                             Tables) or catalog.credential for other REST \
                                             catalogs, and remove the other"
                .to_owned(),
            Self::PlainSecret { .. } => {
                "put it in an environment variable and write env:NAME".to_owned()
            }
            Self::InvalidSecretReference { .. } => {
                "write it as env:NAME, such as env:ICEBLINK_CATALOG_TOKEN".to_owned()
            }
            Self::MissingSecret { key, variable } => {
                format!("set {variable} in Iceblink's environment, or remove {key}")
            }
            Self::InvalidPrefix { .. } => {
                "write each entry as s3://bucket/prefix/, ending in a slash".to_owned()
            }
            Self::NoAllowedPrefixes => {
                "list the s3://bucket/prefix/ locations that hold your warehouse".to_owned()
            }
        }
    }
}

impl Config {
    /// Reads, parses and validates the config at `path`, resolving secrets from the process
    /// environment.
    ///
    /// # Errors
    ///
    /// Any [`ConfigError`]: unreadable or oversized file, bad TOML, or a broken rule.
    pub(crate) fn load(path: &Path) -> Result<ValidConfig, ConfigError> {
        let file = File::open(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let text = read_limited(file, path)?;
        from_toml(&text, &|name| std::env::var(name).ok())
    }
}

/// Reads at most 64 KiB and one byte, so an oversized file is refused without reading it all.
pub(crate) fn read_limited(reader: impl Read, path: &Path) -> Result<String, ConfigError> {
    let mut text = String::new();
    reader
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
    if u64::try_from(text.len()).is_ok_and(|length| length <= MAX_CONFIG_BYTES) {
        Ok(text)
    } else {
        Err(ConfigError::TooLarge {
            path: path.to_owned(),
        })
    }
}

/// Parses and validates config text, reading secrets through `lookup` so tests stay hermetic.
pub(crate) fn from_toml(
    text: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<ValidConfig, ConfigError> {
    let config: Config = toml::from_str(text).map_err(|error| parse_error(text, &error))?;
    validate::validate(config, lookup)
}

/// Keeps the parser's message and position but drops its rendering, which quotes the source
/// line and so could show a secret.
fn parse_error(text: &str, error: &toml::de::Error) -> ConfigError {
    let offset = error.span().map_or(text.len(), |span| span.start);
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last_line| last_line.chars().count())
        + 1;
    ConfigError::Parse {
        line,
        column,
        message: error.message().trim_end().to_owned(),
    }
}
