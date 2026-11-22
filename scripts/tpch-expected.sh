#!/usr/bin/env bash
# Downloads DuckDB's TPC-H queries and its scale-factor-0.01 answers into
# tests/expected/tpch-sf0.01/, pinned to one DuckDB commit. These are the files DuckDB's tpch
# extension serves through tpch_queries() and tpch_answers().
#
# Usage: scripts/tpch-expected.sh            (from the repository root)
#
# Moves to floe-lab once it exists. tests/expected/README.md records the tag and commit.
set -euo pipefail

tag="v2.0.0"
commit="d54cbbe3df17e8e5ce68312f483b4155836112aa"
base="https://raw.githubusercontent.com/duckdb/duckdb/${commit}/extension/tpch/dbgen"
out="tests/expected/tpch-sf0.01"

mkdir -p "$out"
for number in $(seq -w 1 22); do
    curl --fail --silent --show-error --max-time 30 \
        "${base}/queries/q${number}.sql" --output "${out}/q${number}.sql"
    curl --fail --silent --show-error --max-time 30 \
        "${base}/answers/sf0.01/q${number}.csv" --output "${out}/q${number}.csv"
done
curl --fail --silent --show-error --max-time 30 \
    "https://raw.githubusercontent.com/duckdb/duckdb/${commit}/LICENSE" \
    --output "${out}/LICENSE-duckdb"
echo "wrote q01-q22 .sql and .csv from duckdb ${tag} (${commit}) to ${out}"
