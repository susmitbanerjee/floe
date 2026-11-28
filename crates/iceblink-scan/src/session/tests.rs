//! The session can read lake tables and nothing else: every file-reading or writing statement
//! fails, and the closed registry never serves a store.

use std::error::Error;

use datafusion::execution::object_store::ObjectStoreUrl;
use iceblink_catalog::LakeCatalog;
use iceblink_testkit::TableFixture;

use super::*;
use crate::StorageSettings;

#[tokio::test]
async fn file_reading_and_writing_statements_all_fail() -> Result<(), Box<dyn Error>> {
    let fixture = TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(1)
        .rows_per_file(10)
        .build()
        .await?;
    let storage = StorageSettings {
        endpoint: None,
        region: "us-east-1".to_owned(),
        path_style: false,
        allowed_prefixes: vec![Url::parse(&fixture.warehouse_prefix())?],
        allow_local_files: true,
        aws_profile: None,
    };
    let lake = Arc::new(LakeCatalog::from_catalog(Arc::clone(&fixture.catalog)));
    let catalog = Arc::new(LakeCatalogProvider::new(lake, storage));
    catalog.refresh().await?;
    let registry = Arc::new(ClosedObjectStoreRegistry::default());
    let session = session_with_registry(catalog, Arc::clone(&registry))?;

    let copy_target = fixture.dir.path().join("copied.csv");
    let statements = [
        "CREATE EXTERNAL TABLE t STORED AS CSV LOCATION '/etc/passwd'".to_owned(),
        "SELECT * FROM read_csv('/etc/passwd')".to_owned(),
        "SELECT * FROM '/etc/passwd'".to_owned(),
        format!("COPY (SELECT 1) TO '{}'", copy_target.display()),
        "CREATE TABLE t AS SELECT 1".to_owned(),
        "CREATE VIEW v AS SELECT 1".to_owned(),
        "CREATE SCHEMA lake.scratch".to_owned(),
    ];
    for sql in &statements {
        let outcome = match session.sql(sql).await {
            Ok(frame) => frame.collect().await.map(|_| ()),
            Err(error) => Err(error),
        };
        assert!(outcome.is_err(), "{sql} should have failed");
    }
    assert!(!copy_target.exists(), "COPY wrote a file");

    // And the session has no store to read local files with, even if a path got that far.
    let local = session
        .runtime_env()
        .object_store(ObjectStoreUrl::local_filesystem());
    assert!(local.is_err(), "the session can reach the local filesystem");
    assert!(registry.refused() >= 1);

    // Reading the lake still works.
    let counted = session
        .sql("SELECT count(*) FROM orders")
        .await?
        .collect()
        .await?;
    assert_eq!(counted.first().map(|batch| batch.num_rows()), Some(1));
    Ok(())
}
