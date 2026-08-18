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

# A stale fallback points directly into master, while a completed synchronization
# is reachable from develop. Anything else contains work that has not landed in
# either branch, including a human conflict resolution, and must be preserved.
if git merge-base --is-ancestor "$sync_sha" "$master_sha" \
  || git merge-base --is-ancestor "$sync_sha" "$develop_sha"; then
  exit 0
fi

cat >&2 <<EOF
Refusing to replace synchronization branch head $sync_sha.
It contains commits that are not reachable from master ($master_sha) or develop
($develop_sha). Preserve that work and update the synchronization branch from
its current head instead of force-pushing a generated fallback.
EOF
exit 1
