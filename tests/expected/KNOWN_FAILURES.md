# Known TPC-H failures

Queries in this table are expected to fail `tests/tpch.rs`: the suite still runs them, and
fails if one starts passing, so the list stays true. Never delete a query from the suite; fix
it, or list it here with an issue.

Until the GitHub repository exists, the issue column says "to file" and the PR description
lists the issues to open.

| Query | Issue | Error | Cause |
|---|---|---|---|
