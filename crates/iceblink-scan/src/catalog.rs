//! The `lake` catalog: Iceberg namespaces as DataFusion schemas, Iceberg tables as tables.
//!
//! DataFusion asks for names synchronously and loads tables asynchronously, so the listing is
//! fetched up front by `refresh` and each table loads on first use. Loaded tables are kept for
//! the process lifetime in phase 1, until the next `refresh`; the snapshot watcher arrives in
//! v0.1.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use async_trait::async_trait;
use datafusion::catalog::{CatalogProvider, SchemaProvider, TableProvider};
use datafusion::datasource::TableType;
use datafusion::error::{DataFusionError, Result as DFResult};
use floe_catalog::{CatalogError, LakeCatalog, NamespaceName, TableName};

use crate::storage::Storages;
use crate::{FloeTableProvider, ScanError, StorageSettings};

/// Namespace name to its table names, both sorted.
type Listing = BTreeMap<String, BTreeSet<String>>;

/// Tables loaded so far, by namespace and table name.
type Loaded = HashMap<(String, String), Arc<dyn TableProvider>>;

/// The Postgres system schema, the one schema a server may add to the lake catalog.
const PG_CATALOG: &str = "pg_catalog";

/// State shared by the catalog provider and every namespace provider it hands out.
struct Shared {
    lake: Arc<LakeCatalog>,
    storage: StorageSettings,
    /// One S3 client set for every table, so connections and credentials are shared.
    storages: Storages,
    listing: RwLock<Listing>,
    loaded: Mutex<Loaded>,
    pg_catalog: RwLock<Option<Arc<dyn SchemaProvider>>>,
}

impl Shared {
    fn pg_catalog(&self) -> Option<Arc<dyn SchemaProvider>> {
        self.pg_catalog
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn listing(&self) -> std::sync::RwLockReadGuard<'_, Listing> {
        self.listing.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn loaded(&self) -> std::sync::MutexGuard<'_, Loaded> {
        self.loaded.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_listed(&self, namespace: &str, table: &str) -> bool {
        self.listing()
            .get(namespace)
            .is_some_and(|tables| tables.contains(table))
    }
}

/// What a [`refresh`](LakeCatalogProvider::refresh) found.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RefreshSummary {
    /// Namespaces in the catalog.
    pub namespaces: usize,
    /// Tables across all of them.
    pub tables: usize,
}

/// The DataFusion catalog Floe serves as database `lake`.
pub struct LakeCatalogProvider {
    shared: Arc<Shared>,
}

impl LakeCatalogProvider {
    /// An empty catalog over `lake`. Call [`refresh`](Self::refresh) before serving queries.
    pub fn new(lake: Arc<LakeCatalog>, storage: StorageSettings) -> Self {
        Self {
            shared: Arc::new(Shared {
                lake,
                storages: Storages::new(&storage),
                storage,
                listing: RwLock::new(Listing::new()),
                loaded: Mutex::new(Loaded::new()),
                pg_catalog: RwLock::new(None),
            }),
        }
    }

    /// Re-reads every namespace and its tables from the Iceberg catalog, and forgets loaded
    /// tables so the next query loads each table's current snapshot.
    ///
    /// # Errors
    ///
    /// Whatever the catalog returns. The previous listing stays in place on error.
    pub async fn refresh(&self) -> Result<RefreshSummary, CatalogError> {
        let mut listing = Listing::new();
        let mut summary = RefreshSummary::default();
        for namespace in self.shared.lake.list_namespaces().await? {
            let tables = self.shared.lake.list_tables(&namespace).await?;
            let names: BTreeSet<String> =
                tables.iter().map(|table| table.name().to_owned()).collect();
            summary.namespaces += 1;
            summary.tables += names.len();
            listing.insert(namespace.as_str().to_owned(), names);
        }
        *self
            .shared
            .listing
            .write()
            .unwrap_or_else(PoisonError::into_inner) = listing;
        self.shared.loaded().clear();
        Ok(summary)
    }

    /// The first namespace in alphabetical order, if there is one: the session's default schema.
    pub(crate) fn first_namespace(&self) -> Option<String> {
        self.shared.listing().keys().next().cloned()
    }
}

