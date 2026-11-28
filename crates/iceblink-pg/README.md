# iceblink-pg

Job: serve Iceblink's `lake` session over the Postgres wire protocol, with connection and statement
limits, SQLSTATE errors, and one span per statement saying what it read (ADR 0008).

Must never: run a statement outside the allow-list (SELECT, EXPLAIN, SHOW, and SET for
approved session settings only), or show a client internal error text such as file paths.
