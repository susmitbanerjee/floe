//! Suite 2: TPC-H correctness. Generates scale factor 0.01 in-process (tpchgen, dbgen-identical
//! rows), writes it as Iceberg tables, runs all 22 queries through the Postgres wire, and
//! compares each answer with DuckDB's (tests/expected/tpch-sf0.01, see its README).
//!
//! Queries in KNOWN_FAILURES.md are expected to fail. One that passes fails the suite too, so
//! the list never goes stale.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use floe_suites::{InProcess, SuiteError, compare, has_order_by, parse, tpch, workspace_root};
use tokio_postgres::{Client, SimpleQueryMessage};

const SCALE_FACTOR: f64 = 0.01;

fn expected_dir() -> PathBuf {
    workspace_root().join("tests/expected/tpch-sf0.01")
}

/// Query names listed in KNOWN_FAILURES.md's table, such as `q07`.
fn known_failures() -> Result<BTreeSet<String>, SuiteError> {
    let text = std::fs::read_to_string(workspace_root().join("tests/expected/KNOWN_FAILURES.md"))?;
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix("| q"))
        .filter_map(|rest| rest.get(..2))
        .filter(|number| number.chars().all(|c| c.is_ascii_digit()))
        .map(|number| format!("q{number}"))
        .collect())
}

/// Every row of a simple-protocol result, cells as text.
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

/// Runs query `name` and compares it with its expected answer.
async fn check(client: &Client, name: &str) -> Result<(), String> {
    let read = |extension: &str| {
        std::fs::read_to_string(expected_dir().join(format!("{name}.{extension}")))
            .map_err(|error| format!("reading {name}.{extension}: {error}"))
    };
    let sql = read("sql")?;
    let expected = parse(&read("csv")?, '|');
    let actual = rows(client, sql.trim().trim_end_matches(';'))
        .await
        .map_err(|error| match error.as_db_error() {
            Some(database) => format!("{}: {}", database.code().code(), database.message()),
            None => error.to_string(),
        })?;
    compare(&actual, &expected, has_order_by(&sql))
}

#[tokio::test(flavor = "multi_thread")]
async fn tpch_sf001_answers_match_duckdb() -> Result<(), SuiteError> {
    let warehouse = tpch(SCALE_FACTOR).await?;
    let floe = InProcess::start(warehouse.catalog(), &warehouse.warehouse_prefix()).await?;
    let known = known_failures()?;

    // Four connections, each taking every fourth query.
    let mut workers = tokio::task::JoinSet::new();
    for worker in 0..4 {
        let client = floe.connect().await?;
        workers.spawn(async move {
            let mut outcomes = Vec::new();
            for number in (1..=22).skip(worker).step_by(4) {
                let name = format!("q{number:02}");
                let outcome = check(&client, &name).await;
                outcomes.push((name, outcome));
            }
            outcomes
        });
    }
    let mut outcomes = Vec::new();
    while let Some(done) = workers.join_next().await {
        outcomes.extend(done?);
    }
    outcomes.sort_by(|left, right| left.0.cmp(&right.0));
    floe.stop().await?;

    let mut scorecard = String::new();
    let mut problems = Vec::new();
    let passed = outcomes
        .iter()
        .filter(|(_, outcome)| outcome.is_ok())
        .count();
    for (name, outcome) in &outcomes {
        let listed = known.contains(name);
        let verdict = match (outcome, listed) {
            (Ok(()), false) => "pass".to_owned(),
            (Ok(()), true) => {
                problems.push(format!(
                    "{name} passes now: remove it from KNOWN_FAILURES.md"
                ));
                "pass (listed as a known failure)".to_owned()
            }
            (Err(reason), true) => format!("known failure: {reason}"),
            (Err(reason), false) => {
                problems.push(format!("{name}: {reason}"));
                format!("FAIL: {reason}")
            }
        };
        writeln!(scorecard, "{name}  {verdict}")?;
    }
    writeln!(
        scorecard,
        "TPC-H SF {SCALE_FACTOR}: {passed} of 22 match DuckDB"
    )?;
    std::io::stdout().write_all(scorecard.as_bytes())?;
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n").into())
    }
}
