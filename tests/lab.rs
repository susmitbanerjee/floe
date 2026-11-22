//! Gated runs, ignored by default (docs/floe-lab.md has the setup and the files they read).
//!
//! - `FLOE_LAB=1`: against floe-lab (Lakekeeper and MinIO). The region query must equal
//!   `$FLOE_LAB_DIR/checks/region.csv`, and EXPLAIN ANALYZE's `files_scanned` must equal
//!   `$FLOE_LAB_DIR/checks/region_files_scanned.txt`. Uses the `floe serve` at `FLOE_LAB_ADDR`
//!   when set (the lab workflow starts one), or starts Floe in-process and checks startup takes
//!   under a second.
//! - `FLOE_AWS=1`: the same query against the AWS Glue copy must equal
//!   `$FLOE_LAB_DIR/checks/athena_region.csv`.
//!
//! `FLOE_LAB=1 FLOE_LAB_DIR=../floe-lab cargo nextest run -p floe-suites --test lab
//! --run-ignored ignored-only`

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use floe::Deps;
use floe_suites::{InProcess, SuiteError, compare, connect, parse};
use tokio_postgres::{Client, SimpleQueryMessage};

const REGION_QUERY: &str =
    "SELECT region, count(*), sum(amount) FROM sales.orders GROUP BY region ORDER BY region";

/// floe-lab's services as its docker compose file publishes them.
const LAB_CONFIG: &str = r#"
[server]
pg_listen = "127.0.0.1:0"
max_connections = 8
statement_timeout = "60s"

[catalog]
uri = "http://127.0.0.1:8181/catalog"
warehouse = "demo"

[storage]
region = "us-east-1"
endpoint = "http://127.0.0.1:9000"
path_style = true
allowed_prefixes = ["s3://warehouse/"]
allow_local_files = false
"#;

fn enabled(variable: &str) -> bool {
    std::env::var(variable).as_deref() == Ok("1")
}

fn lab_file(relative: &str) -> Result<String, SuiteError> {
    let dir = std::env::var_os("FLOE_LAB_DIR")
        .map(PathBuf::from)
        .ok_or("set FLOE_LAB_DIR to a floe-lab checkout")?;
    let path = dir.join(relative);
    std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()).into())
}

async fn rows(client: &Client, sql: &str) -> Result<Vec<Vec<String>>, tokio_postgres::Error> {
    Ok(client
        .simple_query(sql)
        .await?
        .iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(
                (0..row.len())
                    .map(|index| row.get(index).unwrap_or("NULL").to_owned())
                    .collect(),
            ),
            _ => None,
        })
        .collect())
}

/// The `files_scanned=N` metric from an EXPLAIN ANALYZE plan.
fn files_scanned(plan: &str) -> Option<usize> {
    let (_, after) = plan.split_once("files_scanned=")?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Starts Floe in-process on `config_text`, timing startup as `floe serve` would.
async fn start(config_text: &str) -> Result<(InProcess, Duration), SuiteError> {
    let started = Instant::now();
    let config = floe::parse_config(config_text, &|name| std::env::var(name).ok())?;
    let deps = Deps::connect(&config).await?;
    let floe = InProcess::start_with(config, deps).await?;
    Ok((floe, started.elapsed()))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs floe-lab; run with FLOE_LAB=1 and FLOE_LAB_DIR"]
async fn floe_lab_region_query_and_files_scanned_match_the_checks() -> Result<(), SuiteError> {
    if !enabled("FLOE_LAB") {
        return Ok(());
    }
    let (address, floe) = match std::env::var("FLOE_LAB_ADDR") {
        Ok(address) => (address.parse::<SocketAddr>()?, None),
        Err(_) => {
            let (floe, startup) = start(LAB_CONFIG).await?;
            assert!(startup < Duration::from_secs(1), "startup took {startup:?}");
            (floe.address(), Some(floe))
        }
    };
    let client = connect(address).await?;

    let expected = parse(&lab_file("checks/region.csv")?, ',');
    compare(&rows(&client, REGION_QUERY).await?, &expected, true)?;
    assert_eq!(
        expected.rows.len(),
        5,
        "floe-lab's region check should have 5 regions"
    );

    let plan = rows(&client, &format!("EXPLAIN ANALYZE {REGION_QUERY}"))
        .await?
        .concat()
        .join("\n");
    let scanned = files_scanned(&plan).ok_or("no files_scanned in EXPLAIN ANALYZE")?;
    let wanted: usize = lab_file("checks/region_files_scanned.txt")?
        .trim()
        .parse()?;
    assert_eq!(scanned, wanted, "files_scanned against pyiceberg's count");

    if let Some(floe) = floe {
        floe.stop().await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs AWS; run with FLOE_AWS=1, FLOE_AWS_ACCOUNT_ID, FLOE_AWS_PREFIX and FLOE_LAB_DIR"]
async fn aws_glue_region_query_matches_athena() -> Result<(), SuiteError> {
    if !enabled("FLOE_AWS") {
        return Ok(());
    }
    let account = std::env::var("FLOE_AWS_ACCOUNT_ID")?;
    let prefix = std::env::var("FLOE_AWS_PREFIX")?;
    let region = std::env::var("FLOE_AWS_REGION").unwrap_or_else(|_| "ap-south-1".to_owned());
    let profile = std::env::var("FLOE_AWS_PROFILE")
        .map(|profile| format!("profile = \"{profile}\""))
        .unwrap_or_default();
    let config = format!(
        r#"
        [server]
        pg_listen = "127.0.0.1:0"
        max_connections = 8
        statement_timeout = "120s"

        [catalog]
        uri = "https://glue.{region}.amazonaws.com/iceberg"
        warehouse = "{account}"

        [catalog.sigv4]
        region = "{region}"
        service = "glue"
        {profile}

        [storage]
        region = "{region}"
        path_style = false
        allowed_prefixes = ["{prefix}"]
        allow_local_files = false
        "#
    );
    let (floe, _) = start(&config).await?;
    let client = floe.connect().await?;
    let expected = parse(&lab_file("checks/athena_region.csv")?, ',');
    compare(&rows(&client, REGION_QUERY).await?, &expected, true)?;
    floe.stop().await?;
    Ok(())
}

#[test]
fn reads_files_scanned_from_a_plan() {
    let plan = "FloeScanExec: sales.orders snapshot=7 files=3/90, metrics=[output_rows=10, \
                files_scanned=3, files_total=90]";
    assert_eq!(files_scanned(plan), Some(3));
    assert_eq!(files_scanned("ProjectionExec"), None);
}
