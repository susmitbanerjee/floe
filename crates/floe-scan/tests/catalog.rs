//! The `lake` catalog end to end on the 30-day `sales.orders` fixture, through `new_session`.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;

use datafusion::arrow::array::{Array, Decimal128Array, StringArray};
use datafusion::arrow::record_batch::RecordBatch;
use datafusion::arrow::util::display::array_value_to_string;
use datafusion::catalog::{CatalogProvider, MemorySchemaProvider};
use datafusion::prelude::SessionContext;
use floe_catalog::LakeCatalog;
use floe_scan::{LakeCatalogProvider, StorageSettings, new_session};
use floe_testkit::{Fixture, TableFixture};
use url::Url;

type TestResult = Result<(), Box<dyn Error>>;

async fn orders() -> Result<Fixture, iceberg::Error> {
    TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(30)
        .files_per_day(3)
        .rows_per_file(1_000)
        .seed(42)
        .build()
        .await
}

/// The catalog provider, refreshed once as at startup, and a session over it.
async fn lake(
    fixture: &Fixture,
) -> Result<(Arc<LakeCatalogProvider>, SessionContext), Box<dyn Error>> {
    let storage = StorageSettings {
        endpoint: None,
        region: "us-east-1".to_owned(),
        path_style: false,
        allowed_prefixes: vec![Url::parse(&fixture.warehouse_prefix())?],
        allow_local_files: true,
        aws_profile: None,
    };
    let catalog = Arc::new(LakeCatalog::from_catalog(Arc::clone(&fixture.catalog)));
    let provider = Arc::new(LakeCatalogProvider::new(catalog, storage));
    provider.refresh().await?;
    let session = new_session(Arc::clone(&provider))?;
    Ok((provider, session))
}

/// Every row of the result, each cell as text.
async fn rows(session: &SessionContext, sql: &str) -> Result<Vec<Vec<String>>, Box<dyn Error>> {
    let batches = session.sql(sql).await?.collect().await?;
    let mut rows = Vec::new();
    for batch in &batches {
        for row in 0..batch.num_rows() {
            let cells = batch
                .columns()
                .iter()
                .map(|column| array_value_to_string(column, row))
                .collect::<Result<Vec<_>, _>>()?;
            rows.push(cells);
        }
    }
    Ok(rows)
}

fn shows_table(rows: &[Vec<String>], schema: &str, table: &str) -> bool {
    rows.iter()
        .any(|row| row.as_slice() == ["lake", schema, table, "BASE TABLE"])
}

#[tokio::test]
async fn show_tables_lists_the_fixture_table() -> TestResult {
    let fixture = orders().await?;
    let (_, session) = lake(&fixture).await?;
    let tables = rows(&session, "SHOW TABLES").await?;
    assert!(shows_table(&tables, "sales", "orders"), "{tables:?}");
    Ok(())
}

/// Per-region order count and amount total in cents, from the fixture's own generator.
fn expected_by_region(batches: &[RecordBatch]) -> Result<Vec<Vec<String>>, Box<dyn Error>> {
    let mut totals: BTreeMap<String, (i64, i128)> = BTreeMap::new();
    for batch in batches {
        let regions = batch
            .column_by_name("region")
            .and_then(|column| column.as_any().downcast_ref::<StringArray>())
            .ok_or("no region column")?;
        let amounts = batch
            .column_by_name("amount")
            .and_then(|column| column.as_any().downcast_ref::<Decimal128Array>())
            .ok_or("no amount column")?;
        for row in 0..batch.num_rows() {
            let entry = totals.entry(regions.value(row).to_owned()).or_default();
            entry.0 += 1;
            entry.1 += amounts.value(row);
        }
    }
    Ok(totals
        .into_iter()
        .map(|(region, (count, cents))| {
            vec![
                region,
                count.to_string(),
                format!("{}.{:02}", cents / 100, cents % 100),
            ]
        })
        .collect())
}

#[tokio::test]
async fn group_by_region_matches_the_generator() -> TestResult {
    let fixture = orders().await?;
    let (_, session) = lake(&fixture).await?;
    let actual = rows(
        &session,
        "SELECT region, count(*), sum(amount) FROM lake.sales.orders \
         GROUP BY region ORDER BY region",
    )
    .await?;
    assert_eq!(actual, expected_by_region(&fixture.batches()?)?);
    Ok(())
}

