//! The rules floe.toml must follow, and the conversion into each crate's settings. Secrets are
//! resolved last, once everything else has passed.

use std::time::Duration;

use floe_catalog::{CatalogSettings, CatalogTimeouts, SigV4Service, SigV4Settings};
use floe_pg::ServerSettings;
use floe_scan::StorageSettings;
use url::{Host, Url};

use super::file::{CatalogTable, Config, ServerTable, SigV4Table, StorageTable};
use super::{ConfigError, ValidConfig};

/// How long a shutdown waits for in-flight queries before cancelling them. Not configurable
/// in phase 1.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

const MIN_STATEMENT_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_STATEMENT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Checks every rule and converts the file into the three crates' settings.
pub(super) fn validate(
    config: Config,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<ValidConfig, ConfigError> {
    let server = server_settings(&config.server)?;
    // One AWS identity reads both the catalog and its data files.
    let aws_profile = config
        .catalog
        .sigv4
        .as_ref()
        .and_then(|sigv4| sigv4.profile.clone());
    let storage = storage_settings(&config.storage, aws_profile)?;
    let catalog = catalog_settings(config.catalog, lookup)?;
    Ok(ValidConfig {
        server,
        catalog,
        storage,
    })
}

/// Rules 1 and 2: loopback only, 1 to 1024 connections, statements between 1s and 1h.
fn server_settings(table: &ServerTable) -> Result<ServerSettings, ConfigError> {
    let listen = table.pg_listen;
    if !listen.ip().is_loopback() {
        return Err(ConfigError::NotLoopback { listen });
    }
    let max_connections = usize::try_from(table.max_connections)
        .ok()
        .filter(|count| (1..=1024).contains(count))
        .ok_or_else(|| ConfigError::OutOfRange {
            key: "max_connections",
            value: table.max_connections.to_string(),
            min: "1",
            max: "1024",
        })?;
    let text = &table.statement_timeout;
    let statement_timeout = parse_duration(text).ok_or_else(|| ConfigError::InvalidDuration {
        key: "statement_timeout",
        value: text.clone(),
    })?;
    if !(MIN_STATEMENT_TIMEOUT..=MAX_STATEMENT_TIMEOUT).contains(&statement_timeout) {
        return Err(ConfigError::OutOfRange {
            key: "statement_timeout",
            value: text.clone(),
            min: "1s",
            max: "1h",
        });
    }
    Ok(ServerSettings {
        listen,
        statement_timeout,
        max_connections,
        shutdown_grace: SHUTDOWN_GRACE,
    })
}

/// Parses one whole number and a unit: `ms`, `s`, `m` or `h`. Anything else, including
/// compound forms like `1h 30m` and values that overflow, is `None`.
pub(super) fn parse_duration(text: &str) -> Option<Duration> {
    let unit_start = text.find(|c: char| !c.is_ascii_digit())?;
    let (digits, unit) = text.split_at(unit_start);
    let amount: u64 = digits.parse().ok()?;
    match unit {
        "ms" => Some(Duration::from_millis(amount)),
        "s" => Some(Duration::from_secs(amount)),
        "m" => amount.checked_mul(60).map(Duration::from_secs),
        "h" => amount.checked_mul(60 * 60).map(Duration::from_secs),
        _ => None,
    }
}

/// Rules 3, 4 and 5 for `[catalog]`. The credential is resolved only after the URI is known to
/// be https.
fn catalog_settings(
    table: CatalogTable,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<CatalogSettings, ConfigError> {
    let uri = http_url("catalog.uri", &table.uri)?;
    if table.credential.is_some() && table.sigv4.is_some() {
        return Err(ConfigError::ConflictingCatalogAuth);
    }
    let sigv4 = table.sigv4.as_ref().map(sigv4_settings).transpose()?;
    if let Some(reference) = &table.credential {
        reference.variable_name("catalog.credential")?;
    }
    let signed_by = match (&table.credential, &sigv4) {
        (Some(_), _) => Some("catalog.credential"),
        (None, Some(_)) => Some("catalog.sigv4"),
        (None, None) => None,
    };
    if let Some(setting) = signed_by.filter(|_| uri.scheme() != "https") {
        return Err(ConfigError::InvalidUrl {
            key: "catalog.uri",
            reason: format!(
                "must use https because {setting} is set, and credentials must not cross the \
                 network in plain text"
            ),
            fix: "change catalog.uri to an https:// URL".to_owned(),
        });
    }
    let credential = table
        .credential
        .as_ref()
        .map(|reference| reference.resolve("catalog.credential", lookup))
        .transpose()?;
    Ok(CatalogSettings {
        uri,
        warehouse: table.warehouse,
        credential,
        sigv4,
        timeouts: CatalogTimeouts::default(),
    })
}

/// Rule 4: a service Floe can sign for, an AWS region name, and, if given, a non-blank profile.
fn sigv4_settings(table: &SigV4Table) -> Result<SigV4Settings, ConfigError> {
    if table
        .profile
        .as_deref()
        .is_some_and(|profile| profile.trim().is_empty())
    {
        return Err(ConfigError::InvalidAwsProfile);
    }
    let service = match table.service.as_str() {
        "glue" => SigV4Service::Glue,
        "s3tables" => SigV4Service::S3Tables,
        _ => {
            return Err(ConfigError::InvalidSigV4Service {
                value: table.service.clone(),
            });
        }
    };
    let region = aws_region("catalog.sigv4.region", &table.region)?;
    Ok(SigV4Settings {
        region,
        service,
        profile: table.profile.clone(),
    })
}

/// Rule 6 for `[storage]`, plus the region and endpoint checks. S3 reads use `aws_profile`,
/// the catalog's SigV4 profile.
fn storage_settings(
    table: &StorageTable,
    aws_profile: Option<String>,
) -> Result<StorageSettings, ConfigError> {
    let region = aws_region("storage.region", &table.region)?;
    let endpoint = table
        .endpoint
        .as_deref()
        .map(|endpoint| http_url("storage.endpoint", endpoint))
        .transpose()?;
    if table.allowed_prefixes.is_empty() {
        return Err(ConfigError::NoAllowedPrefixes);
    }
    let allowed_prefixes = table
        .allowed_prefixes
        .iter()
        .map(|prefix| storage_prefix(prefix, table.allow_local_files))
        .collect::<Result<Vec<_>, _>>()?;
    if table.allow_local_files {
        tracing::warn!(
            "storage.allow_local_files is true, so Floe may read file:// locations. \
             Use it only for local tests."
        );
    }
    Ok(StorageSettings {
        endpoint,
        region,
        path_style: table.path_style,
        allowed_prefixes,
        allow_local_files: table.allow_local_files,
        aws_profile,
    })
}

/// Rule 3: an absolute https URL, or plain http to this machine only (docs/adr/0012), so
/// signed requests and table data never cross a network in plain text. The URL itself is never
/// repeated: it may carry a password.
fn http_url(key: &'static str, text: &str) -> Result<Url, ConfigError> {
    let url = match Url::parse(text) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.has_host() => url,
        _ => {
            return Err(ConfigError::InvalidUrl {
                key,
                reason: "is not an http or https URL".to_owned(),
                fix: format!(
                    "write {key} as an http or https URL with a host, such as \
                     http://127.0.0.1:8181/catalog"
                ),
            });
        }
    };
    if url.scheme() == "http" && !is_loopback(&url) {
        return Err(ConfigError::InvalidUrl {
            key,
            reason: "uses plain http to a host that isn't this machine".to_owned(),
            fix: format!(
                "change {key} to https://, or to http://127.0.0.1 when the service runs on this \
                 machine"
            ),
        });
    }
    Ok(url)
}

/// Whether `url` names this machine: a loopback address, or `localhost`.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(name)) => name == "localhost",
        None => false,
    }
}

