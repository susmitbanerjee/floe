//! Iceblink's startup and shutdown, given a checked config and a connected catalog: list the
//! catalog, bind the listener, serve until the token is cancelled, then drain.
//!
//! Tests call [`start`] and [`Started::serve`] with an iceblink-testkit catalog;
//! `iceblink serve` calls [`run`], which is the two together.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use iceblink_catalog::{CatalogError, LakeCatalog};
use iceblink_pg::{PgError, ServerSettings, ShutdownSummary, serve_on};
use iceblink_scan::{CATALOG_NAME, LakeCatalogProvider, RefreshSummary, new_session};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::cli::output;
use crate::config::ValidConfig;

#[cfg(test)]
mod tests;

/// What `run` needs besides the config: the connected catalog. Tests pass an iceblink-testkit one.
pub struct Deps {
    /// The Iceberg catalog Iceblink serves.
    pub catalog: LakeCatalog,
}

impl Deps {
    /// Connects to the catalog `config` names, as `iceblink serve` does.
    ///
    /// # Errors
    ///
    /// Whatever the catalog handshake fails with: unreachable, unauthorised or unexpected.
    pub async fn connect(config: &ValidConfig) -> Result<Self, CatalogError> {
        Ok(Self {
            catalog: LakeCatalog::connect(config.catalog.clone()).await?,
        })
    }
}

/// Why Iceblink couldn't start or keep serving.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AppError {
    /// Listing the catalog failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The Postgres listener couldn't bind.
    #[error("cannot listen on {listen}: {source}")]
    Bind {
        /// The address from pg_listen.
        listen: SocketAddr,
        /// What the operating system said.
        #[source]
        source: io::Error,
    },
    /// The Postgres server failed.
    #[error(transparent)]
    Server(#[from] PgError),
}

impl AppError {
    /// What to do about it, as one instruction.
    pub fn fix(&self) -> String {
        match self {
            Self::Catalog(error) => error.fix(),
            Self::Bind { listen, source } => bind_fix(*listen, source.kind()),
            Self::Server(_) => "see the log for details".to_owned(),
        }
    }
}

fn bind_fix(listen: SocketAddr, kind: io::ErrorKind) -> String {
    let port = listen.port();
    match kind {
        io::ErrorKind::AddrInUse => format!(
            "port {port} is already in use: stop the other server, or set pg_listen to a free \
             port in iceblink.toml"
        ),
        io::ErrorKind::PermissionDenied => format!(
            "port {port} needs extra privileges: set pg_listen to a port of 1024 or above, such \
             as 5432"
        ),
        io::ErrorKind::AddrNotAvailable => format!(
            "{} is not an address of this machine: set pg_listen to 127.0.0.1:{port}",
            listen.ip()
        ),
        _ => "check pg_listen in iceblink.toml".to_owned(),
    }
}

/// Starts Iceblink and serves until `shutdown` is cancelled.
///
/// # Errors
///
/// See [`start`] and [`Started::serve`].
pub async fn run(
    config: ValidConfig,
    deps: Deps,
    shutdown: CancellationToken,
) -> Result<ShutdownSummary, AppError> {
    start(config, deps).await?.serve(shutdown).await
}

/// Lists the catalog and binds the listener, printing a line for each, then the connection URL
/// and the psql command.
///
/// # Errors
///
/// [`AppError::Catalog`] when listing fails, [`AppError::Bind`] when the address is taken or
/// not allowed.
pub async fn start(config: ValidConfig, deps: Deps) -> Result<Started, AppError> {
    let provider = Arc::new(LakeCatalogProvider::new(
        Arc::new(deps.catalog),
        config.storage,
    ));
    let listing = Instant::now();
    let found = provider.refresh().await?;
    output::ok("catalog", &catalog_summary(found), Some(listing.elapsed()));

    let listen = config.server.listen;
    let bind_error = |source| AppError::Bind { listen, source };
    let listener = TcpListener::bind(listen).await.map_err(bind_error)?;
    let address = listener.local_addr().map_err(bind_error)?;
    output::ok("postgres", &format!("listening on {address}"), None);
    output::ready(&format!("postgres://{address}/{CATALOG_NAME}"));
    output::next(&psql_command(address));
    Ok(Started {
        listener,
        provider,
        server: config.server,
        address,
    })
}

/// A started Iceblink: the catalog listed and the listener bound.
pub struct Started {
    listener: TcpListener,
    provider: Arc<LakeCatalogProvider>,
    server: ServerSettings,
    address: SocketAddr,
}

impl Started {
    /// Where clients connect: the bound address, so a `pg_listen` port of 0 shows the real one.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Serves until `shutdown` is cancelled, waits for in-flight queries for the shutdown
    /// grace, cancels the rest, and logs one summary line.
    ///
    /// # Errors
    ///
    /// [`AppError::Server`] if the Postgres server fails.
    pub async fn serve(self, shutdown: CancellationToken) -> Result<ShutdownSummary, AppError> {
        let provider = self.provider;
        let factory = move || new_session(Arc::clone(&provider));
        tracing::debug!(address = %self.address, "serving");
        let summary = serve_on(self.listener, factory, self.server, shutdown).await?;
        tracing::info!(
            queries_drained = summary.drained,
            queries_cancelled = summary.cancelled,
            "Iceblink stopped: {summary}"
        );
        Ok(summary)
    }
}

/// `6 namespaces, 42 tables`.
pub(crate) fn catalog_summary(found: RefreshSummary) -> String {
    let plural = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    format!(
        "{}, {}",
        plural(found.namespaces, "namespace", "namespaces"),
        plural(found.tables, "table", "tables")
    )
}

/// The psql command for `address`, with `-p` only when the port isn't Postgres's 5432.
pub(crate) fn psql_command(address: SocketAddr) -> String {
    let port = match address.port() {
        5432 => String::new(),
        port => format!(" -p {port}"),
    };
    format!("psql -h {}{port} -U iceblink {CATALOG_NAME}", address.ip())
}
