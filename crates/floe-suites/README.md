# floe-suites

Job: run Floe's end-to-end suites through the Postgres wire. The suites and their data live in
the workspace root's `tests/` (SQL logic, TPC-H, psql, and the gated lab runs); this crate holds
their support code: an in-process Floe, the datasets, and the answer comparison.

Must never: assert on results pasted from an unchecked run, or reach the network outside the
gated (`#[ignore]`) lab and AWS tests.
