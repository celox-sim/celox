#!/bin/bash
set -euo pipefail

errors=0

while IFS= read -r line; do
  [ -z "$line" ] && continue
  status="${line:0:1}"
  detail="${line:1}"

  case "$status" in
    " ")
      ;; # OK — initialized and in sync
    -)
      echo "ERROR: Submodule not initialized:$detail" >&2
      echo "  Run: git submodule update --init" >&2
      errors=1
      ;;
    +)
      echo "ERROR: Submodule out of sync with index:$detail" >&2
      echo "  Run: git submodule update  (or 'git add' if the change is intentional)" >&2
      errors=1
      ;;
    U)
      echo "ERROR: Submodule has merge conflicts:$detail" >&2
      errors=1
      ;;
  esac
done < <(git submodule status)

[ "$errors" -ne 0 ] && exit 1

# Verify referenced commits exist on submodule remotes
git submodule foreach --quiet '
  if ! git branch -r --contains HEAD 2>/dev/null | grep -q .; then
    echo "ERROR: Submodule $name — commit $(git rev-parse --short HEAD) not found on any remote branch." >&2
    echo "  Push the submodule first, or check that the commit exists upstream." >&2
    exit 1
  fi
'

echo "Submodules OK: initialized, in sync, and referenceable."
