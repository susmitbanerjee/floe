# 0003. Parse durations without humantime

- Status: Accepted
- Date: 2026-11-19

## Context

The config prompt asks for `statement_timeout` (and later durations) to be "parsed with
humantime". On 2026-09-14 RustSec marked humantime unmaintained (RUSTSEC-2026-0542, all
versions, no fix). It is in Iceblink's tree today only through object_store, under an ignore that
expires on 2027-02-17 (ADR 0002). A direct dependency would tie Iceblink's own config to it.

Iceblink needs very little: values like `"60s"`, `"90m"` and `"1h"`, checked against a range.

## Options

1. **humantime 2.4.** Already in Cargo.lock and accepts compound forms like `"1h 30m"`.
   But Iceblink's own code would depend on an unmaintained crate, and the ignore would outlive
   object_store's use of it.
2. **jiff.** The replacement RustSec recommends, actively maintained, with a friendly duration
   format. A large dependency for one setting, and jiff 1.0 would sit next to jiff 0.2, which
   is already in the tree.
3. **A small std-only parser.** One whole number and a unit (`ms`, `s`, `m`, `h`), about 15 lines
   with a table test. No dependency.

## Decision

Option 3, chosen by the maintainer on 2026-11-19. It lives in `crates/iceblink/src/config/validate.rs`
as `parse_duration`. Overflow and anything that isn't exactly number-then-unit return `None`, and
the config error says to write, for example, `"90m"` rather than `"1h 30m"`.

## Consequences

- No compound durations. Every phase 1 duration fits in one unit.
- If Iceblink later needs richer durations, prefer jiff over humantime and supersede this ADR.
- The RUSTSEC-2026-0542 ignore in deny.toml stays only for object_store's use.
