# Changelog

## Unreleased

### Renamed: Floe is now Iceblink (ADR 0013)

- The binary is `iceblink` (was `floe`), the crates are `iceblink-*` (were `floe-*`), and the
  config file is `iceblink.toml` (was `floe.toml`). Start from `iceblink.example.toml`.
- `FLOE_*` environment variables are now `ICEBLINK_*`, such as `ICEBLINK_CATALOG_TOKEN` and
  `ICEBLINK_LAB`.
- Messages name Iceblink: `Iceblink is read-only: …`. The user agent is `iceblink/<version>`,
  and `server_version` ends `(Iceblink <version>)`.
- The lab checkout lives at `../iceblink-lab`, and the lab workflow reads the repository
  variable `ICEBLINK_LAB_REPO` (was `FLOE_LAB_REPO`).
- No query answer changed.
