//! `CatalogError`, and the one function that maps iceberg's errors onto it.

use iceberg::ErrorKind;
use secrecy::{ExposeSecret, SecretString};

use crate::names::{NamespaceName, TableName};

/// Why a catalog call failed. Display says what happened and why; [`fix`](Self::fix) says what
/// to do about it. Neither carries the credential, request headers or response bodies.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// The catalog couldn't be reached: connection refused, DNS failure or a TLS handshake error.
    #[error(
        "could not reach the catalog at {uri}: the connection was refused, or DNS or TLS failed"
    )]
    Unreachable {
        /// The catalog URI, without credentials or query string.
        uri: String,
    },
    /// The catalog didn't answer in time, after retries where the call is a read.
    #[error("{operation} timed out: the catalog didn't answer in time, even after retries")]
    Timeout {
        /// The call that timed out, such as `load_table`.
        operation: &'static str,
    },
    /// The catalog answered 401 or 403.
    #[error("HTTP {status} from {uri}: the catalog refused Iceblink's request")]
    Unauthorized {
        /// 401 or 403.
        status: u16,
        /// The catalog URI, without credentials or query string.
        uri: String,
        /// What the user should check.
        hint: &'static str,
    },
    /// No source in the AWS credential chain produced credentials.
    #[error("Iceblink found no AWS credentials in the standard chain")]
    NoAwsCredentials,
    /// The namespace or table doesn't exist.
    #[error("{what} does not exist in the catalog")]
    NotFound {
        /// What was missing, such as `table sales.orders`.
        what: String,
    },
    /// The catalog asked for something Iceblink doesn't support yet.
    #[error("{feature} is not supported yet")]
    Unsupported {
        /// What isn't supported, such as `multi-level namespace sales.eu`.
        feature: String,
    },
    /// Any other answer Iceblink didn't expect, with the credential stripped.
    #[error("the catalog gave an unexpected answer: {message}")]
    Protocol {
        /// What the catalog said: the HTTP status and iceberg's summary, never the body.
        message: String,
    },
    /// A caller built a namespace or table name Iceblink can't use.
    #[error("invalid name: {reason}")]
    InvalidName {
        /// What is wrong with the name. The name itself isn't repeated.
        reason: String,
    },
}

impl CatalogError {
    /// What a person should do about this error, as one instruction.
    pub fn fix(&self) -> String {
        match self {
            Self::Unreachable { .. } => {
                "check catalog.uri in iceblink.toml and that the catalog is running".to_owned()
            }
            Self::Timeout { .. } => {
                "check the catalog's health and the network path to it".to_owned()
            }
            Self::Unauthorized { hint, .. } => (*hint).to_owned(),
            Self::NoAwsCredentials => NO_AWS_CREDENTIALS.to_owned(),
            Self::NotFound { .. } => "check the name and catalog.warehouse".to_owned(),
            Self::Unsupported { .. } => {
                "Iceblink reads single-level namespaces from Iceberg REST catalogs; open an issue \
                 if you need more"
                    .to_owned()
            }
            Self::Protocol { .. } => "check that catalog.uri points at an Iceberg REST catalog \
                                      and that catalog.warehouse names one of its warehouses"
                .to_owned(),
            Self::InvalidName { .. } => {
                "use names of 1 to 255 bytes with no control characters".to_owned()
            }
        }
    }
}

const CHECK_CREDENTIAL: &str = "check catalog.credential: the token in its environment \
                                variable may be wrong, expired, or not allowed to read this \
                                warehouse";

const SET_CREDENTIAL: &str = "this catalog needs a token: set catalog.credential = \
                              \"env:NAME\" and put the token in the variable NAME";

const CHECK_AWS_IDENTITY: &str = "check the AWS identity Iceblink signed with (AWS_PROFILE, SSO \
                                  login, or the instance or pod role), that catalog.sigv4.region \
                                  matches the endpoint, and that its policy allows the read \
                                  actions in docs/aws-permissions.md";

const NO_AWS_CREDENTIALS: &str = "set AWS_PROFILE (and run `aws sso login` for an SSO profile), \
                                  export AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or run \
                                  with an instance or pod role";

/// What a call was about, so an error can name it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Subject<'a> {
    Catalog,
    Namespace(&'a NamespaceName),
    Table(&'a TableName),
}

/// One `LakeCatalog` call: the operation and what it was about.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Call<'a> {
    pub(crate) operation: &'static str,
    pub(crate) subject: Subject<'a>,
}

/// What the mapping needs to know about the catalog itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Context<'a> {
    pub(crate) uri: &'a str,
    pub(crate) credential: Option<&'a SecretString>,
    /// Whether requests are SigV4-signed, which changes what a 401 or 403 means.
    pub(crate) signed: bool,
}

