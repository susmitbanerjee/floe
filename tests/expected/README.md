# Expected answers

## TPC-H, scale factor 0.01 (`tpch-sf0.01/`)

`qNN.sql` are the 22 queries and `qNN.csv` their answers, exactly as DuckDB's `tpch` extension
serves them through `tpch_queries()` and `tpch_answers()`. They were copied from DuckDB's
repository, pinned to one commit, with:

```bash
scripts/tpch-expected.sh
```

- **Source:** `duckdb/duckdb`, tag `v2.0.0`, commit `d54cbbe3df17e8e5ce68312f483b4155836112aa`,
  paths `extension/tpch/dbgen/queries/qNN.sql` and `extension/tpch/dbgen/answers/sf0.01/qNN.csv`.
- **Format:** `|`-separated with a header row. An empty field is NULL, so `q17.csv`'s empty line
  is one row holding NULL.
- **Data:** `tests/tpch.rs` regenerates the tables with tpchgen 4 (crate `tpchgen`), which produces
  the same rows as the reference dbgen that DuckDB embeds.
- **Licences and use:** DuckDB is MIT-licensed; its notice is in
  [`tpch-sf0.01/LICENSE-duckdb`](tpch-sf0.01/LICENSE-duckdb). The queries and
  answers derive from the TPC-H specification. These are correctness tests, not TPC-H
  benchmark results, and may not be presented as such.

The script moves to iceblink-lab once it exists, as a DuckDB export (`CALL dbgen(sf = 0.01)`, then
the 22 queries). This README will then record its command and DuckDB version.

## Known failures

[`KNOWN_FAILURES.md`](KNOWN_FAILURES.md) lists TPC-H queries expected to fail, each with an
issue. The suite fails if a listed query starts passing.
