# 0013. Rename Floe to Iceblink

- Status: Accepted
- Date: 2026-11-28

## Context

The project is now Iceblink. The GitHub repositories are renamed, and iceblink-lab has
renamed its own side (its ADR 0004). This repository never used the interim name RocketLake,
so there is no earlier rename ADR to supersede. Nobody runs the project yet, so the rename
keeps no compatibility aliases.

`git grep -n -I -i -E 'floe|rocketlake'` at 63461fa found 738 lines in 116 files, all `floe`
or `Floe`:

| Group | Lines | What |
|---|---|---|
| Manifests and lock | 54 | package names, `[workspace.dependencies]` paths, the `[[bin]]` name |
| Rust source and tests | 449 | `use` paths, 10 type names, strings, errors and fix lines, test names |
| SQL logic tests | 21 | error text in `read_only.slt`, comments |
| Config example | 10 | `floe.example.toml` itself and `env:FLOE_CATALOG_TOKEN` |
| Markdown | 171 | README, AGENTS, CONTRIBUTING, SECURITY, the PR template, crate READMEs, docs/, 94 in ADRs |
| CI, scripts, deny.toml, NOTICE | 33 | the workflows, scripts and deny.toml comments, NOTICE |

The pull request lists them by group.

## Options

1. Rename the brand everywhere mechanically, Rust type names included.
2. Rename the brand, and make Rust type and function names brand-free, so a later rename
   touches strings and docs only.

## Decision

Option 2. Apply this map and nothing else:

| Old | New |
|---|---|
| Floe, RocketLake (prose) | Iceblink |
| binary `floe` | `iceblink` |
| crates `floe`, `floe-catalog`, `floe-scan`, `floe-pg`, `floe-testkit`, `floe-suites` | `iceblink`, `iceblink-catalog`, `iceblink-scan`, `iceblink-pg`, `iceblink-testkit`, `iceblink-suites` |
| `floe.toml`, `floe.example.toml` | `iceblink.toml`, `iceblink.example.toml` |
| `FLOE_*`, `ROCKETLAKE_*` environment variables | `ICEBLINK_*` |
| user agent `floe/<version>` | `iceblink/<version>` |
| CLI tag ` floe ` | ` iceblink ` |
| psql user `floe` in docs, tests and the `next` line | `iceblink` |
| `server_version` suffix `(Floe <version>)` | `(Iceblink <version>)` |
| typetag names `floe-*` | `iceblink-*` |
| `docs/floe-lab.md`, path `../floe-lab` | `docs/iceblink-lab.md`, `../iceblink-lab` |
| repository variable `FLOE_LAB_REPO` | `ICEBLINK_LAB_REPO` |

`floe-suites` is not in the brief's table; it follows the same pattern.

Rust type and function names never carry the brand:

| Old | New |
|---|---|
| `FloeTableProvider`, `FloeScanExec` (iceblink-scan) | `LakeTableProvider`, `LakeScanExec` |
| `FloeHandlers`, `FloeHandler`, `FloeStartup`, `FloeErrorHandler`, `FloeParser`, `FloeHook` (iceblink-pg) | `PgHandlers`, `PgHandler`, `PgStartup`, `PgErrorHandler`, `PgParser`, `PgHook` |
| `floe_code` (iceblink-pg) | `own_code` |
| `FloeDb`, `FloeDbError` (iceblink-suites) | `LakeDb`, `LakeDbError` |

The exec node's display line becomes `LakeScanExec: sales.orders snapshot=... files=.../...`.
Test names and local variables that held the brand say what they hold instead.

Unchanged: the database `lake`, the warehouse layout, every expected answer under
`tests/expected/`, the psql golden files (they name no product), and ADR numbers.

Not in this repository yet, so nothing to rename: a state folder (`_floe/`), a system schema,
metrics, the MCP server, the Slack bot, Studio, a release workflow and `install.sh`. Later
changes create them under the new names. A state folder name or system schema name will each
live in one constant (`STATE_DIR = "_iceblink"` for the folder).

`scripts/check-legacy-name.sh`, copied from iceblink-lab, runs in `scripts/gate.sh` and in CI.
It fails if `git grep -n -I -i -E 'floe|rocketlake'` finds anything, or a file name matches,
outside the allow-list: rename ADRs (`docs/adr/*-rename-to-*.md`), `CHANGELOG.md`, and the
script itself, which holds the pattern. Unlike the lab's copy it does not pass `-w`: here the
old name also hid inside identifiers such as `FloeScanExec` and `FLOE_LAB`, which a word match
misses.

## Consequences

- Every command, path and variable in docs changes: `iceblink serve`, `iceblink.toml`,
  `ICEBLINK_LAB=1`. Local checkouts of the lab move to `../iceblink-lab`.
- Error text changes, so the `.slt` expectations and the CLI tests change with it. No query
  answer changes.
- The lab workflow reads the repository variable `ICEBLINK_LAB_REPO`; the old `FLOE_LAB_REPO`
  must be recreated under the new name in the GitHub settings.
- A later rename edits strings and docs only; the guard finds every place.
