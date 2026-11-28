//! sqllogictest over the Postgres wire: each statement goes through tokio-postgres's simple
//! protocol to an in-process Floe, so the tests cover the protocol and Floe's own handling,
//! not just DataFusion.

use std::net::SocketAddr;
use std::path::Path;

use async_trait::async_trait;
use sqllogictest::{AsyncDB, DBOutput, DefaultColumnType, Runner};
use tokio_postgres::{Client, SimpleQueryMessage, SimpleQueryRow};

use crate::server::{InProcess, SuiteError, connect};
use crate::shop::shop;

/// A statement's failure as the `.slt` files match it: `SQLSTATE 25006: Floe is read-only: …`.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct FloeDbError(String);

impl From<tokio_postgres::Error> for FloeDbError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self(match error.as_db_error() {
            Some(database) => format!(
                "SQLSTATE {}: {}",
                database.code().code(),
                database.message()
            ),
            None => error.to_string(),
        })
    }
}

/// One client connection to Floe.
pub struct FloeDb {
    client: Client,
}

impl FloeDb {
    /// Connects to the Floe at `address`.
    ///
    /// # Errors
    ///
    /// The connection or startup failure.
    pub async fn connect(address: SocketAddr) -> Result<Self, FloeDbError> {
        Ok(Self {
            client: connect(address).await?,
        })
    }
}

#[async_trait]
impl AsyncDB for FloeDb {
    type Error = FloeDbError;
    type ColumnType = DefaultColumnType;

    async fn run(&mut self, sql: &str) -> Result<DBOutput<DefaultColumnType>, FloeDbError> {
        let mut columns = None;
        let mut rows = Vec::new();
        let mut count = 0;
        for message in self.client.simple_query(sql).await? {
            match message {
                SimpleQueryMessage::RowDescription(described) => columns = Some(described.len()),
                SimpleQueryMessage::Row(row) => rows.push(cells(&row)),
                SimpleQueryMessage::CommandComplete(completed) => count = completed,
                _ => {}
            }
        }
        Ok(match columns {
            Some(width) => DBOutput::Rows {
                types: vec![DefaultColumnType::Any; width],
                rows,
            },
            None => DBOutput::StatementComplete(count),
        })
    }

    async fn shutdown(&mut self) {}

    fn engine_name(&self) -> &str {
        "floe"
    }
}

/// A row as sqllogictest compares it: NULL as `NULL`, the empty string as `(empty)`.
fn cells(row: &SimpleQueryRow) -> Vec<String> {
    (0..row.len())
        .map(|index| match row.get(index) {
            None => "NULL".to_owned(),
            Some("") => "(empty)".to_owned(),
            Some(value) => value.to_owned(),
        })
        .collect()
}

/// Runs one `.slt` file against a fresh Floe over the shop dataset.
///
/// # Errors
///
/// A failed setup, or the first record whose result differs, with the file and line.
pub async fn run_file(path: &Path) -> Result<(), SuiteError> {
    let warehouse = shop().await?;
    let floe = InProcess::start(warehouse.catalog(), &warehouse.warehouse_prefix()).await?;
    let address = floe.address();
    let mut runner = Runner::new(move || FloeDb::connect(address));
    let outcome = runner.run_file_async(path).await;
    runner.shutdown_async().await;
    floe.stop().await?;
    outcome.map_err(|error| format!("{}", error.display(false)).into())
}
