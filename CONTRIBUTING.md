# Contributing to Floe

Thanks for helping. Read [AGENTS.md](AGENTS.md) first: it holds the engineering rules every
change follows, from the read-only contract to error handling and tests.

## Sign off your commits (DCO)

Floe uses the [Developer Certificate of Origin](https://developercertificate.org/). Signing off
says you wrote the change, or have the right to submit it, under the project's Apache-2.0
licence. Add the sign-off with `-s`:

```bash
git commit -s -m "feat(pg): answer the startup message"
```

This adds a `Signed-off-by: Your Name <you@example.com>` line that must match the commit author.
Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/). Pull
requests with unsigned commits can't be merged: CI's `dco` job runs `scripts/dco.sh` on every
pull request. To sign off commits you already made, run `git rebase --signoff main` and push
again.

## Run the gate

Every change must pass the gate before review, and the pull request pastes its output.

1. Install [rustup](https://rustup.rs/). The pinned toolchain in `rust-toolchain.toml`, with
   rustfmt and clippy, installs on first use.
2. Install the two cargo tools, at the versions CI uses:

   ```bash
   cargo install --locked cargo-nextest@0.9.152
   ```

   ```bash
   cargo install --locked cargo-deny@0.21.0
   ```

3. Run the gate from the repository root:

   ```bash
   scripts/gate.sh
   ```

It runs, in order: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
`cargo nextest run --workspace` and `cargo deny check`, and stops at the first failure.

## End-to-end suites

The SQL logic and TPC-H suites run with the gate. A few things need care:

- **psql suite.** It runs when `psql` is on your PATH, or when `FLOE_PSQL_BIN` names one, and
  compares with `tests/golden/psql/<psql major>/`. Goldens exist for psql 16
  (`brew install postgresql@16`, or Debian's `postgresql-client-16`).
- **Updating golden files.** After a deliberate change, rewrite them with
  `UPDATE_GOLDEN=1 cargo nextest run -p floe-suites --test psql`. Read the diff before
  committing; reviewers will too.
- **TPC-H failures.** A failing query is never deleted. Fix it, or add it to
  `tests/expected/KNOWN_FAILURES.md` with an issue.
- **Lab and AWS runs.** These are `#[ignore]` tests, described in
  [docs/floe-lab.md](docs/floe-lab.md).

## Pull requests

Keep them small: under 400 changed lines excluding tests and fixtures, where possible. Fill in
every heading of the pull request template, including what is not done. If reality differs from
the plan, write an ADR in `docs/adr/` from `docs/adr/0000-template.md`.

Report security problems privately, as [SECURITY.md](SECURITY.md) describes, not in an issue.
