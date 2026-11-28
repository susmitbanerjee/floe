//! The `iceblink` command: serves Apache Iceberg tables over the Postgres wire protocol, read-only.
//! See `iceblink --help`; the work happens in the library's `run_cli`.

use std::process::ExitCode;

fn main() -> ExitCode {
    iceblink::run_cli()
}
