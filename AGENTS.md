# Iceblink engineering rules

## The product contract (never break)
- Read-only: no code path writes, deletes or commits user tables or objects. Tests assert zero write requests.
- No copies: Iceblink never copies source data. Caches and indexes are derived and disposable.
- Every query can say what it read: snapshot, files, bytes.
- Background work stays inside configured budgets (from v0.3).

## Keep it simple
- Build the smallest thing that passes the acceptance checks. No speculative features, flags or frameworks.
- Add a trait only when two implementations exist today, or the design names the seam
  (AuthProvider, AuditSink, PolicyStore, StudioModule, SkipIndex, RequestSigner). Otherwise use concrete types.
- Rule of three: duplicate twice, extract on the third.
- Prefer std and existing dependencies. A new dependency needs a reason in the PR
  (why, size, licence, maintenance) and goes in [workspace.dependencies] with default-features = false.
- No custom macros unless they remove real duplication in three or more places.

## Structure
- Crates depend one way: iceblink -> iceblink-pg -> iceblink-scan -> iceblink-catalog. No cycles, no upward imports.
- One module, one job. Aim for files under 400 lines, functions under 40 lines, at most 4 parameters
  (group the rest into a struct).
- Keep the public surface small: pub(crate) by default; lib.rs re-exports the crate's API only.
- Use the design's words: catalog, namespace, table, snapshot, receipt, principal, warehouse.
  No abbreviations except SQL, URL, ID, S3, AWS.
- Parse, don't validate: turn input into checked types at the boundary (ListenAddr, WarehouseUri,
  NamespaceName). Inner code trusts them.
- Pass dependencies in through constructors. No global mutable state, no singletons.

## Errors
- Each library crate has one Error enum (thiserror, #[non_exhaustive]). Variants say what failed and
  carry context (table, URI), never secrets.
- anyhow only in the iceblink binary's main and CLI glue, always with .context("what Iceblink was doing").
- No unwrap, expect, panic!, todo!, unimplemented! outside tests. A truly impossible case uses
  .expect("INVARIANT: <why>") under #[expect(clippy::expect_used, reason = "INVARIANT: <why>")]
  on the enclosing statement or function, and needs reviewer sign-off. Without the attribute the
  gate fails; if the expect later goes away, the leftover attribute fails the gate too.
- Messages a person sees say what happened, why, and what to do next.
  Postgres clients get the matching SQLSTATE.

## Security
- #![forbid(unsafe_code)] everywhere (workspace lint).
- Secrets are secrecy::SecretString. Never in logs, errors, Debug output or panics.
  Config refers to them as "env:NAME"; examples never hold real values.
- Every outbound call uses TLS (rustls) and has a connect timeout and a total timeout. The
  exceptions are plain http to this machine (a loopback address or localhost), and plain http to
  a URL whose table sets allow_plain_http = true, for private container networks
  (docs/adr/0012-plain-http-loopback-only.md).
  Retry only idempotent reads, with backoff and a cap.
- Treat clients and catalogs as untrusted:
  - SQL passes an allow-list: SELECT, EXPLAIN, SHOW, and SET for approved session settings only.
  - Table and file locations must sit under the configured warehouse. file:// is refused outside tests.
- Safe defaults: listen on 127.0.0.1. Without auth, loopback only.
- Log query fingerprints at info and full SQL only at debug. Never log row data.
- No shelling out, no dynamic code loading, no file reads except the config file and the cache folder.
- cargo deny check must pass (advisories, licences, bans, sources). Git dependencies only when an ADR
  records why, pinned to a commit SHA.

## Async and performance
- tokio multi-thread runtime. Never block inside async code.
- Stream record batches; don't collect whole results in memory.
- Every spawned task has an owner and stops on shutdown (JoinSet, CancellationToken).
- Measure before optimising, and keep the test or benchmark that shows the gain.

## Observability
- tracing only. No println! outside crates/iceblink/src/cli/output.rs.
- One span per query with: query_id, fingerprint, snapshot_ids, files_scanned, bytes_read, elapsed_ms.
- Levels: error needs a person; warn means degraded but working; info is lifecycle; debug is detail.

## Testing
- Write the test first for every behaviour change. Unit tests sit beside the code; integration tests in tests/.
- Hermetic by default: no network, Docker, wall clock or randomness unless injected.
  Lab tests are #[ignore] and run with ICEBLINK_LAB=1.
- Table-driven tests for parsers and validators.
- Test names describe behaviour, e.g. rejects_non_loopback_listen_without_auth.
- Every bug fix lands with the test that would have caught it.

## Style and docs
- rustfmt and clippy clean under the workspace lints.
- /// on every public item: what it does, why, and its errors. Comments explain why, not what.
- No commented-out code. TODO only with an issue link.
- Each crate has a README of 10 lines or fewer: its job and what it must never do.

## Working loop
- Small PRs: under 400 changed lines excluding tests and fixtures, where possible.
- Conventional commits with DCO sign-off (git commit -s).
- Before saying done, run the gate and paste the results:
  cargo fmt --check; cargo clippy --all-targets --all-features -- -D warnings;
  cargo nextest run; cargo deny check.
  Then list anything not done.
- Check every external API against the docs for the exact version in Cargo.lock. Never invent an API.
  If reality differs from the prompt, stop and write docs/adr/NNNN-title.md with the options and a
  recommendation.
