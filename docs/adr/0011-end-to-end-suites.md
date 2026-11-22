# 0011. End-to-end suites: SQL logic, TPC-H, psql, and gated lab runs

- Status: Accepted
- Date: 2026-11-21

## Context

Prompt 10 asked for three suites in `tests/` at the workspace root, run by `cargo nextest
run`, that go through the Postgres wire:
- sqllogictest files;
- TPC-H at scale factor 0.01 against DuckDB's answers;
- psql golden files.

It also asked for gated runs against floe-lab and AWS, a lab workflow, and a coverage report.
Several facts differed from the prompt's assumptions:

- **App API.** `app::run` and `Deps` were `pub(crate)` in a binary-only crate, so nothing outside
  `floe` could start it in-process.
- **Root tests.** The workspace root is a virtual manifest, so it can't own `tests/*.rs` itself.
- **TPC-H batches.** tpchgen-arrow builds batches only for Arrow 54, 56, 59 and 60. Floe is on
  Arrow 58.
- **Expected answers.** floe-lab doesn't exist yet, so its DuckDB export script can't produce the
  answers.
- **Tools.** psql and DuckDB weren't installed, and psql's `\d` output changes between major
  versions.
- **Issue links.** The GitHub repository doesn't exist yet.

## Decision

- **`floe` gains a library target.**
  - It exports `start`, `run`, `Deps` (with `Deps::connect`), `Started::address` and
    `AppError`, plus `parse_config`, `load_config` and `run_cli`.
  - `main.rs` calls `run_cli`.
  - The binary has `doc = false` to avoid the lib/bin name collision in `cargo doc`.
- **`crates/floe-suites` (publish = false, `autotests = false`).** Its `[[test]]` targets point
  at the root `tests/slt.rs`, `tpch.rs`, `psql.rs` and `lab.rs`. The data sits beside them in
  `tests/slt`, `tests/expected` and `tests/golden`. `cargo nextest run --workspace` runs them,
  and the gate is unchanged.
- **floe-testkit `Warehouse`** writes any Arrow batches as an Iceberg table (field IDs assigned
  in order). `Fixture` shares its writer.
- **SQL logic.**
  - sqllogictest 0.29 drives `FloeDb`, an `AsyncDB` over tokio-postgres's simple protocol.
  - Errors read `SQLSTATE <code>: <message>`.
  - The dataset is literal rows in `floe-suites/src/shop.rs`, so every expected result can be
    worked out by hand.
- **TPC-H.**
  - tpchgen 4 (no dependencies) generates the rows; floe-suites builds the Arrow 58 batches with
    DuckDB's column types.
  - **Expected answers (maintainer's choice):** DuckDB's committed SF 0.01 queries and answers
    at tag v2.0.0, copied by `scripts/tpch-expected.sh`. The script moves to floe-lab later.
  - **Comparison:** integers and text exactly, other numbers within 0.01, an empty field as
    NULL, and in any order when the query has no ORDER BY.
  - **Known failures are strict.** A query in `KNOWN_FAILURES.md` must fail, and its issue
    column says "to file" until the repository exists.
- **psql.**
  - **Goldens per psql major** in `tests/golden/psql/<major>/`. They were generated with psql 16
    (Homebrew `postgresql@16`), the version GitHub's ubuntu-24.04 runners ship. CI sets
    `FLOE_PSQL=1`, so the suite can't silently skip there.
  - **Checked against the generator too.** The region query's rows are compared with aggregates
    computed from the fixture, so a golden file can't hide a wrong answer.
  - **Known differences** from Postgres are listed in `tests/golden/psql/KNOWN_DIFFERENCES.md`.
- **Gated runs.**
  - `tests/lab.rs` covers floe-lab (region CSV, and `files_scanned` against pyiceberg's count)
    and AWS (against Athena's result).
  - `docs/floe-lab.md` is the contract for the files floe-lab provides.
  - `.github/workflows/lab.yml` skips with a notice until `vars.FLOE_LAB_REPO` is set.
- **Coverage.** A CI job runs `cargo llvm-cov nextest`, report only (`continue-on-error`, no
  threshold).

## Consequences

- **Bugs the suites found, fixed with their tests:**
  - **Float literals.** The session parsed decimal literals as floats, so
    `price = 999999999999.99` matched nothing. Sessions now set
    `sql_parser.parse_float_as_decimal`, which is Postgres's (and DuckDB's) meaning.
  - **NULL in answer files.** An answer-file parsing bug read DuckDB's NULL row as no rows.
- **Differences from Postgres, recorded rather than fixed:**
  - **timestamptz text.** Floe always sends six fractional digits for timestamptz text
    (`00:00:00.000000+00`).
  - **psql `\d`.** It shows timestamptz columns as `text` and numeric without precision. That's
    datafusion-pg-catalog 0.18.3's `pg_attribute`; an issue is to be filed upstream.
  - **Aggregate names.** Unaliased aggregates are named after their expression (`count(*)`).
- **Disk.** Every suite is a test binary that links DataFusion. The full workspace build needs
  15–20 GB of `target/` with line-tables-only debuginfo.
- **Releases.** The suite package is never published; the `floe` library is, with the small API
  above.
