#!/usr/bin/env bash
# Checks the Developer Certificate of Origin sign-off (CONTRIBUTING.md): every commit in
# BASE..HEAD, merges aside, needs a Signed-off-by line with its author's email.
#
#   scripts/dco.sh BASE HEAD
#
# CI's dco job runs it on each pull request. Exits 1, naming each commit, if any lacks it.
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: scripts/dco.sh BASE HEAD" >&2
    exit 2
fi
base=$1
head=$2

missing=0
lower() { tr '[:upper:]' '[:lower:]'; }

for commit in $(git rev-list --no-merges "$base..$head"); do
    # Emails compare case-insensitively. No grep -iF: Git for Windows' grep 3.0 aborts on it.
    email=$(git log -1 --format='%ae' "$commit" | lower)
    signed=$(git log -1 --format='%(trailers:key=Signed-off-by,valueonly)' "$commit" | lower)
    if [[ $signed == *"<$email>"* ]]; then
        continue
    fi
    echo "::error::$(git log -1 --format='%h %s' "$commit"): no Signed-off-by for its author <$email>"
    missing=$((missing + 1))
done

if [ "$missing" -gt 0 ]; then
    echo "$missing commit(s) not signed off. Fix: git rebase --signoff $base, then push again." >&2
    exit 1
fi
echo "every commit in $base..$head is signed off"