/// An AWS region name: lowercase words joined by hyphens, ending in a number, such as
/// `us-east-1` or `us-gov-west-1`. No fixed list, because AWS keeps adding regions.
fn aws_region(key: &'static str, text: &str) -> Result<String, ConfigError> {
    let parts: Vec<&str> = text.split('-').collect();
    let is_region = parts.split_last().is_some_and(|(number, words)| {
        words.len() >= 2
            && words.first().is_some_and(|area| area.len() == 2)
            && words
                .iter()
                .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()))
            && !number.is_empty()
            && number.bytes().all(|b| b.is_ascii_digit())
    });
    if is_region {
        Ok(text.to_owned())
    } else {
        Err(ConfigError::InvalidRegion {
            key,
            value: text.to_owned(),
        })
    }
}

/// Rule 6: `s3://bucket/prefix/` ending in a slash, or `file:///path/` when local files are on.
fn storage_prefix(prefix: &str, allow_local_files: bool) -> Result<Url, ConfigError> {
    let invalid = |reason: &str| ConfigError::InvalidPrefix {
        prefix: prefix.to_owned(),
        reason: reason.to_owned(),
    };
    if let Some(path) = prefix.strip_prefix("file://") {
        if !allow_local_files {
            return Err(invalid(
                "is a local path, and file:// needs allow_local_files = true, which is only \
                 for local tests",
            ));
        }
        if !path.starts_with('/') {
            return Err(invalid(
                "must be an absolute path, such as file:///tmp/floe/",
            ));
        }
    } else if let Some(rest) = prefix.strip_prefix("s3://") {
        let bucket = rest.split('/').next().unwrap_or_default();
        if !is_bucket_name(bucket) {
            return Err(invalid(
                "names a bucket that is not a valid S3 bucket name (3 to 63 lowercase letters, \
                 digits, dots and hyphens)",
            ));
        }
    } else {
        return Err(invalid(
            "must start with s3:// (or file:// for local tests)",
        ));
    }
    if !prefix.ends_with('/') {
        return Err(invalid(
            "must end in a slash, so that s3://bucket/data/ can't also match s3://bucket/data2/",
        ));
    }
    Url::parse(prefix).map_err(|_| invalid("is not a valid URL"))
}

fn is_bucket_name(bucket: &str) -> bool {
    let bytes = bucket.as_bytes();
    (3..=63).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'.' || *b == b'-')
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
}
