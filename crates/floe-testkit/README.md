# floe-testkit

Job: write real, deterministic Iceberg tables (Parquet data, an in-memory catalog, local files)
into a temp dir for Floe's tests.

Must never: be a normal dependency of a Floe crate (dev-dependency only), or read or write
anything outside its own temp dir.
