# iceblink-testkit

Job: write real, deterministic Iceberg tables (Parquet data, an in-memory catalog, local files)
into a temp dir for Iceblink's tests.

Must never: be a normal dependency of an Iceblink crate (dev-dependency only), or read or write
anything outside its own temp dir.
