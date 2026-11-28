//! The `floe` command: serves Apache Iceberg tables over the Postgres wire protocol, read-only.
//! See `floe --help`; the work happens in the library's `run_cli`.

use std::process::ExitCode;

fn main() -> ExitCode {
    floe::run_cli()
}
