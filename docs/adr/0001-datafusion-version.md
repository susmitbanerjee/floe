# 0001. Pin DataFusion 54 across all three library families

- Status: Accepted
- Date: 2026-11-19

## Context

Iceblink combines three library families that must share one DataFusion major, and with it one
Arrow. Otherwise the types can't cross crate boundaries and the binary carries two copies of
Arrow:

1. DataFusion itself, with Arrow and Parquet following it.
2. The Iceberg DataFusion integration (`iceberg-datafusion`), plus `iceberg` and
   `iceberg-catalog-rest` at the matching version. It has lived in apache/iceberg-rust and is
   moving to a new apache/datafusion-iceberg repository.
3. `datafusion-postgres`, with `datafusion-pg-catalog` and `arrow-pg` from the same repository.

Read on 2026-11-19 from the crates.io index and API, and cross-checked on docs.rs:

| Crate | Version | datafusion req | arrow req | Released | MSRV |
|---|---|---|---|---|---|
| datafusion (newest) | 55.3.0 | — | ^59.2.0 | 2026-11-17 | 1.94.0 |
| **datafusion** (54 line) | **54.2.0** | — | ^58.3.0 | 2026-11-17 | 1.88.0 |
| arrow, arrow-array, parquet (newest) | 60.1.0 | — | — | 2026-10-28 | 1.88 |
| **arrow, arrow-array, parquet** (resolved) | **58.4.2** | — | — | 2026-11-17 | 1.88 |
| **iceberg-datafusion** (apache/iceberg-rust, newest) | **0.11.0** | ^54 | parquet ^58.4 | 2026-11-03 | 1.95 |
| **iceberg** | **0.11.0** | — | arrow-* ^58.4 | 2026-11-03 | 1.95 |
| **iceberg-catalog-rest** | **0.11.0** | — | — | 2026-11-03 | 1.95 |
| apache/datafusion-iceberg | no release or tag | main: ^55.0.0 | — | repo created 2026-09-09 | — |
| datafusion-postgres (newest) | 0.19.0 | ^55 | — | 2026-10-20 | 1.89 |
| **datafusion-postgres** | **0.18.0** | ^54 | — | 2026-07-19 | 1.89 |
| datafusion-pg-catalog (newest) | 0.19.0 | ^55 | — | 2026-10-20 | 1.89 |
| **datafusion-pg-catalog** | **0.18.3** | ^54 | — | 2026-07-23 | 1.89 |
| arrow-pg (newest) | 0.16.0 | ^55 | ^59 | 2026-10-20 | 1.89 |
| **arrow-pg** | **0.15.0** | ^54 | ^58 | 2026-07-19 | 1.89 |

Bold rows are what Cargo.lock resolves to. apache/datafusion-iceberg's main branch builds
`iceberg-datafusion` 0.12.0 against DataFusion 55.0.0 but takes `iceberg` and
`iceberg-catalog-rest` from git (rev 5b82ab9), and nothing is published yet. So the newest
released integration is still 0.11.0 from apache/iceberg-rust, and it stops at DataFusion 54.

## Options

1. **The newest major all three share: DataFusion 54.** All released crates, no git.
   One major behind DataFusion and one release behind datafusion-postgres.
2. **A git pin for DataFusion 55.** Pin apache/datafusion-iceberg at a commit, which also
   pulls iceberg from git. Two git dependencies, an unreleased API, and an ADR for each.
3. **Vendor the glue.** Copy the table-provider glue from iceberg-datafusion and own it. Most
   work and most risk, for one DataFusion major.

## Decision

Option 1: DataFusion 54. It exists, so the git and vendoring options aren't needed.

Pins, each a caret on the major in `[workspace.dependencies]` with `default-features = false`:
`datafusion = "54"`, `iceberg`, `iceberg-catalog-rest` and `iceberg-datafusion = "0.11"`, and
`datafusion-postgres = "0.18"`. `arrow-pg` and `datafusion-pg-catalog` arrive through
datafusion-postgres, which re-exports both, and Arrow is used through `datafusion::arrow`. Neither
is declared directly. Cargo.lock is the exact pin.

