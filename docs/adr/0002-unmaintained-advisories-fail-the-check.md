# 0002. Unmaintained advisories fail the check

- Status: Accepted
- Date: 2026-11-19

## Context

The workspace prompt asks cargo-deny to deny vulnerabilities and warn on unmaintained crates.
cargo-deny 0.21.0, the current release, can't express "warn" for unmaintained advisories:

- Vulnerability advisories always fail. The old `vulnerability` level was removed in config
  version 2, so "deny vulnerabilities" holds with no setting.
- `advisories.unmaintained` no longer takes a lint level. It picks which crates an unmaintained
  advisory fails for: `all` (default), `workspace` (direct dependencies only), `transitive`
  (indirect dependencies only) or `none`.
- A single advisory can be accepted with an `ignore` entry, with a `reason` and an `expiry`
  (new in 0.21.0) after which it fails again.

## Options

1. **`unmaintained = "all"` and expiring ignores.** Every unmaintained crate fails the gate
   until someone replaces it or records why it is acceptable and until when. Strictest. A new
   advisory on a deep dependency can turn CI red on an unrelated pull request.
2. **`unmaintained = "workspace"`.** Fails only for crates Iceblink depends on directly. Transitive
   unmaintained crates are not reported at all, not even as warnings.
3. **`unmaintained = "none"` plus a separate advisory report.** Never fails, but nobody reads
   reports that never fail, so it gives up the signal.
4. **Pin an older cargo-deny that still has warn levels.** Gives up fixes and the advisory
   format updates, and fights the tool.

## Decision

Option 1, accepted by the maintainer on 2026-11-19. It keeps every unmaintained crate visible,
and an ignore with an expiry records the "we know, we accept it until this date" that a warning
was meant to give.

Write each expiry as an absolute date. A relative one such as `P90D` counts from the advisory's
publish date, so an old advisory's ignore would already have expired.

## Consequences

- An unmaintained advisory anywhere in the graph blocks merges until it is handled.
- Each accepted advisory leaves a reason and an expiry in deny.toml, reviewed in the PR.
- Revisit if transitive advisories block unrelated work more than once a quarter: option 2 is
  the fallback.
