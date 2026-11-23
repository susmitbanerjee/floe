# Floe

Floe is an open-source Rust engine that serves Apache Iceberg tables over the Postgres wire
protocol, so any Postgres client can query a lakehouse without copying data. It is read-only by
contract: it never writes, deletes or commits user tables or objects, and every query can say
which snapshot, files and bytes it read. **Status: pre-alpha.** `floe serve` answers SQL over
an Iceberg REST catalog with data on S3 or MinIO, on loopback only and without authentication;
don't depend on it yet. Licensed under [Apache-2.0](LICENSE); see
[CONTRIBUTING.md](CONTRIBUTING.md) to help build it.

## Usage

Copy [floe.example.toml](floe.example.toml) to `./floe.toml`, point it at your catalog and
warehouse, then:

```bash
floe config check
```

```bash
floe serve
```

A successful start prints one line per step, then where to connect:

```
ok     config    ./floe.toml
ok     catalog   6 namespaces, 42 tables                   310ms
ok     postgres  listening on 127.0.0.1:5432
ready  postgres://127.0.0.1:5432/lake
next   psql -h 127.0.0.1 -U floe lake
```

| Command | What it does |
|---|---|
| `floe serve [--config PATH]` | Serves the catalog's tables until Ctrl-C or SIGTERM, then waits up to 10 s for running queries and cancels the rest. |
| `floe config check [--config PATH]` | Checks the config file without connecting to anything. |
| `floe --version` | Prints the version. |

Global flags: `--log-format text|json` (default `text`) and
`--log-level error|warn|info|debug|trace` (default `info`; `RUST_LOG` wins). Logs go to
stderr. Floe reads S3 and catalog credentials from the standard AWS chain (environment,
`AWS_PROFILE`, SSO, instance or pod role), never from the config file. When `[catalog.sigv4]`
names a `profile`, Floe reads the catalog and S3 with it. `catalog.uri` and `storage.endpoint`
must use https, except plain http to this machine (`127.0.0.1`, `[::1]` or `localhost`). On a
private network you control, such as Floe in a container reaching `http://minio:9000`, set
`allow_plain_http = true` in that URL's table
([ADR 0012](docs/adr/0012-plain-http-loopback-only.md)).

A failure says what went wrong and how to fix it, on stderr, and exits with:

| Exit code | Meaning |
|---|---|
| 0 | ok |
| 2 | bad usage |
| 69 | catalog unreachable or unauthorised |
| 70 | internal error (a bug in Floe) |
| 71 | cannot bind the Postgres listener |
| 78 | config invalid |

## Limits

- **Tables are fixed at first use.** `floe serve` lists the catalog's tables once, at startup,
  and loads each table's metadata the first time a query reads it. It keeps both until it
  restarts, so tables created later, and snapshots committed later, show up only after a
  restart. If the lake expires old snapshots and deletes their files, a long-running Floe can
  fail to read a table until it restarts. The snapshot watcher arrives in v0.1.
- **No authentication.** Floe listens on loopback only, and anyone who can connect there can
  query every table Floe can read. Password login arrives in v0.1.

## Testing

`cargo nextest run --workspace` runs every hermetic test, including three end-to-end suites
that go through the Postgres wire ([ADR 0011](docs/adr/0011-end-to-end-suites.md)):

- `tests/slt/*.slt`: SQL logic tests over a small hand-checked dataset;
- `tests/tpch.rs`: TPC-H at scale factor 0.01 against DuckDB's answers;
- `tests/psql.rs`: psql's output against golden files, when psql is installed.

The lab and AWS runs in `tests/lab.rs` are gated; [docs/floe-lab.md](docs/floe-lab.md) explains
them.
