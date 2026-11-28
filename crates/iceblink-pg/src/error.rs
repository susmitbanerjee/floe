//! Everything that can go wrong while serving Postgres clients, said the way a person reads it.

use std::net::SocketAddr;

use iceblink_scan::ScanError;

/// The settings `SET` accepts, for the message that refuses the rest.
pub(crate) const SETTABLE: &str = "search_path, statement_timeout, application_name, \
                                   client_encoding, DateStyle, extra_float_digits, TimeZone";

/// What went wrong while serving Postgres clients. Clients see these with a matching SQLSTATE.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PgError {
    /// The listener couldn't bind its address.
    #[error("couldn't listen on {listen}: {source}; is another server using that port?")]
    Bind {
        /// The address from the config.
        listen: SocketAddr,
        /// Why the bind failed.
        #[source]
        source: std::io::Error,
    },
    /// The session factory couldn't build a session for a new connection.
    #[error("couldn't build a session for a new connection: {0}")]
    Session(#[from] ScanError),
    /// A new connection's session couldn't be finished, such as its pg_catalog schema.
    #[error("couldn't set up the session for a new connection: {message}")]
    SessionSetup {
        /// What went wrong.
        message: String,
    },
    /// The statement would change something, or isn't on the allow-list.
    #[error("Iceblink is read-only: {kind} is not allowed.")]
    ReadOnly {
        /// The statement kind, such as `CREATE TABLE`.
        kind: String,
    },
    /// `SET` of a setting Iceblink doesn't support.
    #[error("Iceblink doesn't support setting {name}; it can set {SETTABLE}")]
    UnsupportedSetting {
        /// The setting the client tried to change.
        name: String,
    },
    /// A client encoding other than UTF8.
    #[error("client_encoding {encoding} isn't supported; Iceblink speaks only UTF8")]
    UnsupportedEncoding {
        /// The encoding the client asked for.
        encoding: String,
    },
    /// `SET statement_timeout` with a value Iceblink can't read.
    #[error(
        "invalid value for statement_timeout: {value}; use milliseconds, or a number with a \
         unit such as '5s' or '1min'"
    )]
    InvalidTimeout {
        /// The value as the client wrote it.
        value: String,
    },
    /// `SET statement_timeout` to more than the server's limit, or to 0 (no limit).
    #[error("statement_timeout can only be lowered: the server limit is {limit}")]
    TimeoutAboveLimit {
        /// The server's limit, in Postgres notation such as `30s`.
        limit: String,
    },
    /// A schema that isn't one of the lake's namespaces.
    #[error("schema \"{name}\" does not exist")]
    UnknownSchema {
        /// The schema name as the client wrote it.
        name: String,
    },
    /// The statement ran past its deadline.
    #[error("canceling statement due to statement timeout")]
    StatementTimeout,
    /// Every connection slot is taken.
    #[error("too many connections (limit {limit}); close an idle session or try again later")]
    TooManyConnections {
        /// The configured `max_connections`.
        limit: usize,
    },
    /// A new statement arrived while the server drains for shutdown.
    #[error("terminating connection: Iceblink is shutting down")]
    ShuttingDown,
    /// A new connection arrived while the server drains for shutdown.
    #[error("Iceblink is shutting down and accepts no new connections; try again when it is back")]
    NotAcceptingConnections,
}
