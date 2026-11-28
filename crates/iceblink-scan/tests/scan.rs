//! `LakeTableProvider` end to end on a 30-day `sales.orders` fixture: pruning, byte counts,
//! read-only, snapshot pinning, concurrent attribution and EXPLAIN ANALYZE.

use std::any::Any;
use std::error::Error;
use std::sync::Arc;

use datafusion::arrow::util::pretty::pretty_format_batches;
use datafusion::physical_plan::{ExecutionPlan, execute_stream};
use datafusion::prelude::SessionContext;
use futures::StreamExt;
use iceblink_scan::{IoCounts, LakeScanExec, LakeTableProvider, StorageSettings};
use iceblink_testkit::{Fixture, TableFixture};
use url::Url;

type TestResult = Result<(), Box<dyn Error>>;

/// The last 7 of 30 days: 2026-01-24 to 2026-01-30.
const LAST_7_DAYS: &str = "SELECT count(*) AS n FROM orders \
                           WHERE created_at >= TIMESTAMP '2026-01-24T00:00:00Z'";
/// The first 10 days: 2026-01-01 to 2026-01-10.
const FIRST_10_DAYS: &str = "SELECT count(*) AS n FROM orders \
                             WHERE created_at < TIMESTAMP '2026-01-11T00:00:00Z'";

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

fn settings(fixture: &Fixture) -> Result<StorageSettings, url::ParseError> {
    Ok(StorageSettings {
        endpoint: None,
        region: "us-east-1".to_owned(),
        path_style: false,
        allowed_prefixes: vec![Url::parse(&fixture.warehouse_prefix())?],
        allow_local_files: true,
        aws_profile: None,
    })
}

/// A session with the fixture's current table registered as `orders`.
async fn session(fixture: &Fixture) -> Result<SessionContext, Box<dyn Error>> {
    let provider = LakeTableProvider::new(fixture.load_table().await?, &settings(fixture)?)?;
    let context = SessionContext::new();
    context.register_table("orders", Arc::new(provider))?;
    Ok(context)
}

/// The `LakeScanExec` somewhere in `plan`.
fn find_scan(plan: &Arc<dyn ExecutionPlan>) -> Option<&LakeScanExec> {
    let any: &dyn Any = plan.as_ref();
    any.downcast_ref::<LakeScanExec>()
        .or_else(|| plan.children().into_iter().find_map(find_scan))
}

/// Runs `sql`, streaming the result, and returns the summed row count of the first column
/// (a `count(*)`) or of the rows themselves, plus the executed plan.
async fn run(
    context: &SessionContext,
    sql: &str,
) -> Result<(u64, Arc<dyn ExecutionPlan>), Box<dyn Error>> {
    let plan = context.sql(sql).await?.create_physical_plan().await?;
    let mut stream = execute_stream(Arc::clone(&plan), context.task_ctx())?;
    let mut total = 0;
    while let Some(batch) = stream.next().await {
        let batch = batch?;
        let counts = batch
            .column(0)
            .as_any()
            .downcast_ref::<datafusion::arrow::array::Int64Array>();
        total += match counts {
            Some(counts) if sql.contains("count(*)") => counts.iter().flatten().sum::<i64>() as u64,
            _ => batch.num_rows() as u64,
        };
    }
    Ok((total, plan))
}

fn counts(plan: &Arc<dyn ExecutionPlan>) -> Result<IoCounts, Box<dyn Error>> {
    Ok(find_scan(plan)
        .ok_or("no LakeScanExec in the plan")?
        .io_counts())
}

#[tokio::test]
async fn the_last_week_prunes_files_and_counts_its_bytes() -> TestResult {
    let fixture = orders().await?;
    let context = session(&fixture).await?;
    let (rows, plan) = run(&context, LAST_7_DAYS).await?;
    let scan = find_scan(&plan).ok_or("no LakeScanExec in the plan")?;

    assert_eq!(rows, 7 * 3 * 1_000);
    assert_eq!(scan.files_total(), Some(90));
    assert_eq!(scan.files_scanned(), 21);
    let io = scan.io_counts();
    assert!(io.bytes_read > 0, "{io:?}");
    // Each scanned file needs at least its 8-byte tail, its footer and a column chunk, so data
    // reads must show up, not just the manifest list and manifests.
    assert!(io.get_requests > 2 * 21, "{io:?}");
    assert!(
        io.bytes_read <= scan.planned_file_bytes(),
        "read {} bytes, more than the {} bytes in the scanned files",
        io.bytes_read,
        scan.planned_file_bytes()
    );
    Ok(())
}

#[tokio::test]
async fn a_full_query_never_tries_to_write() -> TestResult {
    let fixture = orders().await?;
    let context = session(&fixture).await?;
    let (rows, plan) = run(&context, "SELECT * FROM orders").await?;
    assert_eq!(rows, 30 * 3 * 1_000);
    assert_eq!(counts(&plan)?.write_attempts, 0);
    Ok(())
}

#[tokio::test]
async fn a_query_stays_on_its_snapshot_while_a_day_is_appended() -> TestResult {
    let mut fixture = orders().await?;
    let context = session(&fixture).await?;
    let plan = context
        .sql("SELECT order_id FROM orders")
        .await?
        .create_physical_plan()
        .await?;
    let pinned = find_scan(&plan).ok_or("no scan")?.snapshot_id();
    let mut stream = execute_stream(plan, context.task_ctx())?;
    let mut rows = stream.next().await.ok_or("no first batch")??.num_rows();

    fixture.append_day().await?;
    while let Some(batch) = stream.next().await {
        rows += batch?.num_rows();
    }
    assert_eq!(rows, 30 * 3 * 1_000, "the running query keeps its snapshot");

    let fresh = session(&fixture).await?;
    let (rows, plan) = run(&fresh, "SELECT count(*) AS n FROM orders").await?;
    assert_eq!(rows, 31 * 3 * 1_000, "a new query sees the new day");
    assert_ne!(find_scan(&plan).ok_or("no scan")?.snapshot_id(), pinned);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_queries_count_only_their_own_bytes() -> TestResult {
    let fixture = orders().await?;
    let context = session(&fixture).await?;
    let (_, alone_week) = run(&context, LAST_7_DAYS).await?;
    let (_, alone_ten) = run(&context, FIRST_10_DAYS).await?;
    let expected = (counts(&alone_week)?, counts(&alone_ten)?);
    assert_ne!(expected.0.bytes_read, expected.1.bytes_read);

    let (week, ten) = tokio::join!(run(&context, LAST_7_DAYS), run(&context, FIRST_10_DAYS));
    let (_, week) = week?;
    let (_, ten) = ten?;
    assert_eq!((counts(&week)?, counts(&ten)?), expected);
    Ok(())
}

#[tokio::test]
async fn explain_analyze_shows_what_was_read() -> TestResult {
    let fixture = orders().await?;
    let context = session(&fixture).await?;
    let batches = context
        .sql(&format!("EXPLAIN ANALYZE {LAST_7_DAYS}"))
        .await?
        .collect()
        .await?;
    let shown = pretty_format_batches(&batches)?.to_string();
    for expected in [
        "LakeScanExec: sales.orders snapshot=",
        "files=21/90",
        "files_scanned=21",
        "files_total=90",
        "bytes_read=",
        "get_requests=",
    ] {
        assert!(
            shown.contains(expected),
            "missing {expected:?} in:\n{shown}"
        );
    }
    Ok(())
}
