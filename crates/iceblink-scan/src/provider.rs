//! `FloeTableProvider`: a DataFusion table over one loaded Iceberg table, read-only, with one
//! pinned snapshot and its own storage counters per query.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use datafusion::arrow::datatypes::SchemaRef;
use datafusion::catalog::{Session, TableProvider};
use datafusion::datasource::TableType;
use datafusion::error::Result as DFResult;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::empty::EmptyExec;
use futures::TryStreamExt;
use iceberg::Runtime;
use iceberg::arrow::schema_to_arrow_schema;
use iceberg::io::{FileIO, Storage};
use iceberg::spec::Snapshot;
use iceberg::table::Table;
use iceberg_datafusion::physical_plan::convert_filters_to_predicate;

use crate::exec::{FloeScanExec, ScanPlan};
use crate::io::{CountingStorage, IoStats, query_file_io};
use crate::location::LocationGuard;
use crate::receipt::QueryReceipt;
use crate::storage::Storages;
use crate::{ScanError, StorageSettings};

/// Serves one loaded Iceberg table to DataFusion. Each `scan` pins the table's current snapshot
/// and reads it through its own counting, write-refusing storage. A query sees a newer snapshot
/// when the caller loads the table again and builds a new provider.
pub struct FloeTableProvider {
    table: Table,
    schema: SchemaRef,
    guard: LocationGuard,
    inner: Arc<dyn Storage>,
}

impl FloeTableProvider {
    /// Wraps `table`, reading only under `settings.allowed_prefixes`.
    ///
    /// # Errors
    ///
    /// [`ScanError::Io`] when the table lives on storage Floe can't read (Floe reads `s3://`,
    /// and `file://` in tests), and [`ScanError::Iceberg`] when its schema has no Arrow form.
    pub fn new(table: Table, settings: &StorageSettings) -> Result<Self, ScanError> {
        let inner = Storages::new(settings).for_location(table.metadata().location())?;
        Self::with_inner_storage(table, settings, inner)
    }

    pub(crate) fn with_inner_storage(
        table: Table,
        settings: &StorageSettings,
        inner: Arc<dyn Storage>,
    ) -> Result<Self, ScanError> {
        let schema = schema_to_arrow_schema(table.metadata().current_schema())
            .map_err(|error| ScanError::from_iceberg(&error))?;
        Ok(Self {
            table,
            schema: Arc::new(schema),
            guard: LocationGuard::new(settings),
            inner,
        })
    }

    /// The scan's columns: all of them, or the projection DataFusion asked for.
    fn output_schema(&self, projection: Option<&Vec<usize>>) -> Result<SchemaRef, ScanError> {
        match projection {
            Some(columns) => Ok(Arc::new(self.schema.project(columns).map_err(|error| {
                ScanError::Iceberg {
                    message: format!("bad projection: {error}"),
                }
            })?)),
            None => Ok(Arc::clone(&self.schema)),
        }
    }

    /// Pins the snapshot, checks locations, and plans the files, all before any data is read.
    async fn plan(
        &self,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
    ) -> Result<FloeScanExec, ScanError> {
        let metadata = self.table.metadata();
        let snapshot = metadata
            .current_snapshot()
            .ok_or(ScanError::SnapshotMissing)?;
        self.guard.check(metadata.location())?;
        if let Some(location) = self.table.metadata_location() {
            self.guard.check(location)?;
        }
        let stats = Arc::new(IoStats::default());
        let storage = CountingStorage::new(
            Arc::clone(&self.inner),
            Arc::clone(&stats),
            self.guard.clone(),
        );
        let file_io = query_file_io(storage);
        let runtime = Runtime::current();
        let table = query_table(&self.table, file_io.clone(), runtime.clone())?;

        let output_schema = self.output_schema(projection)?;
        let builder = table.scan().snapshot_id(snapshot.snapshot_id());
        let mut builder = match projection {
            Some(_) => builder.select(output_schema.fields().iter().map(|field| field.name())),
            None => builder.select_all(),
        };
        if let Some(predicate) = convert_filters_to_predicate(filters) {
            builder = builder.with_filter(predicate);
        }
        let scan = builder
            .build()
            .map_err(|error| ScanError::from_iceberg(&error))?;
        let tasks = scan
            .plan_files()
            .await
            .map_err(|error| ScanError::from_iceberg(&error))?
            .try_collect()
            .await
            .map_err(|error| ScanError::from_iceberg(&error))?;
        Ok(FloeScanExec::new(ScanPlan {
            table: display_name(&self.table),
            snapshot_id: snapshot.snapshot_id(),
            files_total: total_data_files(snapshot),
            tasks,
            file_io,
            runtime,
            stats,
            schema: output_schema,
        }))
    }
}

impl fmt::Debug for FloeTableProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FloeTableProvider")
            .field("table", &display_name(&self.table))
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl TableProvider for FloeTableProvider {
    fn schema(&self) -> SchemaRef {
        Arc::clone(&self.schema)
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        _limit: Option<usize>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        if self.table.metadata().current_snapshot().is_none() {
            // Created but never written: the table is empty, and there is nothing to read.
            let schema = self
                .output_schema(projection)
                .map_err(ScanError::into_datafusion)?;
            return Ok(Arc::new(EmptyExec::new(schema)));
        }
        let exec = self
            .plan(projection, filters)
            .await
            .map_err(ScanError::into_datafusion)?;
        if let Some(receipt) = state.config().get_extension::<QueryReceipt>() {
            receipt.record(exec.receipt());
        }
        Ok(Arc::new(exec))
    }

    /// The same as the iceberg integration: everything is pushed down as a hint, and DataFusion
    /// re-checks every filter on the rows that come back.
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DFResult<Vec<TableProviderFilterPushDown>> {
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }
}

/// The query's own copy of the table: same metadata, read through the query's counting FileIO,
/// and marked read-only.
fn query_table(table: &Table, file_io: FileIO, runtime: Runtime) -> Result<Table, ScanError> {
    let mut builder = Table::builder()
        .identifier(table.identifier().clone())
        .metadata(table.metadata_ref())
        .file_io(file_io)
        .readonly(true)
        .runtime(runtime);
    if let Some(location) = table.metadata_location() {
        builder = builder.metadata_location(location);
    }
    builder
        .build()
        .map_err(|error| ScanError::from_iceberg(&error))
}

fn total_data_files(snapshot: &Snapshot) -> Option<u64> {
    snapshot
        .summary()
        .additional_properties
        .get("total-data-files")
        .and_then(|total| total.parse().ok())
}

fn display_name(table: &Table) -> String {
    let ident = table.identifier();
    let mut parts = ident.namespace().as_ref().clone();
    parts.push(ident.name().to_owned());
    parts.join(".")
}

#[cfg(test)]
mod tests;
