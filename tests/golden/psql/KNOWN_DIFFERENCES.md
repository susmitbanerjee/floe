# Where psql against Iceblink differs from psql against Postgres

The golden files record what Iceblink answers today, including these differences. When one is
fixed, its golden file changes, and the reviewer checks the new output against Postgres.

| Command | Iceblink shows | Postgres shows | Cause | Issue |
|---|---|---|---|---|
| `\d sales.orders` | `created_at` as `text` | `timestamp with time zone` | datafusion-pg-catalog 0.18.3's `pg_attribute` maps only some Arrow types and falls back to `text`; it has no case for `TIMESTAMPTZ`. Query results are typed correctly (`tests/slt/types.slt`) | upstream, to file |
| `\d sales.orders` | `amount` as `numeric` | `numeric(14,2)` | the same table always writes `atttypmod = -1`, so precision and scale are lost | upstream, to file |
| region query | columns `count(*)` and `sum(sales.orders.amount)` | `count` and `sum` | DataFusion names unaliased aggregates after their expression; Postgres after the function | to file |
