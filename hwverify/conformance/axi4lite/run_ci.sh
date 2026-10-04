#!/usr/bin/env bash
# Includes the existing mandatory source replay gate; no optional simulator skip.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:?provide a fresh evidence directory}
test ! -e "$OUT"
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
finish() {
  status=$?
  python3 - "$OUT" "$status" <<'PY'
import json,pathlib,sys
pathlib.Path(sys.argv[1], 'run-status.json').write_text(json.dumps({'status':'passed' if sys.argv[2]=='0' else 'failed','exit_code':int(sys.argv[2])},indent=2)+'\n')
PY
  trap - EXIT
  exit "$status"
}
trap finish EXIT
cd "$REPO"
./conformance/celox-replay/run_ci.sh "$OUT/base-replay"
python3 -m unittest discover -s conformance/axi4lite -p 'test_*.py' -v 2>&1 | tee "$OUT/axi-unit.log"
python3 conformance/axi4lite/run.py --out "$OUT/axi-projects" 2>&1 | tee "$OUT/axi-projects.log"
