//! Suite 3: psql golden files. Runs real psql against an in-process Iceblink over the
//! iceblink-testkit `sales.orders` table and compares its output with
//! tests/golden/psql/<psql major>/*.out.
//!
//! Runs when `ICEBLINK_PSQL_BIN` names a psql, when `psql` is on PATH, or when
//! `ICEBLINK_PSQL=1` (which fails if no psql is found). Otherwise it skips with a message.
//! `UPDATE_GOLDEN=1` rewrites the golden files; reviewers must read the diff.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use datafusion::arrow::array::{Array, AsArray};
use datafusion::arrow::datatypes::Decimal128Type;
use datafusion::arrow::util::display::array_value_to_string;
use iceblink_suites::{InProcess, SuiteError, workspace_root};
use iceblink_testkit::{Fixture, TableFixture};
use tokio::process::Command;

const REGION_QUERY: &str =
    "SELECT region, count(*), sum(amount) FROM sales.orders GROUP BY region ORDER BY region";

/// What each golden file holds: its name and the psql command.
const CASES: [(&str, &str); 4] = [
    ("dn", "\\dn"),
    ("dt_sales", "\\dt sales.*"),
    ("d_sales_orders", "\\d sales.orders"),
    ("region", REGION_QUERY),
];

fn say(message: &str) {
    let _ = writeln!(std::io::stdout(), "{message}");
}

/// The psql to run, or `None` to skip.
fn find_psql() -> Result<Option<PathBuf>, SuiteError> {
    if let Some(path) = std::env::var_os("ICEBLINK_PSQL_BIN") {
        return Ok(Some(PathBuf::from(path)));
    }
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("psql"))
            .find(|candidate| candidate.is_file())
    });
    match (
        on_path,
        std::env::var("ICEBLINK_PSQL").as_deref() == Ok("1"),
    ) {
        (Some(path), _) => Ok(Some(path)),
        (None, true) => {
            Err("ICEBLINK_PSQL=1 but no psql on PATH: install it or set ICEBLINK_PSQL_BIN".into())
        }
        (None, false) => Ok(None),
    }
}

/// The major version from `psql (PostgreSQL) 16.16 (Homebrew)`.
async fn major_version(psql: &Path) -> Result<String, SuiteError> {
    let output = Command::new(psql).arg("--version").output().await?;
    let text = String::from_utf8(output.stdout)?;
    text.split_whitespace()
        .nth(2)
        .and_then(|version| version.split('.').next())
        .map(str::to_owned)
        .ok_or_else(|| format!("can't read a version from {text:?}").into())
}

/// psql's output for `command`, without anything that changes between runs.
async fn run(psql: &Path, server: &InProcess, command: &str) -> Result<String, SuiteError> {
    let port = server.address().port().to_string();
    let output = Command::new(psql)
        .args([
            "-X",
            "-A",
            "-F,",
            "-h",
            "127.0.0.1",
            "-p",
            &port,
            "-U",
            "iceblink",
            "-d",
            "lake",
        ])
        .args(["-c", command])
        .env("PGCONNECT_TIMEOUT", "10")
        .env_remove("PGPASSWORD")
        .env_remove("PGOPTIONS")
        .env_remove("PGSERVICE")
        .output()
        .await?;
    let stdout = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;
    if !output.status.success() || stderr.contains("ERROR") {
        return Err(format!(
            "psql -c {command:?} failed ({}):\n{stderr}{stdout}",
            output.status
        )
        .into());
    }
    Ok(stdout
        .lines()
        .filter(|line| !line.starts_with("Time:"))
        .map(|line| format!("{}\n", line.trim_end()))
        .collect())
}

/// Region, row count and summed amount for each region, from the fixture's own generator.
fn expected_regions(fixture: &Fixture) -> Result<Vec<String>, SuiteError> {
    let mut totals: BTreeMap<String, (i64, i128)> = BTreeMap::new();
    for batch in fixture.batches()? {
        let regions = batch.column_by_name("region").ok_or("no region")?;
        let amounts = batch
            .column_by_name("amount")
            .ok_or("no amount")?
            .as_primitive::<Decimal128Type>();
        for row in 0..batch.num_rows() {
            let entry = totals
                .entry(array_value_to_string(regions, row)?)
                .or_default();
            entry.0 += 1;
            if !amounts.is_null(row) {
                entry.1 += amounts.value(row);
            }
        }
    }
    Ok(totals
        .into_iter()
        .map(|(region, (count, cents))| {
            format!("{region},{count},{}.{:02}", cents / 100, cents % 100)
        })
        .collect())
}

#[tokio::test(flavor = "multi_thread")]
async fn psql_output_matches_the_golden_files() -> Result<(), SuiteError> {
    let Some(psql) = find_psql()? else {
        say("skipped: no psql on PATH (set ICEBLINK_PSQL_BIN, or ICEBLINK_PSQL=1 to require it)");
        return Ok(());
    };
    let major = major_version(&psql).await?;
    let golden = workspace_root().join("tests/golden/psql").join(&major);
    let update = std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1");

    let fixture = TableFixture::new("sales", "orders")
        .partition_by_day("created_at")
        .days(30)
        .files_per_day(3)
        .rows_per_file(1_000)
        .seed(42)
        .build()
        .await?;
    let server = InProcess::start(fixture.catalog.clone(), &fixture.warehouse_prefix()).await?;

    let mut mismatches = Vec::new();
    for (name, command) in CASES {
        let output = run(&psql, &server, command).await?;
        let file = golden.join(format!("{name}.out"));
        if update {
            std::fs::create_dir_all(&golden)?;
            std::fs::write(&file, &output)?;
            say(&format!("wrote {}", file.display()));
        } else {
            let wanted = std::fs::read_to_string(&file).map_err(|error| {
                format!(
                    "{}: {error}. For psql {major}, run with UPDATE_GOLDEN=1 and review the files",
                    file.display()
                )
            })?;
            if output != wanted {
                mismatches.push(format!("{name}:\n--- golden\n{wanted}--- psql\n{output}"));
            }
        }
        if name == "region" {
            // The golden file can't hide a wrong answer: the rows must equal the generator's.
            let rows: Vec<&str> = output
                .lines()
                .skip(1)
                .take_while(|line| !line.starts_with('('))
                .collect();
            assert_eq!(
                rows,
                expected_regions(&fixture)?,
                "region query through psql"
            );
        }
    }
    server.stop().await?;
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches.join("\n").into())
    }
}
