#!/usr/bin/env bash

set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <sync-sha> <master-sha> <develop-sha>" >&2
  exit 2
fi

sync_sha="$1"
master_sha="$2"
develop_sha="$3"

for sha in "$sync_sha" "$master_sha" "$develop_sha"; do
  if ! git cat-file -e "$sha^{commit}" 2>/dev/null; then
    echo "sync branch guard: $sha is not an available commit" >&2
    exit 2
  fi
done

is_pending_automation_merge() {
  local _commit first_parent second_parent extra_parent
  local author_email committer_email subject automation_marker

  read -r _commit first_parent second_parent extra_parent \
    < <(git rev-list --parents -n 1 "$sync_sha")
  if [ -z "${first_parent:-}" ] \
    || [ -z "${second_parent:-}" ] \
    || [ -n "${extra_parent:-}" ]; then
    return 1
  fi

  author_email="$(git show -s --format=%ae "$sync_sha")"
  committer_email="$(git show -s --format=%ce "$sync_sha")"
  subject="$(git show -s --format=%s "$sync_sha")"
  automation_marker="$(
    git show -s \
      --format='%(trailers:key=Celox-Sync-Automation,valueonly)' \
      "$sync_sha"
  )"
  if [ -n "$automation_marker" ] && [ "$automation_marker" != "true" ]; then
    return 1
  fi

  [ "$author_email" = "celox-release-bot@users.noreply.github.com" ] \
    && [ "$committer_email" = "celox-release-bot@users.noreply.github.com" ] \
    && [ "$subject" = "chore(develop): sync master" ] \
    && git merge-base --is-ancestor "$first_parent" "$develop_sha" \
    && git merge-base --is-ancestor "$second_parent" "$master_sha"
}

# A stale fallback points directly into master, while a completed synchronization
# is reachable from develop. A merge produced by the workflow is also disposable
# while its PR is pending, provided its identity and both parents match the
# expected branch histories. Accept an absent automation marker for pending
# merges created by the workflow version immediately before marker rollout.
# Anything else contains work that has not landed in either branch, including a
# human conflict resolution, and must be preserved.
if git merge-base --is-ancestor "$sync_sha" "$master_sha" \
  || git merge-base --is-ancestor "$sync_sha" "$develop_sha" \
  || is_pending_automation_merge; then
  exit 0
fi

cat >&2 <<EOF
Refusing to replace synchronization branch head $sync_sha.
It contains commits that are not reachable from master ($master_sha) or develop
($develop_sha). Preserve that work and update the synchronization branch from
its current head instead of force-pushing a generated fallback.
EOF
exit 1
