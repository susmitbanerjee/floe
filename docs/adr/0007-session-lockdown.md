# 0007. The lake catalog and a locked-down session

- Status: Accepted
- Date: 2026-11-20

## Context

psql users connect to database `lake` and expect schemas to be Iceberg namespaces and tables to
be Iceberg tables. DataFusion asks for names synchronously and loads tables asynchronously.
Floe also needs one session builder that can read lake tables and nothing else: defence in depth
ahead of the Postgres statement allow-list (Prompt 8).

Read from DataFusion 54.2.0:

- **Catalog traits.** `CatalogProvider` and `SchemaProvider` refuse `register_table`,
  `deregister_table` and `register_schema` by default. Their default `table_type()` loads the
  table, which SHOW TABLES calls for every table.
- **File-reading entry points:**
  - table factories for CREATE EXTERNAL TABLE (PARQUET, CSV, JSON, NDJSON, AVRO, ARROW);
  - file formats for COPY … TO (parquet, json, csv, arrow);
  - URL tables (off unless `enable_url_table()`);
  - an object store registry that pre-registers a `LocalFileSystem` rooted at `/`.
- **The default in-memory `datafusion.public` schema** accepts CREATE TABLE AS and CREATE VIEW.
- **The default disk manager** spills to the OS temp dir.
- **Default functions.** The only table functions are `generate_series` and `range`. None of the
  default scalar, nested, aggregate, window or table function crates references the
  filesystem, network, processes or object stores.

## Options

For the object store registry, which the prompt says should hold the configured bucket or test
dir:

1. **Register stores as asked.** Floe's scans never use them: they read through iceberg's FileIO
   and Floe's counting storage. A registered store only helps DataFusion's own file readers,
   which this session disables.
2. **A closed registry** that serves no store.

## Decision

- **Catalog.**
  - `LakeCatalogProvider` and `NamespaceProvider` serve a listing (namespace to table names)
    behind an `RwLock`, filled by `refresh()`.
  - Tables load on first use into `FloeTableProvider` and are kept until the next `refresh()`,
    which also forgets loaded tables, so a refresh picks up new snapshots too.
  - `table_type()` answers from the listing.
  - An unlisted table returns `None`, so DataFusion reports "table 'lake.sales.missing' not
    found".
- **The session** (`new_session`).
  - **Catalog:** only `lake`, which is also the default catalog. The default schema is the first
    namespace alphabetically, or `public`.
  - **No default in-memory catalog.**
  - **`information_schema` on:** pg_catalog needs it.
  - **`target_partitions` = available CPUs.**
  - **PostgreSQL SQL dialect:** psql clients.
  - **No table factories and no file formats.**
  - **URL tables left off.**
  - **The disk manager disabled:** an out-of-memory query fails rather than writing spill files.
  - **The default functions kept,** after the audit.
- **Object stores: option 2, maintainer's choice.** `ClosedObjectStoreRegistry` refuses every
  store. It drops and counts registrations, because the trait can't return an error there.
- **Signatures, simplified by the closed registry.**
  - `new_session(catalog)` takes no `StorageSettings`; each `FloeTableProvider` gets them from the
    catalog provider.
  - It returns `Result`, because DataFusion's runtime builder can fail.
  - `refresh(&self)` uses the catalog the provider already holds for lazy loads, so listings and
    loads can't come from different catalogs.

## Consequences

- **What fails, and is tested to fail:** CREATE EXTERNAL TABLE, `read_csv`, URL tables,
  COPY … TO, CREATE TABLE AS, CREATE VIEW and CREATE SCHEMA. COPY writes no file, and the session
  has no local filesystem store.
- **Out-of-memory queries fail** instead of spilling. Revisit with a spill directory under the
  cache folder if large sorts need it.
- **Stale data until refresh.** New snapshots and new tables appear only after `refresh()`, until
  the v0.1 watcher.
- **Upgrades.** A DataFusion upgrade must repeat the audit: new default table functions or
  factories would be re-enabled by `with_default_features`.
