# 0008. The Postgres server: a hook, a handler wrapper and Iceblink's own accept loop

- Status: Accepted
- Date: 2026-11-20

## Context

`iceblink_pg::serve(session_factory, settings, shutdown)` serves the `lake` session (ADR 0007) to
psql and BI tools. The prompt asked for:
- datafusion-postgres's handlers and pg_catalog, hooked or wrapped, never forked;
- a statement allow-list;
- connection and statement limits;
- SQLSTATE-correct errors;
- one span per statement.

Read from datafusion-postgres 0.18.0, datafusion-pg-catalog 0.18.3, pgwire 0.40.7 and
arrow-pg 0.15.0:

- **Extension points.**
  - `QueryHook` sees every parsed statement: simple query, extended Parse and extended
    Execute.
  - `DfSessionService` and its `Parser` are public.
  - pgwire lets a server supply every handler, including an `ErrorHandler` that may rewrite any
    error before it is sent.
- **The library's `serve` doesn't fit.**
  - It loops forever, so there is no shutdown.
  - Over its connection limit it drops the socket without a word: no 53300.
  - It shares one session among all connections.
- **Statement timeout.** The library times out only `SessionContext::sql`, which is planning.
  Rows stream after the handler returns, so a slow aggregate ignored the limit.
- **Cancel.** pgwire's `ConnectionManager` cancels a statement only while the handler runs,
  which is planning. Once rows stream, a cancel request does nothing.
- **SET.**
  - `SetShowHook` sets `search_path` only from a quoted literal; `SET search_path = sales`
    silently does nothing.
  - Every unknown variable is answered "SET" and ignored.
- **Startup.** `NoopStartupHandler` always reports `16.6-pgwire-0.40.7`, `is_superuser=on` and
  `default_transaction_read_only=off`.
- **Errors.** `PgWireError::ApiError` reaches the client as XX000 with the raw error text,
  which can name data files.
- **pg_catalog.** `setup_pg_catalog` registers a `pg_catalog` schema on the `lake` catalog,
  which ADR 0007's provider refused. It also parses about 20 embedded Arrow tables each time it
  runs.

## Options

**Where the read-only rules sit:**
1. A `QueryHook` only.
2. A wrapper around `DfSessionService` only.
3. Both.
4. Fork datafusion-postgres. This was ruled out by the prompt.

**How each connection gets its session:**
1. A factory called per connection.
2. One template session cloned per connection, with pg_catalog set up once.

## Decision

**Option 3 for the read-only rules: a hook and a wrapper.**

**`PgHook`** runs first after the library's transaction hook. It holds:
- **The allow-list** (`allow.rs`):
  - **Run:** SELECT, WITH, VALUES, EXPLAIN [ANALYZE] of those, SHOW TABLES and SHOW COLUMNS.
  - **Iceblink's own settings:** SET for `statement_timeout`, `search_path` and `client_encoding`,
    and `SHOW statement_timeout`.
  - **Left to the library:** application_name, DateStyle, extra_float_digits, TimeZone, other
    SHOWs, BEGIN, COMMIT and ROLLBACK (no-ops).
  - **Refused:** everything else gets 25006, naming the kind. SET of anything else gets 0A000.
  - **SQL that doesn't parse** but starts with a changing verb still gets 25006, such as
    DataFusion's `CREATE EXTERNAL TABLE … STORED AS CSV`.
- **Simple-protocol queries.** The hook plans each one, checks the plan with DataFusion's
  `SQLOptions` (no DDL, no DML, no statements), and starts it.

**`PgHandler`** wraps `DfSessionService` for what a hook can't do:
- **Extended protocol.** Before running a portal it checks the plan with the same
  `SQLOptions`, then tracks the rows. Only the library knows the portal's result formats, so
  it runs them.
- **Shutdown.** It keeps a connection busy from its first message until the reply is flushed.
- **Unparsable SQL.** It maps parse errors to 25006 for changing verbs.

**Iceblink's own accept loop** around pgwire's `process_socket`:
- **Connection limit.** A semaphore. Over the limit, a connection is refused after its startup
  message with FATAL 53300, as Postgres does. Cancel requests still reach the server.
