//! `TableFixture`: builds a real Iceberg table (Parquet data files, an in-memory catalog,
//! local files) in a temp dir, deterministically.

use std::collections::HashMap;
use std::sync::Arc;

use datafusion::arrow::datatypes::SchemaRef as ArrowSchemaRef;
use datafusion::arrow::error::ArrowError;
use datafusion::arrow::record_batch::RecordBatch;
use iceberg::arrow::schema_to_arrow_schema;
use iceberg::spec::{
    DataFile, Literal, NestedField, PartitionKey, PrimitiveType, Schema, Struct, Transform, Type,
    UnboundPartitionSpec,
};
use iceberg::table::Table;
use iceberg::{Catalog, Error, ErrorKind, NamespaceIdent, Result, TableCreation, TableIdent};
use tempfile::TempDir;

use crate::rows::{BASE_EPOCH_DAY, FileSpec, batch};
use crate::warehouse::{append, new_catalog, write_data_file};

/// Builds a `sales.orders`-shaped Iceberg table: order_id, tenant_id, customer_id, region,
/// status, amount decimal(14,2) and created_at timestamptz, with day 0 on 2026-01-01.
#[derive(Debug, Clone)]
pub struct TableFixture {
    namespace: String,
    table: String,
    partition_column: Option<String>,
    days: u32,
    files_per_day: u32,
    rows_per_file: u32,
    seed: u64,
}

impl TableFixture {
    /// A one-day, one-file, 100-row unpartitioned table named `namespace.table`.
    pub fn new(namespace: &str, table: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            table: table.to_owned(),
            partition_column: None,
            days: 1,
            files_per_day: 1,
            rows_per_file: 100,
            seed: 0,
        }
    }

    /// Partitions by `day(column)`. Only `created_at` exists in this schema.
    pub fn partition_by_day(mut self, column: &str) -> Self {
        self.partition_column = Some(column.to_owned());
        self
    }

    /// How many consecutive days, starting 2026-01-01, the first snapshot holds.
    pub fn days(mut self, days: u32) -> Self {
        self.days = days;
        self
    }

    /// Data files written for each day.
    pub fn files_per_day(mut self, files: u32) -> Self {
        self.files_per_day = files;
        self
    }

    /// Rows in each data file.
    pub fn rows_per_file(mut self, rows: u32) -> Self {
        self.rows_per_file = rows;
        self
    }

    /// Seeds the row generator. The same seed gives byte-identical data files.
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Creates the catalog, namespace and table in a fresh temp dir, writes every day's files and
    /// commits them as one snapshot.
    ///
    /// # Errors
    ///
    /// Any iceberg error from creating, writing or committing, or an I/O error making the dir.
    pub async fn build(self) -> Result<Fixture> {
        let (catalog, dir, warehouse) = new_catalog().await?;
        let namespace = NamespaceIdent::new(self.namespace.clone());
        catalog.create_namespace(&namespace, HashMap::new()).await?;
        let table = catalog.create_table(&namespace, self.creation()?).await?;
        let mut fixture = Fixture {
            catalog,
            dir,
            warehouse,
            ident: table.identifier().clone(),
            next_day: 0,
            spec: self,
        };
        let days = fixture.spec.days;
        fixture.commit_days(days).await?;
        Ok(fixture)
    }

    fn creation(&self) -> Result<TableCreation> {
        let schema = Schema::builder()
            .with_schema_id(0)
            .with_fields(vec![
                NestedField::required(1, "order_id", Type::Primitive(PrimitiveType::Long)).into(),
                NestedField::required(2, "tenant_id", Type::Primitive(PrimitiveType::String))
                    .into(),
                NestedField::optional(3, "customer_id", Type::Primitive(PrimitiveType::String))
                    .into(),
                NestedField::optional(4, "region", Type::Primitive(PrimitiveType::String)).into(),
                NestedField::optional(5, "status", Type::Primitive(PrimitiveType::String)).into(),
                NestedField::optional(
                    6,
                    "amount",
                    Type::Primitive(PrimitiveType::Decimal {
                        precision: 14,
                        scale: 2,
                    }),
                )
                .into(),
                NestedField::required(7, "created_at", Type::Primitive(PrimitiveType::Timestamptz))
                    .into(),
            ])
            .build()?;
        let creation = TableCreation::builder()
            .name(self.table.clone())
            .schema(schema.clone());
        Ok(match &self.partition_column {
            None => creation.build(),
            Some(column) => {
                let field = schema.field_by_name(column).ok_or_else(|| {
                    Error::new(ErrorKind::DataInvalid, format!("no column named {column}"))
                })?;
                let spec = UnboundPartitionSpec::builder()
                    .add_partition_field(field.id, format!("{column}_day"), Transform::Day)?
                    .build();
                creation.partition_spec(spec).build()
            }
        })
    }
}

