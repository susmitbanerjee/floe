#!/usr/bin/env bash
# The gate every change must pass before review (AGENTS.md, "Working loop").
# Needs: the toolchain from rust-toolchain.toml, cargo-nextest and cargo-deny.
set -euo pipefail

cd "$(dirname "$0")/.."

step() {
    printf '\n==> %s\n' "$*"
    "$@"
}

step cargo fmt --check
step cargo clippy --all-targets --all-features -- -D warnings
step cargo nextest run --workspace
step cargo deny check

printf '\n==> gate passed\n'
