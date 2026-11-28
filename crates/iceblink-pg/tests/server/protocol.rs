//! Types, parameters, refusals, settings, limits and shutdown, as a client sees them.

use std::collections::HashMap;
use std::time::Duration;

use datafusion::arrow::array::AsArray;
use datafusion::arrow::datatypes::{Decimal128Type, Int64Type};
use datafusion::arrow::util::display::array_value_to_string;
use futures::{StreamExt, pin_mut};
use iceblink_pg::ShutdownSummary;
use tokio_postgres::types::Type;

use crate::common::{TestResult, TestServer, failure, files_under, orders, rows};

#[tokio::test]
async fn serves_bigint_text_numeric_and_timestamptz() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;

    let columns = "SELECT order_id, region, amount, created_at FROM orders";
    let statement = client.prepare(columns).await?;
    let types: Vec<Type> = statement
        .columns()
        .iter()
        .map(|column| column.type_().clone())
        .collect();
    assert_eq!(
        types,
        [Type::INT8, Type::TEXT, Type::NUMERIC, Type::TIMESTAMPTZ]
    );

    let first = rows(&client, &format!("{columns} ORDER BY order_id LIMIT 1")).await?;
    let [row] = first.as_slice() else {
        return Err(format!("expected one row, got {first:?}").into());
    };
    let expected = first_order(&fixture.batches()?)?;
    assert_eq!(row[..3], expected[..3]);
    // Postgres prints `2026-01-01 03:12:45…+00`, Arrow `2026-01-01T03:12:45…Z`.
    let (served, generated) = (&row[3], &expected[3]);
    assert_eq!(
        served.get(..10),
        generated.get(..10),
        "{served} vs {generated}"
    );
    assert_eq!(
        served.get(11..19),
        generated.get(11..19),
        "{served} vs {generated}"
    );
    Ok(())
}

/// The generator's row with the smallest order_id, as text.
fn first_order(
    batches: &[datafusion::arrow::record_batch::RecordBatch],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut best: Option<(i64, Vec<String>)> = None;
    for batch in batches {
        let ids = batch
            .column_by_name("order_id")
            .ok_or("no order_id")?
            .as_primitive::<Int64Type>();
        for row in 0..batch.num_rows() {
            if best.as_ref().is_some_and(|(id, _)| *id <= ids.value(row)) {
                continue;
            }
            let cents = batch
                .column_by_name("amount")
                .ok_or("no amount")?
                .as_primitive::<Decimal128Type>()
                .value(row);
            let text = |name: &str| -> Result<String, Box<dyn std::error::Error>> {
                let column = batch.column_by_name(name).ok_or("no column")?;
                Ok(array_value_to_string(column, row)?)
            };
            let cells = vec![
                ids.value(row).to_string(),
                text("region")?,
                format!("{}.{:02}", cents / 100, cents % 100),
                text("created_at")?,
            ];
            best = Some((ids.value(row), cells));
        }
    }
    Ok(best.ok_or("no rows")?.1)
}

#[tokio::test]
async fn binds_a_parameter_in_the_extended_protocol() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;

    let mut by_region: HashMap<String, i64> = HashMap::new();
    for batch in fixture.batches()? {
        let regions = batch.column_by_name("region").ok_or("no region")?;
        for row in 0..batch.num_rows() {
            *by_region
                .entry(array_value_to_string(regions, row)?)
                .or_default() += 1;
        }
    }
    let sql = "SELECT count(*) FROM orders WHERE region = $1";
    for region in ["eu-west-1", "us-east-1"] {
        let row = client.query_one(sql, &[&region]).await?;
        let counted: i64 = row.try_get(0)?;
        assert_eq!(Some(&counted), by_region.get(region), "{region}");
    }
    Ok(())
}

