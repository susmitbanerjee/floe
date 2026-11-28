//! The fixture writes the table it promises, and does it deterministically.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use floe_testkit::{Fixture, TableFixture};

type TestResult = Result<(), Box<dyn Error>>;

async fn orders(seed: u64) -> Result<Fixture, iceberg::Error> {
    TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(3)
        .files_per_day(2)
        .rows_per_file(50)
        .seed(seed)
        .build()
        .await
}

fn total_data_files(table: &iceberg::table::Table) -> Option<String> {
    table.metadata().current_snapshot().and_then(|snapshot| {
        snapshot
            .summary()
            .additional_properties
            .get("total-data-files")
            .cloned()
    })
}

/// A data file: its path relative to the fixture dir, and its bytes.
type DataFile = (PathBuf, Vec<u8>);

/// Every Parquet file under `dir`, sorted by path.
fn data_files(dir: &Path) -> Result<Vec<DataFile>, Box<dyn Error>> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "parquet")
            {
                let bytes = fs::read(&path)?;
                found.push((path.strip_prefix(dir)?.to_path_buf(), bytes));
            }
        }
    }
    found.sort();
    Ok(found)
}

#[tokio::test]
async fn writes_a_partitioned_table_and_appends_a_day() -> TestResult {
    let mut fixture = orders(42).await?;
    let table = fixture.load_table().await?;
    assert_eq!(table.identifier().to_string(), "sales.orders");
    assert_eq!(total_data_files(&table).as_deref(), Some("6"));
    let first_snapshot = table.metadata().current_snapshot_id();

    fixture.append_day().await?;
    let table = fixture.load_table().await?;
    assert_eq!(total_data_files(&table).as_deref(), Some("8"));
    assert_ne!(table.metadata().current_snapshot_id(), first_snapshot);
    Ok(())
}

#[tokio::test]
async fn the_same_seed_writes_the_same_data_bytes() -> TestResult {
    let first = orders(42).await?;
    let again = orders(42).await?;
    let other = orders(43).await?;
    let first_files = data_files(first.dir.path())?;
    assert_eq!(first_files.len(), 6);
    assert_eq!(first_files, data_files(again.dir.path())?);
    let other_files = data_files(other.dir.path())?;
    assert_eq!(
        first_files.iter().map(|(path, _)| path).collect::<Vec<_>>(),
        other_files.iter().map(|(path, _)| path).collect::<Vec<_>>(),
        "file names depend only on day and file number"
    );
    assert_ne!(
        first_files, other_files,
        "a different seed writes different rows"
    );
    Ok(())
}
