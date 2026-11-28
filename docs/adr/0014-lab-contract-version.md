# 0014. Lab tests and iceblink-lab's contract version

- Status: Proposed
- Date: 2026-11-28

## Context

The rename brief asks the lab tests to read `../iceblink-lab/contract.json` and refuse any
version older than the one iceblink-lab's R1 published, with a message naming R1.

R1 published no contract. iceblink-lab's ADR 0004 lists contract.json under "not in this
repository yet", and its CHANGELOG says there is no contract version to bump. So there is no
file, format or version for Iceblink to check.

The lab has also moved away from what `docs/iceblink-lab.md` and `tests/lab.rs` expect, before
and apart from the rename:

- Its expected answers are under `checks/expected/<profile>/` (`region_7d.csv`, `files.json`,
  and others). It has no `checks/region.csv` or `checks/region_files_scanned.txt`.
- It ships no engine config or env file for Iceblink to start with (its ADR 0004 plans
  `engine-config` and `engine-env` targets).
- Its warehouse is `lab`, not `demo`, and its object store is RustFS, not MinIO.

So `ICEBLINK_LAB=1` cannot pass against today's lab, with or without the rename.

## Options

1. Define contract.json here (fields and a first version) and check it. This invents the lab's
   interface from this side, which AGENTS.md forbids, and the lab would have to follow.
2. iceblink-lab publishes contract.json first: a version number, and the paths of the files
   it provides. Then one Iceblink change reads it, refuses older versions with a message naming
   the release that introduced it, and moves `tests/lab.rs` and `docs/iceblink-lab.md` to the
   lab's current layout.
3. Use the lab's git tag or CHANGELOG heading as the version. Neither is a stable,
   machine-readable interface, and R1 has no tag.

## Recommendation

Option 2. Until then the lab tests keep reading the files they read today, under the new
names (`ICEBLINK_LAB_DIR` and the rest), and stay ignored by default.

## Consequences

- The rename lands without the contract check, and the lab run stays red until option 2 is
  done.
- The check and the layout move are one change, because both are the lab interface.