#[tokio::test]
async fn refuses_changing_statements_naming_them() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;
    let before = files_under(fixture.dir.path())?;
    let copy_target = fixture.dir.path().join("copied.csv");

    let refused = [
        ("CREATE TABLE t (a INT)".to_owned(), "CREATE TABLE"),
        (
            "INSERT INTO orders (order_id) VALUES (1)".to_owned(),
            "INSERT",
        ),
        (
            format!("COPY orders TO '{}'", copy_target.display()),
            "COPY",
        ),
        (
            "CREATE EXTERNAL TABLE t STORED AS CSV LOCATION '/etc/passwd'".to_owned(),
            "CREATE EXTERNAL TABLE",
        ),
        ("DROP TABLE orders".to_owned(), "DROP TABLE"),
    ];
    for (sql, kind) in &refused {
        let expected = format!("Iceblink is read-only: {kind} is not allowed.");
        let simple = failure(client.batch_execute(sql).await)?;
        assert_eq!(simple, ("25006".to_owned(), expected.clone()), "{sql}");
        let extended = failure(client.execute(sql.as_str(), &[]).await)?;
        assert_eq!(extended, ("25006".to_owned(), expected), "{sql} (extended)");
    }

    assert_eq!(files_under(fixture.dir.path())?, before, "a file changed");
    assert!(!copy_target.exists(), "COPY wrote a file");
    assert_eq!(
        rows(&client, "SELECT count(*) FROM orders").await?,
        [["90000"]]
    );
    Ok(())
}

#[tokio::test]
async fn search_path_picks_the_default_schema() -> TestResult {
    let fixture = orders().await?;
    // `archive` sorts before `sales`, so it is the default schema.
    fixture.add_empty_table_in("archive", "orders").await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;
    let count = "SELECT count(*) FROM orders";
    assert_eq!(rows(&client, count).await?, [["0"]]);

    client.batch_execute("SET search_path = sales").await?;
    assert_eq!(rows(&client, count).await?, [["90000"]]);
    assert_eq!(rows(&client, "SHOW search_path").await?, [["sales"]]);

    let unknown = failure(client.batch_execute("SET search_path = nope").await)?;
    assert_eq!(
        unknown,
        (
            "3F000".to_owned(),
            "schema \"nope\" does not exist".to_owned()
        )
    );
    assert_eq!(
        rows(&client, count).await?,
        [["90000"]],
        "the failed SET changed the path"
    );

    let work_mem = failure(client.batch_execute("SET work_mem = '64MB'").await)?;
    assert_eq!(work_mem.0, "0A000", "{}", work_mem.1);

    // Each connection keeps its own path.
    let other = server.connect().await?;
    assert_eq!(rows(&other, count).await?, [["0"]]);
    Ok(())
}

#[tokio::test]
async fn statement_timeout_can_only_be_lowered() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;
    let show = "SHOW statement_timeout";
    assert_eq!(rows(&client, show).await?, [["30s"]]);

    for raise in ["SET statement_timeout = '60s'", "SET statement_timeout = 0"] {
        let (code, message) = failure(client.batch_execute(raise).await)?;
        assert_eq!(code, "22023", "{raise}");
        assert!(message.contains("server limit is 30s"), "{message}");
    }
    assert_eq!(rows(&client, show).await?, [["30s"]]);

    client.batch_execute("SET statement_timeout = '5s'").await?;
    assert_eq!(rows(&client, show).await?, [["5s"]]);
    client
        .batch_execute("SET statement_timeout = DEFAULT")
        .await?;
    assert_eq!(rows(&client, show).await?, [["30s"]]);
    Ok(())
}

#[tokio::test]
async fn a_statement_past_its_timeout_is_canceled_and_the_server_carries_on() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;

    client.batch_execute("SET statement_timeout = 1").await?;
    let slow = "SELECT count(*) FROM generate_series(1, 100000000)";
    let (code, message) = failure(client.simple_query(slow).await)?;
    assert_eq!(code, "57014");
    assert_eq!(message, "canceling statement due to statement timeout");

    client
        .batch_execute("SET statement_timeout = DEFAULT")
        .await?;
    assert_eq!(rows(&client, "SELECT 1").await?, [["1"]]);
    let other = server.connect().await?;
    assert_eq!(
        rows(&other, "SELECT count(*) FROM orders").await?,
        [["90000"]]
    );
    Ok(())
}

