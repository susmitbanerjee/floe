//! An in-process Floe server on 127.0.0.1:0 over the floe-testkit `sales.orders` table, and a
//! tokio-postgres client for it.

use std::error::Error;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use floe_catalog::LakeCatalog;
use floe_pg::{PgError, ServerSettings, ShutdownSummary, serve_on};
use floe_scan::{LakeCatalogProvider, StorageSettings, new_session};
use floe_testkit::{Fixture, TableFixture};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls, SimpleQueryMessage};
use tokio_util::sync::CancellationToken;
use url::Url;

pub(crate) type TestResult = Result<(), Box<dyn Error>>;

/// The server's statement timeout in every test.
pub(crate) const LIMIT: Duration = Duration::from_secs(30);

/// 30 days of orders, 3 files a day, 1,000 rows a file: 90,000 rows.
pub(crate) async fn orders() -> Result<Fixture, Box<dyn Error>> {
    Ok(TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(30)
        .files_per_day(3)
        .rows_per_file(1_000)
        .seed(42)
        .build()
        .await?)
}

pub(crate) struct TestServer {
    address: SocketAddr,
    pub(crate) shutdown: CancellationToken,
    pub(crate) task: JoinHandle<Result<ShutdownSummary, PgError>>,
}

impl TestServer {
    /// Serves `fixture`'s catalog, refreshed once as at startup, with a 10 s shutdown grace.
    pub(crate) async fn start(
        fixture: &Fixture,
        max_connections: usize,
    ) -> Result<Self, Box<dyn Error>> {
        Self::start_with_grace(fixture, max_connections, Duration::from_secs(10)).await
    }

    /// [`start`](Self::start) with a chosen shutdown grace.
    pub(crate) async fn start_with_grace(
        fixture: &Fixture,
        max_connections: usize,
        shutdown_grace: Duration,
    ) -> Result<Self, Box<dyn Error>> {
        let storage = StorageSettings {
            endpoint: None,
            region: "us-east-1".to_owned(),
            path_style: false,
            allowed_prefixes: vec![Url::parse(&fixture.warehouse_prefix())?],
            allow_local_files: true,
            aws_profile: None,
        };
        let lake = Arc::new(LakeCatalog::from_catalog(Arc::clone(&fixture.catalog)));
        let provider = Arc::new(LakeCatalogProvider::new(lake, storage));
        provider.refresh().await?;

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let settings = ServerSettings {
            listen: address,
            statement_timeout: LIMIT,
            max_connections,
            shutdown_grace,
        };
        let shutdown = CancellationToken::new();
        let factory = move || new_session(Arc::clone(&provider));
        let task = tokio::spawn(serve_on(listener, factory, settings, shutdown.clone()));
        Ok(Self {
            address,
            shutdown,
            task,
        })
    }

    /// A client connected as psql would be, to database `lake`.
    pub(crate) async fn connect(&self) -> Result<Client, tokio_postgres::Error> {
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(self.address.port())
            .user("floe")
            .dbname("lake")
            .connect(NoTls)
            .await?;
        // Ends when the client is dropped or the server closes the connection.
        tokio::spawn(connection);
        Ok(client)
    }
}

/// Every row of a simple-protocol result, as the text Postgres sent.
pub(crate) async fn rows(
    client: &Client,
    sql: &str,
) -> Result<Vec<Vec<String>>, tokio_postgres::Error> {
    let messages = client.simple_query(sql).await?;
    Ok(messages
        .iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(
                (0..row.len())
                    .map(|column| row.get(column).unwrap_or("NULL").to_owned())
                    .collect(),
            ),
            _ => None,
        })
        .collect())
}

/// The SQLSTATE and message of a statement that should have failed.
pub(crate) fn failure<T: std::fmt::Debug>(
    outcome: Result<T, tokio_postgres::Error>,
) -> Result<(String, String), String> {
    let error = match outcome {
        Ok(value) => return Err(format!("expected an error, got {value:?}")),
        Err(error) => error,
    };
    let database = error
        .as_db_error()
        .ok_or_else(|| format!("not a database error: {error}"))?;
    Ok((
        database.code().code().to_owned(),
        database.message().to_owned(),
    ))
}

/// Every file under `dir` with its size and modification time: equal before and after means
/// nothing was written.
pub(crate) fn files_under(dir: &Path) -> Result<Vec<(PathBuf, u64, SystemTime)>, std::io::Error> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(next)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                found.push((entry.path(), metadata.len(), metadata.modified()?));
            }
        }
    }
    found.sort();
    Ok(found)
}