#[tokio::test]
async fn unqualified_names_use_the_first_namespace() -> TestResult {
    let fixture = orders().await?;
    let (_, session) = lake(&fixture).await?;
    let counted = rows(&session, "SELECT count(*) FROM orders").await?;
    assert_eq!(counted, [["90000"]]);
    Ok(())
}

#[tokio::test]
async fn an_unknown_table_is_not_found_by_its_full_name() -> TestResult {
    let fixture = orders().await?;
    let (_, session) = lake(&fixture).await?;
    let error = rows(&session, "SELECT * FROM lake.sales.missing")
        .await
        .err()
        .map(|error| error.to_string())
        .unwrap_or_default();
    assert!(error.contains("lake.sales.missing"), "{error}");
    assert!(error.contains("not found"), "{error}");
    Ok(())
}

#[tokio::test]
async fn refresh_picks_up_a_table_created_after_startup() -> TestResult {
    let fixture = orders().await?;
    let (provider, session) = lake(&fixture).await?;
    fixture.add_empty_table("returns").await?;
    assert!(!shows_table(
        &rows(&session, "SHOW TABLES").await?,
        "sales",
        "returns"
    ));

    provider.refresh().await?;
    let tables = rows(&session, "SHOW TABLES").await?;
    assert!(shows_table(&tables, "sales", "returns"), "{tables:?}");
    assert!(shows_table(&tables, "sales", "orders"), "{tables:?}");
    Ok(())
}

#[tokio::test]
async fn a_table_never_written_reads_as_empty() -> TestResult {
    let fixture = orders().await?;
    fixture.add_empty_table("returns").await?;
    let (_, session) = lake(&fixture).await?;
    let counted = rows(&session, "SELECT count(*), sum(amount) FROM returns").await?;
    assert_eq!(counted, [["0", ""]]);
    Ok(())
}

#[tokio::test]
async fn only_pg_catalog_can_be_added_to_the_lake() -> TestResult {
    let fixture = orders().await?;
    let (provider, session) = lake(&fixture).await?;
    let scratch = provider.register_schema("scratch", Arc::new(MemorySchemaProvider::new()));
    assert!(scratch.is_err(), "the lake catalog created a schema");

    provider.register_schema("pg_catalog", Arc::new(MemorySchemaProvider::new()))?;
    assert_eq!(provider.schema_names(), ["sales", "pg_catalog"]);
    assert!(provider.schema("pg_catalog").is_some());
    let counted = rows(&session, "SELECT count(*) FROM orders").await?;
    assert_eq!(counted, [["90000"]]);
    Ok(())
}

#[tokio::test]
async fn a_session_receipt_records_each_scan() -> TestResult {
    let fixture = orders().await?;
    let (_, session) = lake(&fixture).await?;
    let receipt = Arc::new(floe_scan::QueryReceipt::default());
    session
        .state_ref()
        .write()
        .config_mut()
        .set_extension(Arc::clone(&receipt));

    rows(&session, "SELECT count(*) FROM orders WHERE region = 'eu'").await?;
    let scans = receipt.take();
    let scan = scans.first().ok_or("no scan recorded")?;
    assert_eq!(scans.len(), 1);
    assert_eq!(scan.table(), "sales.orders");
    assert!(scan.files_scanned() > 0);
    assert!(scan.io_counts().bytes_read > 0);
    Ok(())
}

#[tokio::test]
async fn decimal_literals_compare_exactly_with_decimal_columns() -> TestResult {
    let fixture = orders().await?;
    let (_, session) = lake(&fixture).await?;
    let largest = rows(&session, "SELECT max(amount) FROM orders").await?;
    let largest = largest
        .first()
        .and_then(|row| row.first())
        .ok_or("no amounts")?
        .clone();
    let sql = format!("SELECT count(*) > 0 FROM orders WHERE amount = {largest}");
    assert_eq!(rows(&session, &sql).await?, [["true"]], "{sql}");
    assert_eq!(rows(&session, "SELECT 0.1 + 0.2 = 0.3").await?, [["true"]]);
    Ok(())
}
