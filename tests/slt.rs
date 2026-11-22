//! Suite 1: SQL logic tests. Each `.slt` file in tests/slt runs against its own in-process Floe
//! over the shop dataset, statement by statement through tokio-postgres.

use std::path::PathBuf;

use floe_suites::{SuiteError, run_file, workspace_root};

fn slt(name: &str) -> PathBuf {
    workspace_root().join("tests/slt").join(name)
}

#[tokio::test(flavor = "multi_thread")]
async fn basics() -> Result<(), SuiteError> {
    run_file(&slt("basics.slt")).await
}

#[tokio::test(flavor = "multi_thread")]
async fn types() -> Result<(), SuiteError> {
    run_file(&slt("types.slt")).await
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only() -> Result<(), SuiteError> {
    run_file(&slt("read_only.slt")).await
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog() -> Result<(), SuiteError> {
    run_file(&slt("catalog.slt")).await
}
