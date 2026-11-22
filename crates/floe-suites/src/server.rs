//! Floe started in-process, the way `floe serve` starts it, over any Iceberg catalog.

use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

use floe::{AppError, Deps, ShutdownSummary, ValidConfig};
use floe_catalog::{Catalog, LakeCatalog};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};
use tokio_util::sync::CancellationToken;

/// A suite's boxed error.
pub type SuiteError = Box<dyn Error + Send + Sync>;

/// A running Floe on 127.0.0.1 and an OS-chosen port.
pub struct InProcess {
    address: SocketAddr,
    shutdown: CancellationToken,
    serving: JoinHandle<Result<ShutdownSummary, AppError>>,
}

impl InProcess {
    /// Serves `catalog`, whose table files sit under `warehouse_prefix` (a `file://` URL
    /// ending in a slash).
    ///
    /// # Errors
    ///
    /// A config Floe refuses, or a failed startup.
    pub async fn start(
        catalog: Arc<dyn Catalog>,
        warehouse_prefix: &str,
    ) -> Result<Self, SuiteError> {
        let config = floe::parse_config(&local_config(warehouse_prefix), &|_| None)?;
        Self::start_with(
            config,
            Deps {
                catalog: LakeCatalog::from_catalog(catalog),
            },
        )
        .await
    }

    /// Serves with a checked config and its catalog.
    ///
    /// # Errors
    ///
    /// A failed startup: listing the catalog or binding the listener.
    pub async fn start_with(config: ValidConfig, deps: Deps) -> Result<Self, SuiteError> {
        let started = floe::start(config, deps).await?;
        let address = started.address();
        let shutdown = CancellationToken::new();
        let serving = tokio::spawn(started.serve(shutdown.clone()));
        Ok(Self {
            address,
            shutdown,
            serving,
        })
    }

    /// Where clients connect.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// A client connected as psql would be, to database `lake`. Its connection runs on a
    /// task that ends when the client is dropped.
    ///
    /// # Errors
    ///
    /// The connection or startup failure.
    pub async fn connect(&self) -> Result<Client, tokio_postgres::Error> {
        connect(self.address).await
    }

    /// Stops Floe, waiting for in-flight queries as `floe serve` does on SIGTERM.
    ///
    /// # Errors
    ///
    /// The server failed, or its task panicked.
    pub async fn stop(self) -> Result<ShutdownSummary, SuiteError> {
        self.shutdown.cancel();
        Ok(self.serving.await??)
    }
}

/// A client for the Floe at `address`, database `lake`, user `floe`.
///
/// # Errors
///
/// The connection or startup failure.
pub async fn connect(address: SocketAddr) -> Result<Client, tokio_postgres::Error> {
    let (client, connection) = tokio_postgres::Config::new()
        .host(address.ip().to_string())
        .port(address.port())
        .user("floe")
        .dbname("lake")
        .connect(NoTls)
        .await?;
    tokio::spawn(connection);
    Ok(client)
}

/// floe.toml for a local warehouse: loopback on port 0, local files allowed.
fn local_config(warehouse_prefix: &str) -> String {
    format!(
        r#"
        [server]
        pg_listen = "127.0.0.1:0"
        max_connections = 16
        statement_timeout = "60s"

        [catalog]
        uri = "http://127.0.0.1:8181/catalog"
        warehouse = "suites"

        [storage]
        region = "us-east-1"
        path_style = false
        allowed_prefixes = ["{warehouse_prefix}"]
        allow_local_files = true
        "#
    )
}
