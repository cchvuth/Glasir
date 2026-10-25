#!/bin/sh
# Runs CI (macOS, Linux, Windows) on the current commit without releasing: pushes HEAD to the
# ci/check branch, waits for the run that push starts and prints the failed jobs' logs. Run before
# pushing master. A ci/check already at HEAD is deleted first, so the push starts a new run.
set -eu
sha=$(git rev-parse HEAD)
last=$(gh run list --branch ci/check --limit 1 --json databaseId --jq '.[0].databaseId // 0')
[ "$(git ls-remote origin refs/heads/ci/check | cut -f1)" = "$sha" ] && git push -q origin :refs/heads/ci/check
git push -q -f origin HEAD:refs/heads/ci/check
id=
for _ in $(seq 60); do
  id=$(gh run list --branch ci/check --json databaseId,headSha --jq "map(select(.headSha==\"$sha\" and .databaseId > $last)) | .[0].databaseId // empty")
  [ -n "$id" ] && break
  sleep 5
done
[ -n "$id" ] || { echo "no CI run for $sha" >&2; exit 1; }
if gh run watch "$id" --exit-status --interval 30 >/dev/null; then
  echo "CI passed on all platforms: $sha"
else
  gh run view "$id" --log-failed | grep -E "panicked|FAILED|error(\[|:)|assert" | head -40 >&2
  echo "CI failed: https://github.com/cchvuth/Glasir/actions/runs/$id" >&2
  exit 1
fi
