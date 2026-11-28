//! The `SessionContext` every Floe query runs in: the `lake` catalog, and nothing that can read
//! or write files on its own (docs/adr/0007-session-lockdown.md).
//!
//! # Audit of DataFusion 54.2.0's defaults
//!
//! What `SessionStateBuilder::with_default_features` registers, and what Floe keeps:
//!
//! - **Table functions:** `generate_series` and `range` only. Neither reads files or URLs; both
//!   are kept. There is no `read_csv`, `read_parquet` or similar in DataFusion core.
//! - **Scalar, nested, aggregate and window functions:** the full default sets from
//!   datafusion-functions, -nested, -aggregate and -window. None of those crates references
//!   `std::fs`, `tokio::fs`, `std::process`, `std::net`, `object_store` or `reqwest` (grepped at
//!   54.2.0). They compute on values only; all are kept.
//! - **Table factories** PARQUET, CSV, JSON, NDJSON, AVRO and ARROW, which back
//!   `CREATE EXTERNAL TABLE`: **removed**.
//! - **File formats** parquet, json, csv and arrow, which back `COPY … TO` and listing tables:
//!   **removed**.
//! - **Object store registry:** the default pre-registers a `LocalFileSystem` rooted at `/` for
//!   `file://`. **Replaced** with [`ClosedObjectStoreRegistry`], which serves no store at all.
//!   Floe's scans read through iceberg's FileIO and Floe's counting storage, never this registry.
//! - **URL tables** (`SELECT * FROM '/path'`): off by default, and left off (`enable_url_table`
//!   is never called).
//! - **The in-memory `datafusion.public` schema**, where `CREATE TABLE AS` and `CREATE VIEW`
//!   would succeed: **not created**. The `lake` schemas refuse to register tables.
//! - **Spill files:** the default disk manager writes spills to the OS temp dir. **Disabled**: a
//!   query that runs out of memory fails instead of writing files.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use datafusion::catalog::{CatalogProviderList, MemoryCatalogProviderList};
use datafusion::config::Dialect;
use datafusion::error::{DataFusionError, Result as DFResult};
use datafusion::execution::disk_manager::{DiskManagerBuilder, DiskManagerMode};
use datafusion::execution::object_store::ObjectStoreRegistry;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::execution::session_state::SessionStateBuilder;
use datafusion::object_store::ObjectStore;
use datafusion::prelude::{SessionConfig, SessionContext};
use url::Url;

use crate::{LakeCatalogProvider, ScanError};

/// The catalog name psql users connect to as their database.
pub const CATALOG_NAME: &str = "lake";

/// The default schema when the catalog has no namespaces yet.
const FALLBACK_SCHEMA: &str = "public";

/// Builds the locked-down session over `catalog`.
///
/// - Default catalog `lake`; default schema the first namespace in alphabetical order, or
///   `public` when there is none.
/// - `information_schema` on, which pg_catalog needs. `target_partitions` = available CPUs.
///   PostgreSQL SQL dialect, for psql clients. Literals such as `120.50` are decimals, as in
///   Postgres, not floats: a float literal compared with a decimal column loses cents
///   (`price = 999999999999.99` matched nothing).
/// - No table factories, file formats, object stores, URL tables, in-memory schemas or spill
///   files. See the module documentation for the audit.
///
/// # Errors
///
/// [`ScanError::Io`] if DataFusion's runtime can't be set up.
pub fn new_session(catalog: Arc<LakeCatalogProvider>) -> Result<SessionContext, ScanError> {
    session_with_registry(catalog, Arc::new(ClosedObjectStoreRegistry::default()))
}

pub(crate) fn session_with_registry(
    catalog: Arc<LakeCatalogProvider>,
    registry: Arc<ClosedObjectStoreRegistry>,
) -> Result<SessionContext, ScanError> {
    let default_schema = catalog
        .first_namespace()
        .unwrap_or_else(|| FALLBACK_SCHEMA.to_owned());
    let partitions = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
    let mut config = SessionConfig::new()
        .with_default_catalog_and_schema(CATALOG_NAME, default_schema)
        .with_create_default_catalog_and_schema(false)
        .with_information_schema(true)
        .with_target_partitions(partitions);
    config.options_mut().sql_parser.dialect = Dialect::PostgreSQL;
    config.options_mut().sql_parser.parse_float_as_decimal = true;

    let runtime = RuntimeEnvBuilder::new()
        .with_object_store_registry(registry)
        .with_disk_manager_builder(
            DiskManagerBuilder::default().with_mode(DiskManagerMode::Disabled),
        )
        .build_arc()
        .map_err(|error| ScanError::Io {
            message: format!("could not set up the query runtime: {error}"),
        })?;

    let catalogs = MemoryCatalogProviderList::new();
    catalogs.register_catalog(CATALOG_NAME.to_owned(), catalog);
    let state = SessionStateBuilder::new()
        .with_config(config)
        .with_runtime_env(runtime)
        .with_default_features()
        .with_table_factories(HashMap::new())
        .with_file_formats(Vec::new())
        .with_catalog_list(Arc::new(catalogs))
        .build();
    Ok(SessionContext::new_with_state(state))
}

/// An object store registry with no stores, which refuses every request for one. Counts
/// refusals so tests can see a file-reading path was stopped here.
#[derive(Debug, Default)]
pub(crate) struct ClosedObjectStoreRegistry {
    refused: AtomicUsize,
}

impl ClosedObjectStoreRegistry {
    #[cfg(test)]
    pub(crate) fn refused(&self) -> usize {
        self.refused.load(Ordering::SeqCst)
    }

    fn refuse(&self, url: &Url) -> DataFusionError {
        self.refused.fetch_add(1, Ordering::SeqCst);
        DataFusionError::Plan(format!(
            "Floe reads storage only through its Iceberg tables, so {}:// locations can't be \
             used from SQL. Query a table in the lake catalog instead.",
            url.scheme()
        ))
    }
}

impl ObjectStoreRegistry for ClosedObjectStoreRegistry {
    /// Never stores anything: the trait can't return an error here, so the store is dropped
    /// and the refusal counted.
    fn register_store(
        &self,
        url: &Url,
        _store: Arc<dyn ObjectStore>,
    ) -> Option<Arc<dyn ObjectStore>> {
        let _ = self.refuse(url);
        None
    }

    fn get_store(&self, url: &Url) -> DFResult<Arc<dyn ObjectStore>> {
        Err(self.refuse(url))
    }
}

#[cfg(test)]
mod tests;
