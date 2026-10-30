#!/usr/bin/env bash
set -euo pipefail
version=${1:?version required}
revision=${2:?source commit required}
prefix=${3:?installation prefix required}
if [[ $(cat "$prefix/.celox-verilator-source") != "$revision" ]]; then
  echo 'Cached Verilator source identity does not match the requested commit' >&2
  exit 1
fi
# Test the installed binary and its runtime dependencies on cache hits as well.
actual=$(VERILATOR_ROOT="$prefix/share/verilator" "$prefix/bin/verilator" --version)
case "$actual" in
  "Verilator $version "*) printf '%s\n' "$actual" ;;
  *) echo "Unexpected Verilator version: $actual" >&2; exit 1 ;;
esac
test -f "$prefix/share/verilator/include/verilated_vpi.h"