Each crate sits in the crate that will own it, as a normal dependency so the licence check sees
it: `iceblink-catalog` (iceberg, iceberg-catalog-rest), `iceblink-scan` (datafusion, iceberg-datafusion)
and `iceblink-pg` (datafusion-postgres).

Features Iceblink turns on, and why:

- `datafusion/parquet`: read Iceberg data files, which are Parquet.
- `datafusion/sql`: plan SQL text into DataFusion logical plans.
- `tokio/macros` and `tokio/rt-multi-thread`, in iceblink-scan's dev-dependencies only, for the
  compile-together test's multi-threaded runtime.

Nothing else. The iceberg 0.11 crates have no features. datafusion-postgres has only `postgis`,
which stays off.

What actually gets compiled is more: iceberg-datafusion, datafusion-postgres,
datafusion-pg-catalog and arrow-pg all depend on `datafusion` with its default features, and
Cargo unifies features. So DataFusion's whole default set is on: compression (bzip2, gzip, xz,
zstd), the crypto, datetime, encoding, nested, regex, string and unicode functions, and
recursive_protection. Iceblink can't turn these off without forking those crates. pgwire comes in
with `server-api-ring`, so TLS uses ring, not aws-lc.

`crates/iceblink-scan/tests/compile_together.rs` proves the pins fit. It runs `SELECT 1` on a
SessionContext with an in-memory table, and checks with trait bounds that iceberg-datafusion's
providers implement Iceblink's DataFusion traits and that `RestCatalog` implements Iceblink's
`iceberg::Catalog`. It also builds a `RestCatalogBuilder`, a `ServerOptions` and an unpolled
`serve` future. If any family resolves to a different DataFusion or iceberg, it fails to compile.

## Consequences

Supply-chain exceptions this pin brings, all in deny.toml with comments:

- Licences: `ar_archive_writer` (Apache-2.0 WITH LLVM-exception, build-time only),
  `x509-certificate` (MPL-2.0, from pgwire), and `libbz2-rs-sys` (bzip2-1.0.6: permissive but not
  OSI-listed; maintainer approved on 2026-11-19).
- Unmaintained advisories, ignored until 2027-02-17 under ADR 0002: rustls-pemfile
  (RUSTSEC-2025-0134, datafusion-postgres), paste (RUSTSEC-2024-0436, parquet), chrono
  (RUSTSEC-2026-0543), chrono-tz (RUSTSEC-2026-0555) and humantime (RUSTSEC-2026-0542).
  No vulnerability advisories.

API facts the next prompts must work with:

- `RestCatalogConfig` is `pub(crate)` in iceberg-catalog-rest 0.11. Configure a catalog through
  `RestCatalogBuilder` and a properties map (`REST_CATALOG_PROP_URI`,
  `REST_CATALOG_PROP_WAREHOUSE`). `CatalogBuilder::load` is the call that contacts the server.
- iceberg 0.11 uses reqwest 0.12 with no TLS feature. The catalog prompt must pass in a
  rustls-backed client with connect and total timeouts through `RestCatalogBuilder::with_client`.
- `datafusion_postgres::serve(Arc<SessionContext>, &ServerOptions)` is the entry point, and
  `ServerOptions` defaults to 127.0.0.1:5432.

Upgrade policy: once a quarter, one PR moves all three families together. Re-read this table
first, keep `cargo tree -i datafusion` and `cargo tree -i arrow-array` at one version each, run
the compile-together test, and update this ADR. Don't move one family alone.

What to watch:

- The first release from apache/datafusion-iceberg, expected as iceberg-datafusion 0.12 on
  DataFusion 55. With datafusion-postgres 0.19 already on 55, that unblocks the move to 55.
  Check whether iceberg 0.12 is released by then too: the integration's main branch takes it
  from git today.
- Whether iceberg-datafusion keeps publishing from apache/iceberg-rust after the move, so Iceblink
  follows the right repository.
- datafusion-postgres dropping rustls-pemfile for rustls-pki-types' `PemObject`. It still
  depends on it in 0.19.
- The chrono family advisories: Arrow and DataFusion migrating to jiff or time would clear three
  of the ignores.