- **Shutdown.**
  1. The `CancellationToken` stops normal accepting.
  2. New statements get FATAL 57P01; new connections get FATAL 57P03.
  3. The server waits for busy connections, for at most `ServerSettings.shutdown_grace`
     (10 s from the binary; ADR 0010 replaced the earlier statement timeout plus 5 s). It
     returns a `ShutdownSummary` of statements drained and cancelled.
  4. Idle connections are closed.
- **Statement timeout.** It covers planning and every row (`statement.rs`).
  `SET statement_timeout` can only lower it: a higher value, or 0, gets 22023.

**Startup.** Iceblink reports:
- `server_version` = `16.6 (Iceblink <version>)`;
- `client_encoding` = UTF8;
- `integer_datetimes` = on;
- `DateStyle` = pgwire's default;
- `default_transaction_read_only` = on;
- `is_superuser` = off;
- `search_path` = the real default schema.

**Cancel: pgwire's `ConnectionManager` (maintainer's choice).** It stops a statement while it
plans. Stopping one that is already streaming rows is left to v0.1. Until then the statement
timeout bounds it.

**Sessions: a factory per connection (maintainer's choice).**
- Each connection calls the factory and runs `setup_pg_catalog`.
- `LakeCatalogProvider::register_schema` accepts only `pg_catalog`. Each setup replaces the
  previous one; all are equivalent, because every session lists the same `lake` provider.
- **Measured cost:** 160 ms minimum, 250 ms median and 500 ms maximum per connection (debug
  build, 20 runs, Apple Silicon). It runs on tokio's blocking pool so it never stalls the async
  workers.

**Missing tables.** An Iceberg table that was created but never written has no snapshot. It now
scans as empty instead of failing. The server tests found this.

**SQLSTATE codes** (`errors.rs`, the one mapping):

| SQLSTATE | When |
|---|---|
| 42601 | Parse error, with the parser's message |
| 42P01 | DataFusion `table '…' not found`, sent as `relation "…" does not exist` |
| 3F000 | `failed to resolve schema`; SET search_path with no existing schema (maintainer's choice) |
| 0A000 | DataFusion NotImplemented; SET of an unsupported setting or encoding |
| 25006 | A statement kind off the allow-list |
| 57014 | The statement's deadline passed, while planning or streaming |
| 53300 | Over `max_connections` (FATAL) |
| XX000 | Anything internal: "internal error (request ID q-00002a); the server log has the details". The detail is logged at error level under the same ID |
| *added* 22023 | `statement_timeout` above the limit, or unreadable |
| *added* 42703 / 42000 | Unknown column / other planning errors. They keep DataFusion's message, which names only SQL, rather than being hidden as XX000 |
| *added* 57P01 / 57P03 | A new statement / a new connection while shutting down (FATAL) |

**One span per statement.** It covers every query, refusal and Iceblink setting. Its fields:
- `query_id` and `fingerprint` (the AST with every literal as `?`);
- `snapshot_ids`, `files_scanned` and `bytes_read`, from a `QueryReceipt` that each scan records
  into;
- `rows`, `elapsed_ms` and `sqlstate`.

An info event marks its end, and the full SQL is logged at debug only. Statements the library
answers itself (BEGIN, SET DateStyle, SHOW search_path) get no span.

## Consequences

- **What is tested.** Every statement kind on and off the allow-list. Over both protocols,
  changing statements leave the warehouse untouched. Also tested:
  - types: bigint, text, numeric(14,2) and timestamptz;
  - parameters;
  - per-connection search_path;
  - timeouts while streaming;
  - the connection limit;
  - internal errors that show no path;
  - a drain that finishes a streaming query and refuses newcomers.
- **The library's internals are pinned.**
  - Iceblink relies on DataFusion's planning messages for 42P01 and 3F000.
  - Iceblink relies on the library's statement type in the extended protocol.
  - The unit tests pin both, but upgrading datafusion-postgres or DataFusion means re-reading
    `handlers.rs`, `hooks/set_show.rs` and `session_state.rs`.
- **Per-connection setup.** Every new connection costs a pg_catalog setup. Revisit with the
  template option if connection storms or release-build numbers make it matter.
- **Cancel.** It stops planning only, until v0.1.
- **Not handled yet:**
  - authentication and TLS (v0.1);
  - a `search_path` sent in the startup message;
  - RESET;
  - cursors, which are refused.
- **PR size.** The server is about 1,750 lines without tests, well over the 400-line guideline.
  The pieces only make sense tested together, behind one listener.