/// A built fixture. The temp dir, and every file in it, lives as long as this value.
#[derive(Debug)]
pub struct Fixture {
    /// The in-memory catalog holding the table.
    pub catalog: Arc<dyn Catalog>,
    /// The temp dir holding the warehouse.
    pub dir: TempDir,
    warehouse: String,
    ident: TableIdent,
    next_day: u32,
    spec: TableFixture,
}

impl Fixture {
    /// The table's identifier, such as `sales.orders`.
    pub fn ident(&self) -> &TableIdent {
        &self.ident
    }

    /// The warehouse URL with a trailing slash, for `StorageSettings.allowed_prefixes`.
    pub fn warehouse_prefix(&self) -> String {
        format!("{}/", self.warehouse)
    }

    /// Loads the table's current metadata from the catalog.
    ///
    /// # Errors
    ///
    /// Whatever the in-memory catalog returns.
    pub async fn load_table(&self) -> Result<Table> {
        self.catalog.load_table(&self.ident).await
    }

    /// Writes the next day's files and commits them as a new snapshot.
    ///
    /// # Errors
    ///
    /// Any iceberg error from writing or committing.
    pub async fn append_day(&mut self) -> Result<()> {
        self.commit_days(1).await
    }

    /// Every row committed so far, regenerated from the same generator that wrote the files:
    /// the independent answer for tests to check queries against.
    ///
    /// # Errors
    ///
    /// An iceberg or Arrow error building the schema or a batch.
    pub fn batches(&self) -> Result<Vec<RecordBatch>> {
        let schema = self.spec.creation()?.schema;
        let arrow_schema: ArrowSchemaRef = Arc::new(schema_to_arrow_schema(&schema)?);
        let mut batches = Vec::new();
        for day in 0..self.next_day {
            for file in 0..self.spec.files_per_day {
                let spec = self.file_spec(day, file);
                batches.push(batch(Arc::clone(&arrow_schema), spec).map_err(bad_batch)?);
            }
        }
        Ok(batches)
    }

    /// Creates another table, with the same schema and no data, in the fixture's namespace.
    ///
    /// # Errors
    ///
    /// Whatever the in-memory catalog returns, for example if the name is taken.
    pub async fn add_empty_table(&self, name: &str) -> Result<Table> {
        self.create_empty(self.ident.namespace(), name).await
    }

    /// Creates a table with the same schema and no data in `namespace`, creating the namespace
    /// first if the catalog doesn't have it: for tests that need two namespaces.
    ///
    /// # Errors
    ///
    /// Whatever the in-memory catalog returns, for example if the name is taken.
    pub async fn add_empty_table_in(&self, namespace: &str, name: &str) -> Result<Table> {
        let namespace = NamespaceIdent::new(namespace.to_owned());
        if !self.catalog.namespace_exists(&namespace).await? {
            self.catalog
                .create_namespace(&namespace, HashMap::new())
                .await?;
        }
        self.create_empty(&namespace, name).await
    }

    async fn create_empty(&self, namespace: &NamespaceIdent, name: &str) -> Result<Table> {
        let creation = TableFixture {
            table: name.to_owned(),
            ..self.spec.clone()
        }
        .creation()?;
        self.catalog.create_table(namespace, creation).await
    }

    fn file_spec(&self, day: u32, file: u32) -> FileSpec {
        FileSpec {
            seed: self.spec.seed,
            day,
            file,
            files_per_day: self.spec.files_per_day,
            rows: self.spec.rows_per_file,
        }
    }

    async fn commit_days(&mut self, days: u32) -> Result<()> {
        let table = self.load_table().await?;
        let mut files = Vec::new();
        for day in self.next_day..self.next_day + days {
            for file in 0..self.spec.files_per_day {
                files.extend(self.write_file(&table, day, file).await?);
            }
        }
        append(self.catalog.as_ref(), &table, files).await?;
        self.next_day += days;
        Ok(())
    }

    async fn write_file(&self, table: &Table, day: u32, file: u32) -> Result<Vec<DataFile>> {
        let metadata = table.metadata();
        let schema = metadata.current_schema().clone();
        let arrow_schema: ArrowSchemaRef = Arc::new(schema_to_arrow_schema(&schema)?);
        let partition_key = match self.spec.partition_column {
            None => None,
            Some(_) => {
                let date = BASE_EPOCH_DAY + i32::try_from(day).unwrap_or(i32::MAX);
                let spec = metadata.default_partition_spec().as_ref().clone();
                let data = Struct::from_iter([Some(Literal::date(date))]);
                Some(PartitionKey::new(spec, schema.clone(), data))
            }
        };
        let rows = batch(arrow_schema, self.file_spec(day, file)).map_err(bad_batch)?;
        let name = format!("d{day:03}-f{file:02}.parquet");
        write_data_file(table, partition_key, &name, vec![rows]).await
    }
}

fn bad_batch(error: ArrowError) -> Error {
    Error::new(ErrorKind::Unexpected, "bad fixture batch").with_source(error)
}
