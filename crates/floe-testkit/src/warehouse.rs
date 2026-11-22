//! `Warehouse`: an in-memory Iceberg catalog over local files in a temp dir, holding tables
//! built from any Arrow batches. The end-to-end suites write their literal datasets and TPC-H
//! through it; `Fixture` shares its writer.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use datafusion::arrow::record_batch::RecordBatch;
use iceberg::arrow::{arrow_schema_to_schema_auto_assign_ids, schema_to_arrow_schema};
use iceberg::io::LocalFsStorageFactory;
use iceberg::memory::{MEMORY_CATALOG_WAREHOUSE, MemoryCatalogBuilder};
use iceberg::spec::{DataFile, PartitionKey};
use iceberg::table::Table;
use iceberg::transaction::{ApplyTransactionAction, Transaction};
use iceberg::writer::base_writer::data_file_writer::DataFileWriterBuilder;
use iceberg::writer::file_writer::ParquetWriterBuilder;
use iceberg::writer::file_writer::location_generator::{
    DefaultLocationGenerator, FileNameGenerator,
};
use iceberg::writer::file_writer::rolling_writer::RollingFileWriterBuilder;
use iceberg::writer::{IcebergWriter, IcebergWriterBuilder};
use iceberg::{Catalog, CatalogBuilder, Error, ErrorKind, NamespaceIdent, Result, TableCreation};
use parquet::file::properties::WriterProperties;
use tempfile::TempDir;

/// An empty warehouse: a memory catalog whose tables live under a temp dir. The dir, and every
/// file in it, lives as long as this value.
#[derive(Debug)]
pub struct Warehouse {
    catalog: Arc<dyn Catalog>,
    dir: TempDir,
    location: String,
}

impl Warehouse {
    /// A new, empty warehouse in a fresh temp dir.
    ///
    /// # Errors
    ///
    /// An I/O error making the dir, or whatever the memory catalog returns.
    pub async fn new() -> Result<Self> {
        let (catalog, dir, location) = new_catalog().await?;
        Ok(Self {
            catalog,
            dir,
            location,
        })
    }

    /// The catalog, for `LakeCatalog::from_catalog`.
    pub fn catalog(&self) -> Arc<dyn Catalog> {
        Arc::clone(&self.catalog)
    }

    /// The temp dir holding every table's files.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The warehouse URL with a trailing slash, for `storage.allowed_prefixes`.
    pub fn warehouse_prefix(&self) -> String {
        format!("{}/", self.location)
    }

    /// Creates `namespace.name` with the first batch's schema (field IDs assigned in order),
    /// writes `batches` into one Parquet file, and commits them as one snapshot. The namespace
    /// is created if needed.
    ///
    /// # Errors
    ///
    /// No batches, a schema Iceberg can't hold, or any error creating, writing or committing.
    pub async fn add_table(
        &self,
        namespace: &str,
        name: &str,
        batches: &[RecordBatch],
    ) -> Result<Table> {
        let first = batches.first().ok_or_else(|| {
            Error::new(
                ErrorKind::DataInvalid,
                "a table needs at least one batch, for its schema",
            )
        })?;
        let schema = arrow_schema_to_schema_auto_assign_ids(&first.schema())?;
        let namespace = NamespaceIdent::new(namespace.to_owned());
        if !self.catalog.namespace_exists(&namespace).await? {
            self.catalog
                .create_namespace(&namespace, HashMap::new())
                .await?;
        }
        let creation = TableCreation::builder()
            .name(name.to_owned())
            .schema(schema)
            .build();
        let table = self.catalog.create_table(&namespace, creation).await?;
        // The writer wants the Arrow schema that carries Iceberg's field IDs.
        let with_ids = Arc::new(schema_to_arrow_schema(table.metadata().current_schema())?);
        let batches = batches
            .iter()
            .map(|batch| RecordBatch::try_new(Arc::clone(&with_ids), batch.columns().to_vec()))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                Error::new(ErrorKind::DataInvalid, "batches don't share one schema")
                    .with_source(error)
            })?;
        let files = write_data_file(&table, None, "data-00.parquet", batches).await?;
        append(self.catalog.as_ref(), &table, files).await?;
        self.catalog.load_table(table.identifier()).await
    }
}

/// A memory catalog over `file://<temp dir>/warehouse`.
pub(crate) async fn new_catalog() -> Result<(Arc<dyn Catalog>, TempDir, String)> {
    let dir = tempfile::tempdir()
        .map_err(|error| Error::new(ErrorKind::Unexpected, "no temp dir").with_source(error))?;
    let location = format!("file://{}/warehouse", dir.path().display());
    let catalog = MemoryCatalogBuilder::default()
        .with_storage_factory(Arc::new(LocalFsStorageFactory))
        .load(
            "fixture",
            HashMap::from([(MEMORY_CATALOG_WAREHOUSE.to_owned(), location.clone())]),
        )
        .await?;
    Ok((Arc::new(catalog), dir, location))
}

/// Writes `batches` into one data file named `name` (fixed, so paths and bytes are
/// deterministic) under `table`'s data location.
pub(crate) async fn write_data_file(
    table: &Table,
    partition_key: Option<PartitionKey>,
    name: &str,
    batches: Vec<RecordBatch>,
) -> Result<Vec<DataFile>> {
    let metadata = table.metadata();
    let properties = WriterProperties::builder()
        .set_created_by("floe-testkit".to_owned())
        .build();
    let rolling = RollingFileWriterBuilder::new_with_default_file_size(
        ParquetWriterBuilder::new(properties, metadata.current_schema().clone()),
        table.file_io().clone(),
        DefaultLocationGenerator::new(metadata)?,
        FixedName(name.to_owned()),
    );
    let mut writer = DataFileWriterBuilder::new(rolling)
        .build(partition_key)
        .await?;
    for batch in batches {
        writer.write(batch).await?;
    }
    writer.close().await
}

/// Commits `files` to `table` as one fast-append snapshot.
pub(crate) async fn append(
    catalog: &dyn Catalog,
    table: &Table,
    files: Vec<DataFile>,
) -> Result<()> {
    let transaction = Transaction::new(table);
    let transaction = transaction
        .fast_append()
        .add_data_files(files)
        .apply(transaction)?;
    transaction.commit(catalog).await?;
    Ok(())
}

/// One file per writer, so a fixed name keeps paths, and with them the bytes, deterministic.
#[derive(Debug, Clone)]
struct FixedName(String);

impl FileNameGenerator for FixedName {
    fn generate_file_name(&self) -> String {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use datafusion::arrow::array::{ArrayRef, Int64Array, StringArray};
    use datafusion::arrow::datatypes::{DataType, Field, Schema};

    use super::*;

    #[tokio::test]
    async fn writes_any_batches_as_an_iceberg_table() -> Result<()> {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]));
        let columns: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![1, 2])),
            Arc::new(StringArray::from(vec![Some("a"), None])),
        ];
        let batch = RecordBatch::try_new(schema, columns)
            .map_err(|error| Error::new(ErrorKind::DataInvalid, "bad batch").with_source(error))?;
        let warehouse = Warehouse::new().await?;
        let table = warehouse.add_table("shop", "things", &[batch]).await?;
        let snapshot = table.metadata().current_snapshot();
        assert!(snapshot.is_some(), "no snapshot committed");
        let fields: Vec<String> = table
            .metadata()
            .current_schema()
            .as_struct()
            .fields()
            .iter()
            .map(|field| format!("{}:{}", field.id, field.name))
            .collect();
        assert_eq!(fields, ["1:id", "2:name"]);
        Ok(())
    }
}
