#!/usr/bin/env bash
# Saves today's GitHub traffic (views, clones, popular referrers, popular paths) as
# data/YYYY-MM-DD.json on the `traffic` branch. GitHub keeps only 14 days of traffic.
#
# Runs in .github/workflows/traffic.yml. Needs:
#   GITHUB_REPOSITORY  owner/name (set by GitHub Actions)
#   GH_TOKEN           fine-grained token with "Administration: read", read by `gh api`
#   PUSH_TOKEN         token allowed to push to the traffic branch
set -euo pipefail

repository="${GITHUB_REPOSITORY:?set GITHUB_REPOSITORY to owner/name}"
: "${GH_TOKEN:?set GH_TOKEN to a token with Administration: read}"
: "${PUSH_TOKEN:?set PUSH_TOKEN to a token that can push to the traffic branch}"

branch="traffic"
day="$(date -u +%Y-%m-%d)"
tree="$(mktemp -d)"
trap 'rm -rf "$tree"' EXIT

fetch() {
    gh api -H "X-GitHub-Api-Version: 2022-11-28" "repos/${repository}/traffic/$1"
}

views="$(fetch 'views?per=day')"
clones="$(fetch 'clones?per=day')"
referrers="$(fetch 'popular/referrers')"
paths="$(fetch 'popular/paths')"

# Pass the push credential through the environment so it never shows up in argv or .git/config.
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0="http.https://github.com/.extraheader"
GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $(printf 'x-access-token:%s' "$PUSH_TOKEN" | base64 | tr -d '\n')"
export GIT_CONFIG_VALUE_0

git init --quiet "$tree"
cd "$tree"
git remote add origin "https://github.com/${repository}.git"
if git ls-remote --exit-code --heads origin "$branch" >/dev/null; then
    git fetch --quiet --no-tags --depth=1 origin "$branch"
    git checkout --quiet -b "$branch" FETCH_HEAD
else
    git checkout --quiet --orphan "$branch"
fi

mkdir -p data
jq --null-input \
    --arg date "$day" \
    --arg repository "$repository" \
    --argjson views "$views" \
    --argjson clones "$clones" \
    --argjson referrers "$referrers" \
    --argjson paths "$paths" \
    '{date: $date, repository: $repository, views: $views, clones: $clones,
      popular_referrers: $referrers, popular_paths: $paths}' \
    >"data/${day}.json"

git add "data/${day}.json"
if git diff --cached --quiet; then
    printf 'traffic: data/%s.json unchanged, nothing to push\n' "$day"
    exit 0
fi

export GIT_AUTHOR_NAME="github-actions[bot]"
export GIT_AUTHOR_EMAIL="41898282+github-actions[bot]@users.noreply.github.com"
export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME"
export GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
git commit --quiet --signoff --message "chore(traffic): snapshot for ${day}"
git push --quiet origin "HEAD:refs/heads/${branch}"
printf 'traffic: pushed data/%s.json to %s\n' "$day" "$branch"
