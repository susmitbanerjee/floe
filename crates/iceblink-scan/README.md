# iceblink-scan

Job: serve Iceberg tables to DataFusion as the `lake` catalog in a locked-down session, read
their files from S3 (or local files in tests), and count what each scan read (snapshot, files,
bytes).

Must never: write to storage (no puts, deletes or overwrites), or let a session read files
outside its Iceberg tables.