#[tokio::test]
async fn connections_over_the_limit_get_53300() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 2).await?;
    let first = server.connect().await?;
    let second = server.connect().await?;
    assert_eq!(rows(&first, "SELECT 1").await?, [["1"]]);
    assert_eq!(rows(&second, "SELECT 2").await?, [["2"]]);

    let (code, message) = failure(server.connect().await)?;
    assert_eq!(code, "53300");
    assert!(
        message.contains("too many connections (limit 2)"),
        "{message}"
    );
    assert_eq!(rows(&first, "SELECT 3").await?, [["3"]]);
    Ok(())
}

#[tokio::test]
async fn an_internal_error_shows_only_a_request_id() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;
    let data_file = files_under(fixture.dir.path())?
        .into_iter()
        .map(|(path, _, _)| path)
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "parquet")
        })
        .ok_or("the fixture has no data files")?;
    std::fs::remove_file(&data_file)?;

    let (code, message) = failure(client.simple_query("SELECT sum(amount) FROM orders").await)?;
    assert_eq!(code, "XX000");
    assert!(message.contains("request ID q-"), "{message}");
    let dir = fixture.dir.path().display().to_string();
    for leak in [dir.as_str(), ".parquet", "panicked", "backtrace", "/"] {
        assert!(!message.contains(leak), "{message} shows {leak}");
    }
    assert_eq!(rows(&client, "SELECT 1").await?, [["1"]]);
    Ok(())
}

#[tokio::test]
async fn shutdown_drains_in_flight_statements_and_refuses_new_connections() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start(&fixture, 8).await?;
    let client = server.connect().await?;

    // About 19 MB of rows: far more than the socket buffers hold, so once the client stops
    // reading after the first row, the statement is still in flight.
    let total: i64 = 1_000_000;
    let sql = format!("SELECT value FROM generate_series(1, {total})");
    let streamed = client.query_raw(sql.as_str(), Vec::<i64>::new()).await?;
    pin_mut!(streamed);
    let first = streamed.next().await.ok_or("no first row")??;
    assert_eq!(first.try_get::<_, i64>(0)?, 1);

    server.shutdown.cancel();
    let (code, _) = failure(server.connect().await)?;
    assert_eq!(code, "57P03");

    let mut received = 1;
    while let Some(row) = streamed.next().await {
        row?;
        received += 1;
    }
    assert_eq!(received, total);
    let summary = server.task.await??;
    assert_eq!(
        summary,
        ShutdownSummary {
            drained: 1,
            cancelled: 0
        }
    );
    assert_eq!(summary.to_string(), "1 query drained, 0 cancelled");
    Ok(())
}

#[tokio::test]
async fn shutdown_cancels_statements_still_running_after_the_grace() -> TestResult {
    let fixture = orders().await?;
    let server = TestServer::start_with_grace(&fixture, 8, Duration::from_millis(200)).await?;
    let client = server.connect().await?;

    let sql = "SELECT value FROM generate_series(1, 1000000)";
    let streamed = client.query_raw(sql, Vec::<i64>::new()).await?;
    pin_mut!(streamed);
    streamed.next().await.ok_or("no first row")??;

    // The client stops reading, so the statement can't finish within the grace.
    server.shutdown.cancel();
    let summary = server.task.await??;
    assert_eq!(
        summary,
        ShutdownSummary {
            drained: 0,
            cancelled: 1
        }
    );
    let mut rest = Ok(());
    while let Some(row) = streamed.next().await {
        if let Err(error) = row {
            rest = Err(error);
            break;
        }
    }
    assert!(rest.is_err(), "the cancelled statement still completed");
    Ok(())
}
