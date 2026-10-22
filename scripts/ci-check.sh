#!/bin/sh
# Runs CI (macOS, Linux, Windows) on the current commit without releasing: pushes HEAD to the
# ci/check branch, waits for its run and prints the failed jobs' logs. Run before pushing master.
set -eu
sha=$(git rev-parse HEAD)
git push -q -f origin HEAD:refs/heads/ci/check
id=
for _ in $(seq 60); do
  id=$(gh run list --branch ci/check --json databaseId,headSha --jq ".[] | select(.headSha==\"$sha\") | .databaseId" | head -1)
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
