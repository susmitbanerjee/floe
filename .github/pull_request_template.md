## Plan

<!-- What this PR sets out to do and how, in a few lines. Link the issue or prompt. -->

## What changed

<!-- The changes a reviewer should look at first. -->

## Tests

<!-- The tests written first for each behaviour change, and what they prove. -->

## Gate output

<!-- Paste the output of scripts/gate.sh. -->

```text
```

## Not done

<!-- Anything left out, deferred, or that differs from the plan. Link ADRs for deviations. -->

## New dependencies (why)

<!-- For each new dependency: why, size, licence, maintenance. Or "None". -->

## Review checklist

- [ ] Read-only: no code path writes, deletes or commits user tables or objects.
- [ ] No copies of source data; caches and indexes are derived and disposable.
- [ ] Queries can say what they read (snapshot, files, bytes).
- [ ] Smallest thing that passes; no speculative features, flags, traits or macros.
- [ ] Crate dependencies point one way (floe -> floe-pg -> floe-scan -> floe-catalog); `pub(crate)` by default.
- [ ] Input is parsed into checked types at the boundary.
- [ ] Errors use the crate's Error enum, carry context and no secrets; no unwrap, expect or panic outside tests.
- [ ] Secrets are `SecretString` and never logged; outbound calls use TLS with connect and total timeouts.
- [ ] SQL allow-list and warehouse-location checks hold.
- [ ] No blocking in async code; results stream; spawned tasks have an owner and stop on shutdown.
- [ ] Logs: fingerprints at info, full SQL only at debug, never row data.
- [ ] Tests were written first, are hermetic, and are named for behaviour.
- [ ] Public items have `///` docs; comments explain why; no commented-out code.
- [ ] New dependencies are justified above, live in `[workspace.dependencies]` with `default-features = false`, and pass `cargo deny check`.
- [ ] External APIs were checked against the docs for the exact version in Cargo.lock.
- [ ] Commits are conventional and signed off (`git commit -s`).
