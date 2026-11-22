//! Startup and shutdown with a floe-testkit catalog: a real listener on 127.0.0.1:0, a real
//! Postgres client, and a query in flight when the token is cancelled.

use std::error::Error;
use std::net::{Ipv4Addr, SocketAddr, TcpListener as StdListener};

use floe_testkit::{Fixture, TableFixture};
use futures::{StreamExt, pin_mut};
use tokio_postgres::NoTls;

use super::*;
use crate::cli::Failure;
use crate::config::from_toml;

type TestResult = Result<(), Box<dyn Error>>;

async fn orders() -> Result<Fixture, Box<dyn Error>> {
    Ok(TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(3)
        .rows_per_file(100)
        .build()
        .await?)
}

/// A floe.toml for `fixture`, listening on `pg_listen`.
fn config(fixture: &Fixture, pg_listen: &str) -> Result<ValidConfig, Box<dyn Error>> {
    let text = format!(
        r#"
        [server]
        pg_listen = "{pg_listen}"
        max_connections = 8
        statement_timeout = "30s"

        [catalog]
        uri = "http://127.0.0.1:8181/catalog"
        warehouse = "demo"

        [storage]
        region = "us-east-1"
        path_style = false
        allowed_prefixes = ["{prefix}"]
        allow_local_files = true
        "#,
        prefix = fixture.warehouse_prefix()
    );
    Ok(from_toml(&text, &|_| None)?)
}

fn deps(fixture: &Fixture) -> Deps {
    Deps {
        catalog: LakeCatalog::from_catalog(Arc::clone(&fixture.catalog)),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_query_in_flight_at_shutdown_is_drained() -> TestResult {
    let fixture = orders().await?;
    let started = start(config(&fixture, "127.0.0.1:0")?, deps(&fixture)).await?;
    let address = started.address();
    let shutdown = CancellationToken::new();
    let serving = tokio::spawn(started.serve(shutdown.clone()));

    let (client, connection) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(address.port())
        .user("floe")
        .dbname("lake")
        .connect(NoTls)
        .await?;
    let connection = tokio::spawn(connection);
    let counted = client.query_one("SELECT count(*) FROM orders", &[]).await?;
    assert_eq!(counted.try_get::<_, i64>(0)?, 300);

    // About 19 MB of rows, far more than socket buffers hold: once the client stops reading,
    // the statement stays in flight.
    let total: i64 = 1_000_000;
    let sql = format!("SELECT value FROM generate_series(1, {total})");
    let streamed = client.query_raw(sql.as_str(), Vec::<i64>::new()).await?;
    pin_mut!(streamed);
    streamed.next().await.ok_or("no first row")??;

    shutdown.cancel();
    let mut received = 1;
    while let Some(row) = streamed.next().await {
        row?;
        received += 1;
    }
    assert_eq!(received, total);
    let summary = serving.await??;
    assert_eq!(
        summary,
        ShutdownSummary {
            drained: 1,
            cancelled: 0
        }
    );
    assert_eq!(summary.to_string(), "1 query drained, 0 cancelled");
    connection.abort();
    Ok(())
}

#[tokio::test]
async fn a_port_in_use_fails_with_exit_71_and_names_the_port() -> TestResult {
    let fixture = orders().await?;
    let taken = StdListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let port = taken.local_addr()?.port();
    let config = config(&fixture, &format!("127.0.0.1:{port}"))?;

    let Err(error) = start(config, deps(&fixture)).await else {
        return Err("Floe started on a port that was already in use".into());
    };
    assert!(matches!(error, AppError::Bind { .. }), "{error:?}");
    let failure = Failure::app(&error);
    assert_eq!(failure.code(), 71);
    assert!(
        failure.fix().contains(&format!("port {port}")),
        "{}",
        failure.fix()
    );
    Ok(())
}

#[test]
fn the_catalog_line_counts_namespaces_and_tables() {
    let line = |namespaces, tables| catalog_summary(RefreshSummary { namespaces, tables });
    assert_eq!(line(6, 42), "6 namespaces, 42 tables");
    assert_eq!(line(1, 1), "1 namespace, 1 table");
    assert_eq!(line(0, 0), "0 namespaces, 0 tables");
}

#[test]
fn psql_gets_a_port_only_when_it_isnt_5432() -> Result<(), std::net::AddrParseError> {
    assert_eq!(
        psql_command("127.0.0.1:5432".parse()?),
        "psql -h 127.0.0.1 -U floe lake"
    );
    assert_eq!(
        psql_command("127.0.0.1:6543".parse()?),
        "psql -h 127.0.0.1 -p 6543 -U floe lake"
    );
    assert_eq!(
        psql_command("[::1]:5432".parse()?),
        "psql -h ::1 -U floe lake"
    );
    Ok(())
}

#[test]
fn bind_fixes_follow_the_cause() -> Result<(), std::net::AddrParseError> {
    let listen: SocketAddr = "127.0.0.1:80".parse()?;
    assert!(bind_fix(listen, io::ErrorKind::PermissionDenied).contains("1024 or above"));
    assert!(bind_fix(listen, io::ErrorKind::AddrNotAvailable).contains("127.0.0.1:80"));
    Ok(())
}