impl fmt::Debug for LakeCatalogProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LakeCatalogProvider")
            .field("namespaces", &self.shared.listing().len())
            .finish_non_exhaustive()
    }
}

impl CatalogProvider for LakeCatalogProvider {
    fn schema_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.shared.listing().keys().cloned().collect();
        if self.shared.pg_catalog().is_some() && !names.iter().any(|name| name == PG_CATALOG) {
            names.push(PG_CATALOG.to_owned());
        }
        names
    }

    /// `pg_catalog`, once registered, hides an Iceberg namespace of the same name.
    fn schema(&self, name: &str) -> Option<Arc<dyn SchemaProvider>> {
        if name == PG_CATALOG
            && let Some(schema) = self.shared.pg_catalog()
        {
            return Some(schema);
        }
        self.shared
            .listing()
            .contains_key(name)
            .then(|| -> Arc<dyn SchemaProvider> {
                Arc::new(NamespaceProvider {
                    namespace: name.to_owned(),
                    shared: Arc::clone(&self.shared),
                })
            })
    }

    /// Accepts only `pg_catalog`, the Postgres system schema the server sets up for clients
    /// such as psql. Every other schema is an Iceberg namespace, and Floe never creates those.
    fn register_schema(
        &self,
        name: &str,
        schema: Arc<dyn SchemaProvider>,
    ) -> DFResult<Option<Arc<dyn SchemaProvider>>> {
        if name != PG_CATALOG {
            return Err(DataFusionError::Plan(format!(
                "Floe is read-only: schema {name} can't be created; schemas are the Iceberg \
                 catalog's namespaces"
            )));
        }
        let mut slot = self
            .shared
            .pg_catalog
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        Ok(slot.replace(schema))
    }
}

/// One Iceberg namespace, served as a DataFusion schema.
struct NamespaceProvider {
    namespace: String,
    shared: Arc<Shared>,
}

impl NamespaceProvider {
    /// Loads `table` from the catalog and wraps it for scanning.
    async fn load(&self, table: &str) -> Result<Arc<dyn TableProvider>, DataFusionError> {
        let external = |error: CatalogError| DataFusionError::External(Box::new(error));
        let name = TableName::new(
            NamespaceName::new(self.namespace.clone()).map_err(external)?,
            table,
        )
        .map_err(external)?;
        let loaded = self.shared.lake.load_table(&name).await.map_err(external)?;
        let scan_error = |error: ScanError| DataFusionError::External(Box::new(error));
        let inner = self
            .shared
            .storages
            .for_location(loaded.metadata().location())
            .map_err(scan_error)?;
        let provider = FloeTableProvider::with_inner_storage(loaded, &self.shared.storage, inner)
            .map_err(scan_error)?;
        Ok(Arc::new(provider))
    }
}

impl fmt::Debug for NamespaceProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NamespaceProvider")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SchemaProvider for NamespaceProvider {
    fn table_names(&self) -> Vec<String> {
        self.shared
            .listing()
            .get(&self.namespace)
            .map(|tables| tables.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// An unlisted name is `None`, which DataFusion reports as "table … not found".
    async fn table(&self, name: &str) -> DFResult<Option<Arc<dyn TableProvider>>> {
        if !self.shared.is_listed(&self.namespace, name) {
            return Ok(None);
        }
        let key = (self.namespace.clone(), name.to_owned());
        if let Some(cached) = self.shared.loaded().get(&key) {
            return Ok(Some(Arc::clone(cached)));
        }
        let provider = self.load(name).await?;
        self.shared.loaded().insert(key, Arc::clone(&provider));
        Ok(Some(provider))
    }

    /// Answers from the listing: SHOW TABLES must not load every table from the catalog.
    async fn table_type(&self, name: &str) -> DFResult<Option<TableType>> {
        Ok(self
            .shared
            .is_listed(&self.namespace, name)
            .then_some(TableType::Base))
    }

    fn table_exist(&self, name: &str) -> bool {
        self.shared.is_listed(&self.namespace, name)
    }
}
