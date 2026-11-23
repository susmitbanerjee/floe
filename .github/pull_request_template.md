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

The gate already enforces formatting, the lints (no `unwrap`, `expect`, `panic!`, `todo!`,
`unimplemented!` or `dbg!`; `///` on every public item), the tests and `cargo deny`, and CI's
`dco` job checks every commit's sign-off. This list covers what they can't.

### Contract

- [ ] No code path writes, deletes or commits user tables or objects.
- [ ] No copies of source data; caches and indexes are derived and disposable.
- [ ] Every query can still say what it read: snapshot, files, bytes.
- [ ] The read-only tests are unchanged, or each change to them is explained:
      `a_full_query_never_tries_to_write`, `writes_are_refused_before_the_inner_storage`,
      `writes_and_deletes_never_reach_s3` and `tests/slt/read_only.slt`.

### Simplicity and structure

- [ ] The smallest thing that passes: no speculative features, flags, options or macros. Could a
      plain function replace anything new?
- [ ] Every new trait, generic or layer has two real implementations today, or is a seam the
      design names.
- [ ] Crates depend one way (`floe -> floe-pg -> floe-scan -> floe-catalog`), and new items are
      `pub(crate)` unless they are the crate's API.
- [ ] Names use the design's words: catalog, namespace, table, snapshot, receipt, principal,
      warehouse.

### Errors

- [ ] Errors use the crate's one `Error` enum; variants carry context (table, URI), never secrets.
- [ ] Any `.expect("INVARIANT: …")` sits under `#[expect(clippy::expect_used, reason = …)]`,
      and the reviewer accepts the reason.
- [ ] Messages a person sees say what happened, why, and what to do. Postgres clients get the
      right SQLSTATE.

### Security

- [ ] No secret can reach a log line, an error, `Debug` output, a panic or a client message.
- [ ] New input is parsed into a checked type at the boundary.
- [ ] Every new outbound call uses TLS (plain http only to this machine, ADR 0012), has connect
      and total timeouts, and retries idempotent reads only.
- [ ] The statement allow-list and the location guard are unchanged, or the change is reviewed
      line by line.
- [ ] Logs: query fingerprints at info, full SQL only at debug, never row data.
- [ ] No shelling out, no dynamic code loading, and no file reads except the config file and
      the cache folder.
- [ ] New dependencies are justified above and live in `[workspace.dependencies]` with
      `default-features = false`. A git dependency only with an ADR and a pinned commit SHA.

### Async and performance

- [ ] No blocking call inside async code. Every spawned task has an owner and stops on shutdown.
- [ ] Record batches are streamed, not collected.

### Tests

- [ ] Every behaviour change has a test that fails without it, named under Tests above. A bug
      fix includes the test that would have caught it.
- [ ] Failure paths are covered, not just the happy path, and test names describe behaviour.
- [ ] Tests are hermetic; lab and AWS tests are `#[ignore]` and gated.

### Docs, size and process

- [ ] Comments explain why; no commented-out code; a TODO only with an issue link.
- [ ] The README, a crate README or an ADR is updated if behaviour or a decision changed.
- [ ] External APIs were checked against the docs for the exact version in `Cargo.lock`. Where
      reality differed from the prompt, an ADR says so.
- [ ] Under 400 changed lines, excluding tests and fixtures, where possible; one concern per PR.
- [ ] Commits are conventional; the gate output is pasted above, and CI is green.
