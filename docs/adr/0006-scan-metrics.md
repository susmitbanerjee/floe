# 0006. Scan metrics and the read-only storage seam

- Status: Accepted
- Date: 2026-11-20

The prompt names this ADR 0003, which is taken (durations), so it is 0006.

## Context

Floe's promise is that every answer shows what it read. In phase 1, EXPLAIN ANALYZE shows on the
scan node:

- the snapshot ID;
- the files in the snapshot, and the files left after pruning;
- the bytes read from storage, and the requests made.

No scan may write. What the pinned iceberg 0.11 and iceberg-datafusion 0.11 offer:

- **Storage is a trait.** It has 11 methods (exists, metadata, read, reader, write, writer,
  delete, delete_prefix, delete_stream, new_input, new_output) and, like `StorageFactory`, is a
  `#[typetag::serde]` trait. `FileIO` builds its storage lazily through the factory.
  `InputFile` reads through its storage: `read()` for whole objects, or `reader()` returning a
  `FileRead` that serves byte ranges. There is no list or rename method.
- **iceberg's `ScanMetrics`** counts only the Arrow reader's data bytes. It counts no requests,
  and nothing read during planning (the manifest list and manifests).
- **The integration's scan** (`IcebergTableScan`) plans files inside `execute()`, keeps no
  metrics, and has a crate-private constructor. So it can't report files scanned at plan time,
  and it can't carry Floe's counters. Its filter conversion, `convert_filters_to_predicate`, is
  public.
- **iceberg's Parquet reader** prefetches 512 KiB to find a file's footer, then reads column
  chunks separately. In files smaller than that it reads the column bytes twice.

## Options

1. **Attribution.** (a) A per-query `FileIO` over shared inner storage with fresh counters;
   (b) global counters with per-query deltas, which concurrent queries make wrong; (c) iceberg's
   `ScanMetrics`, which is incomplete.
2. **Scan.** (a) The integration's provider, which can't report files scanned or carry counters;
   (b) driving iceberg's `TableScan` directly.
3. **Footer reads.** (a) Keep the 512 KiB default, which re-reads small files' bytes; (b) exact
   footer reads, which cost one more small GET per file.
4. **Storage.** (a) Local files now and S3 in its own prompt; (b) iceberg-storage-opendal now.

## Decision

- **Attribution: 1a.**
  - Each query gets a fresh `IoStats` and a new `FileIO`, from a `CountingStorageFactory` that
    wraps the provider's one shared inner storage.
  - The query's `Table` is rebuilt around that FileIO and marked read-only, so planning and data
    reads both count, into that query alone.
  - The counters are `#[serde(skip)]`, so a FileIO serialized elsewhere starts at zero rather
    than sharing counts.
- **Counters** (`IoCounts`):
  - `get_requests`: whole-object reads plus ranged reads.
  - `bytes_read`: the bytes those GETs returned, planning included.
  - `list_requests`: `exists` and `metadata` calls, since the trait has no listing call.
  - `write_attempts`: write, writer, delete, delete_prefix, delete_stream and new_output. They
    are counted and refused with `ScanError::ReadOnly` and never reach the inner storage. There
    is no rename to refuse.
- **Scan: 2b.** `FloeTableProvider::scan`:
  - pins `current_snapshot()` once;
  - checks the location guard;
  - builds the `TableScan` with `snapshot_id`, `select`, and `with_filter` from the integration's
    `convert_filters_to_predicate` (pushdown is exactly the integration's, `Inexact`, re-checked
    by DataFusion);
  - plans the files once, keeping the task list (metadata, not data).

  `FloeScanExec` then streams those tasks through `ArrowReaderBuilder::read`, never collecting
  batches.
- **Metrics.** `files_total` (the snapshot summary's `total-data-files`), `files_scanned`,
  `bytes_read`, `get_requests` and `output_rows`. The display line is
  `FloeScanExec: <table> snapshot=<id> files=<scanned>/<total>`.
- **Footer: 3b, maintainer's choice.** `with_metadata_size_hint(8)`: no byte is read twice, so
  `bytes_read` stays within the scanned files' sizes plus the manifests.
- **Storage: 4a, maintainer's choice.** `s3://` tables get `ScanError::Io` until the object-store
  prompt.
- **Location guard.** It runs in `scan()` on the table and metadata locations before any FileIO
  exists, and again on every storage call. A path must start with an allowed prefix (each ends
  in a slash). `file://` needs `allow_local_files`, and `..` segments are refused. Errors show
  only `scheme://first-segment/`.
- **Snapshots.** The provider serves one loaded table and pins its current snapshot per query.
  A newer snapshot appears when the table is loaded again. The DataFusion catalog (ADR 0007)
  loads each table on first use and keeps it until `LakeCatalogProvider::refresh()`. The
  snapshot watcher in v0.1 will refresh on its own.

## Consequences

- EXPLAIN ANALYZE on the 30-day fixture, last 7 days, shows `files=21/90`, about 500 KB read,
  and 107 GET requests (2 for planning, about 5 per file).
- **On S3**, exact footer reads mean a second small GET per file. Revisit with real latencies
  once S3 is wired.
- **The fixture's data files are byte-identical per seed; its metadata is not.** iceberg
  generates the table UUID, snapshot IDs and commit timestamps, so manifest sizes shift slightly
  between builds.
- **iceberg's `ScanMetrics` stays unused.** Re-check it at the next iceberg upgrade.
