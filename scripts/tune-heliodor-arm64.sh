#!/usr/bin/env bash
# Temporary register-policy experiment; not intended for the production branch.
set -euo pipefail

source scripts/run-heliodor-bench.sh
trap cleanup_heliodor_fallback EXIT
prepare
chmod +x "$CELOX_RUNNER_BIN"
collect_test_source_files test_soc_linux_boot sources
args=()
for source in "${sources[@]}"; do args+=(--source-file "$source"); done
cd "$HELIODOR_DIR"

# phi-localization : spill-temporary reserve : state-page registers
policies=(1:6:0 0:6:0 0:4:0 0:2:0 0:0:0 0:0:1 0:0:2 0:0:3 0:0:4 0:2:2 0:4:2 1:0:0 1:0:2)
for round in 1 2; do
  for ((index = 0; index < ${#policies[@]}; index++)); do
    selected=$index
    if ((round == 2)); then selected=$((${#policies[@]} - index - 1)); fi
    IFS=: read -r phi reserve pages <<< "${policies[$selected]}"
    label="phi${phi}-reserve${reserve}-pages${pages}"
    log="$HELIODOR_RESULTS_DIR/tune-$label-$round.log"
    echo "policy=$label round=$round"
    if ! env CELOX_ARM64_TUNE_PHI_VALUES="$phi" CELOX_ARM64_TUNE_RESERVE="$reserve" \
      CELOX_ARM64_TUNE_PAGES="$pages" timeout 300 \
      "$CELOX_RUNNER_BIN" --project "$HELIODOR_DIR" --test test_soc_linux_boot \
      --backend native --opt-level o2 "${args[@]}" > "$log" 2>&1; then
      echo "TUNING_RUN_FAILED" >> "$log"
    fi
    tail -n 4 "$log"
  done
done

python3 - "$HELIODOR_RESULTS_DIR" <<'PY'
import json
import re
import statistics
import sys
from pathlib import Path

root = Path(sys.argv[1])
rows = []
failed = []
for path in sorted(root.glob('tune-*.log')):
    text = path.read_text()
    label, round_number = path.stem.removeprefix('tune-').rsplit('-', 1)
    valid = (
        'TUNING_RUN_FAILED' not in text
        and 'cy=87cda0 x3=aa pass=1' in text
        and 'CELOX_TEST_RESULT test=test_soc_linux_boot status=pass' in text
    )
    timing = next((line for line in text.splitlines() if line.startswith('CELOX_TEST_TIMING ')), '')
    metrics = {key: int(value) / 1e9 for key, value in re.findall(r'\b(\w+_ns)=(\d+)', timing)}
    if not valid or not all(key in metrics for key in ('compile_ns', 'execute_ns')):
        failed.append(path.name)
        continue
    rows.append({'policy': label, 'round': int(round_number), **metrics})
summary = []
for label in sorted({row['policy'] for row in rows}):
    trials = [row for row in rows if row['policy'] == label]
    if len(trials) != 2:
        failed.append(label)
    summary.append({
        'policy': label,
        'execute_seconds': [row['execute_ns'] for row in trials],
        'median_execute_seconds': statistics.median(row['execute_ns'] for row in trials),
        'median_compile_seconds': statistics.median(row['compile_ns'] for row in trials),
    })
summary.sort(key=lambda row: row['median_execute_seconds'])
report = {'all_boots_match': not failed and len(rows) == 26, 'failures': failed, 'summary': summary, 'rows': rows}
output = json.dumps(report, indent=2)
(root / 'tuning-summary.log').write_text(output + '\n')
print(output)
if not report['all_boots_match']:
    raise SystemExit('Not all tuning configurations completed valid Linux boots')
PY