/// Maps an iceberg error onto `CatalogError`. iceberg 0.11 keeps HTTP statuses in a private
/// context, so 401, 403, 429 and 5xx are read back from its Display text by `http_status`.
pub(crate) fn map_error(
    error: &iceberg::Error,
    call: &Call<'_>,
    context: &Context<'_>,
) -> CatalogError {
    if let Some(transport) = transport_error(error) {
        if transport.is_timeout() {
            return CatalogError::Timeout {
                operation: call.operation,
            };
        }
        if transport.is_connect() {
            return CatalogError::Unreachable {
                uri: context.uri.to_owned(),
            };
        }
    }
    match (error.kind(), http_status(error)) {
        (ErrorKind::TableNotFound, _) => CatalogError::NotFound {
            what: missing_table(call.subject),
        },
        (ErrorKind::NamespaceNotFound, _) => CatalogError::NotFound {
            what: missing_namespace(call.subject),
        },
        (ErrorKind::FeatureUnsupported, _) => CatalogError::Unsupported {
            feature: scrub(error.message(), context.credential),
        },
        (_, Some(status @ (401 | 403))) => CatalogError::Unauthorized {
            status,
            uri: context.uri.to_owned(),
            hint: match (context.signed, context.credential) {
                (true, _) => CHECK_AWS_IDENTITY,
                (false, Some(_)) => CHECK_CREDENTIAL,
                (false, None) => SET_CREDENTIAL,
            },
        },
        (_, Some(status)) => CatalogError::Protocol {
            message: format!("HTTP {status} during {}", call.operation),
        },
        (kind, None) => CatalogError::Protocol {
            message: scrub(
                &format!("{kind} during {}: {}", call.operation, error.message()),
                context.credential,
            ),
        },
    }
}

/// Reads are retried on timeouts, 429 and 5xx. Every other failure is final.
pub(crate) fn is_retryable(error: &iceberg::Error) -> bool {
    transport_error(error).is_some_and(reqwest::Error::is_timeout)
        || http_status(error).is_some_and(|status| status == 429 || (500..=599).contains(&status))
}

/// The message iceberg-catalog-rest 0.11 gives every status it doesn't handle itself.
const UNEXPECTED_STATUS: &str = "Received response with unexpected status code";

/// iceberg 0.11 keeps the status only in the error's private context, which Display renders
/// first as `Unexpected, context: { status: 503 Service Unavailable, ... }`.
fn http_status(error: &iceberg::Error) -> Option<u16> {
    if error.kind() != ErrorKind::Unexpected || error.message() != UNEXPECTED_STATUS {
        return None;
    }
    let shown = error.to_string();
    let (_, after) = shown.split_once("context: { status: ")?;
    after.get(..3)?.parse().ok()
}

/// The reqwest error underneath, if the request failed before an HTTP response arrived.
fn transport_error(error: &iceberg::Error) -> Option<&reqwest::Error> {
    let mut cause = std::error::Error::source(error);
    while let Some(current) = cause {
        if let Some(transport) = current.downcast_ref::<reqwest::Error>() {
            return Some(transport);
        }
        cause = current.source();
    }
    None
}

fn missing_table(subject: Subject<'_>) -> String {
    match subject {
        Subject::Table(table) => format!("table {table}"),
        Subject::Namespace(namespace) => format!("a table in namespace {namespace}"),
        Subject::Catalog => "the table".to_owned(),
    }
}

fn missing_namespace(subject: Subject<'_>) -> String {
    match subject {
        Subject::Table(table) => format!("namespace {}", table.namespace()),
        Subject::Namespace(namespace) => format!("namespace {namespace}"),
        Subject::Catalog => "the namespace".to_owned(),
    }
}

