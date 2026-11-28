//! Proves DataFusion, the Iceberg integration and datafusion-postgres build against one
//! DataFusion and one Arrow (see docs/adr/0001-datafusion-version.md).
//!
//! If any of them resolves to a different DataFusion or iceberg version, the types below stop
//! lining up and this file fails to compile. Nothing here touches the network.

use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;

use datafusion::arrow::array::Int64Array;
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::catalog::{CatalogProvider, MemTable, TableProvider};
use datafusion::prelude::SessionContext;
use datafusion_postgres::ServerOptions;
use iceberg_catalog_rest::{
    REST_CATALOG_PROP_URI, REST_CATALOG_PROP_WAREHOUSE, RestCatalog, RestCatalogBuilder,
};
use iceberg_datafusion::{IcebergCatalogProvider, IcebergTableProvider};

/// Holds only if `T` implements the `TableProvider` of the DataFusion this crate uses.
fn is_our_table_provider<T: TableProvider>() {}

/// Holds only if `T` implements the `CatalogProvider` of the DataFusion this crate uses.
fn is_our_catalog_provider<T: CatalogProvider>() {}

/// Holds only if `T` implements the `Catalog` of the iceberg this crate uses.
fn is_our_iceberg_catalog<T: iceberg::Catalog>() {}

#[tokio::test(flavor = "multi_thread")]
async fn runs_select_one_with_an_in_memory_table_registered() -> Result<(), Box<dyn Error>> {
    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
    let column = Arc::new(Int64Array::from(vec![1, 2, 3]));
    let batch = RecordBatch::try_new(Arc::clone(&schema), vec![column])?;
    let table = MemTable::try_new(schema, vec![vec![batch]])?;

    let context = SessionContext::new();
    context.register_table("numbers", Arc::new(table))?;
    assert!(context.table_exist("numbers")?);

    let batches = context.sql("SELECT 1").await?.collect().await?;
    let rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(rows, 1);
    Ok(())
}

#[test]
fn integrations_share_one_datafusion_and_one_iceberg() {
    is_our_table_provider::<IcebergTableProvider>();
    is_our_catalog_provider::<IcebergCatalogProvider>();
    is_our_iceberg_catalog::<RestCatalog>();
}

#[test]
fn builds_rest_catalog_and_server_options_without_connecting() {
    // `load` is the call that contacts the catalog, so the builder and its properties are as far
    // as a hermetic test can go. RestCatalogConfig itself is private in iceberg-catalog-rest 0.11.
    let _builder = RestCatalogBuilder::default();
    let properties = HashMap::from([
        (
            REST_CATALOG_PROP_URI.to_owned(),
            "http://127.0.0.1:8181".to_owned(),
        ),
        (
            REST_CATALOG_PROP_WAREHOUSE.to_owned(),
            "s3://warehouse/".to_owned(),
        ),
    ]);
    assert_eq!(properties.len(), 2);

    let options = ServerOptions::new().with_port(15432);
    assert_eq!(options.host(), "127.0.0.1");
    assert_eq!(*options.port(), 15432);

    // Building the future type-checks `serve` against our SessionContext. It is dropped without
    // being polled, so no socket is opened.
    let server = datafusion_postgres::serve(Arc::new(SessionContext::new()), &options);
    drop(server);
}