/// Defence in depth: no message from the catalog side may carry the credential.
fn scrub(text: &str, credential: Option<&SecretString>) -> String {
    match credential.map(ExposeSecret::expose_secret) {
        Some(secret) if !secret.is_empty() => text.replace(secret, "[REDACTED]"),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;
    use crate::CatalogTimeouts;
    use crate::http::build_client;

    const SECRET: &str = "s3cr3t-catalog-token";

    fn status_error(status: &str) -> iceberg::Error {
        iceberg::Error::new(ErrorKind::Unexpected, UNEXPECTED_STATUS)
            .with_context("status", status)
            .with_context("headers", "{}")
            .with_context("json", r#"{"error":{"message":"no","code":500}}"#)
    }

    fn map(error: &iceberg::Error, credential: Option<&SecretString>) -> CatalogError {
        let namespace = NamespaceName::new("sales").ok();
        let table = namespace.and_then(|namespace| TableName::new(namespace, "orders").ok());
        let subject = table.as_ref().map_or(Subject::Catalog, Subject::Table);
        let call = Call {
            operation: "load_table",
            subject,
        };
        let context = Context {
            uri: "http://127.0.0.1:8181/catalog",
            credential,
            signed: false,
        };
        map_error(error, &call, &context)
    }

    #[test]
    fn status_403_under_sigv4_points_at_the_aws_identity() {
        let call = Call {
            operation: "list_namespaces",
            subject: Subject::Catalog,
        };
        let context = Context {
            uri: "https://glue.ap-south-1.amazonaws.com/iceberg",
            credential: None,
            signed: true,
        };
        let mapped = map_error(&status_error("403 Forbidden"), &call, &context);
        assert!(
            matches!(mapped, CatalogError::Unauthorized { status: 403, .. }),
            "{mapped:?}"
        );
        assert_eq!(
            mapped.to_string(),
            "HTTP 403 from https://glue.ap-south-1.amazonaws.com/iceberg: the catalog refused \
             Iceblink's request"
        );
        let fix = mapped.fix();
        assert!(fix.contains("docs/aws-permissions.md"), "{fix}");
        assert!(!fix.contains("catalog.credential"), "{fix}");
    }

    #[test]
    fn table_not_found_names_the_table() {
        let error = iceberg::Error::new(ErrorKind::TableNotFound, "Tried to load a table");
        let mapped = map(&error, None);
        assert!(
            matches!(&mapped, CatalogError::NotFound { what } if what == "table sales.orders"),
            "{mapped:?}"
        );
    }

    #[test]
    fn namespace_not_found_names_the_namespace() {
        let error = iceberg::Error::new(ErrorKind::NamespaceNotFound, "no such namespace");
        let mapped = map(&error, None);
        assert!(
            matches!(&mapped, CatalogError::NotFound { what } if what == "namespace sales"),
            "{mapped:?}"
        );
    }

    #[test]
    fn feature_unsupported_is_unsupported() {
        let error = iceberg::Error::new(ErrorKind::FeatureUnsupported, "views");
        let mapped = map(&error, None);
        assert!(
            matches!(&mapped, CatalogError::Unsupported { feature } if feature == "views"),
            "{mapped:?}"
        );
    }

    #[test]
    fn status_401_and_403_are_unauthorized_and_point_at_the_credential() {
        let secret = SecretString::from(SECRET);
        for (status, credential) in [("401 Unauthorized", None), ("403 Forbidden", Some(&secret))] {
            let mapped = map(&status_error(status), credential);
            assert!(
                matches!(mapped, CatalogError::Unauthorized { .. }),
                "{status}: {mapped:?}"
            );
            assert!(
                mapped.fix().contains("catalog.credential"),
                "{}",
                mapped.fix()
            );
        }
    }

    #[test]
    fn other_statuses_are_protocol_errors_without_the_body() {
        for status in [
            "429 Too Many Requests",
            "503 Service Unavailable",
            "400 Bad Request",
        ] {
            let mapped = map(&status_error(status), None);
            let code = status.get(..3).unwrap_or_default();
            assert!(
                matches!(&mapped, CatalogError::Protocol { message } if message == &format!("HTTP {code} during load_table")),
                "{status}: {mapped:?}"
            );
        }
    }

    #[test]
    fn other_kinds_are_protocol_errors_with_the_credential_scrubbed() {
        let secret = SecretString::from(SECRET);
        let error = iceberg::Error::new(ErrorKind::DataInvalid, format!("bad token {SECRET}"));
        let mapped = map(&error, Some(&secret));
        let shown = format!("{mapped} {mapped:?}");
        assert!(
            matches!(mapped, CatalogError::Protocol { .. }),
            "{mapped:?}"
        );
        assert!(!shown.contains(SECRET), "{shown}");
        assert!(shown.contains("[REDACTED]"), "{shown}");
    }

    #[test]
    fn retries_only_429_and_5xx_statuses() {
        let cases = [
            ("429 Too Many Requests", true),
            ("500 Internal Server Error", true),
            ("503 Service Unavailable", true),
            ("401 Unauthorized", false),
            ("404 Not Found", false),
        ];
        for (status, retryable) in cases {
            assert_eq!(is_retryable(&status_error(status)), retryable, "{status}");
        }
        let not_found = iceberg::Error::new(ErrorKind::TableNotFound, "missing");
        assert!(!is_retryable(&not_found));
    }

    #[tokio::test]
    async fn refused_connections_are_unreachable() -> Result<(), Box<dyn std::error::Error>> {
        let address = TcpListener::bind("127.0.0.1:0")?.local_addr()?;
        let client = build_client(&CatalogTimeouts::default())?;
        let failure = client
            .get(format!("http://{address}/v1/config"))
            .send()
            .await;
        let error = failure.err().map(iceberg::Error::from);
        let mapped = error.as_ref().map(|error| map(error, None));
        assert!(
            matches!(&mapped, Some(CatalogError::Unreachable { uri }) if uri == "http://127.0.0.1:8181/catalog"),
            "{mapped:?}"
        );
        assert!(
            !error.as_ref().is_some_and(is_retryable),
            "refused isn't retried"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn silent_catalogs_time_out_and_are_retried() -> Result<(), Box<dyn std::error::Error>> {
        // Accepts connections (through the backlog) but never answers. With the clock paused,
        // the client's timeout fires as soon as the runtime is idle, so nothing really waits.
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let client = build_client(&CatalogTimeouts::default())?;
        let failure = client
            .get(format!("http://{address}/v1/config"))
            .send()
            .await;
        let error = failure.err().map(iceberg::Error::from);
        let mapped = error.as_ref().map(|error| map(error, None));
        assert!(
            matches!(
                mapped,
                Some(CatalogError::Timeout {
                    operation: "load_table"
                })
            ),
            "{mapped:?}"
        );
        assert!(
            error.as_ref().is_some_and(is_retryable),
            "timeouts are retried"
        );
        drop(listener);
        Ok(())
    }
}
