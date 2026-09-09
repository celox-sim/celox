#!/usr/bin/env bash
# Temporary experiment: measure execution of validated, cross-generated images.
set -euo pipefail
source scripts/run-heliodor-bench.sh
trap cleanup_heliodor_fallback EXIT
prepare
collect_test_source_files test_soc_linux_boot sources
args=()
for source in "${sources[@]}"; do args+=(--source-file "$source"); done
image_dir="$GITHUB_WORKSPACE/scripts/arm64-images"
veryl_runner="$GITHUB_WORKSPACE/target/release/veryl-heliodor"
chmod +x "$CELOX_RUNNER_BIN" "$veryl_runner"
python3 - "$image_dir" <<'PY'
import gzip, hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
for row in json.loads((root / 'manifest.json').read_text())['images']:
    packed = (root / row['file']).read_bytes()
    assert hashlib.sha256(packed).hexdigest() == row['compressed_sha256']
    data = gzip.decompress(packed)
    assert hashlib.sha256(data).hexdigest() == row['sha256']
    (root / (row['label'] + '.native-image')).write_bytes(data)
PY
cd "$HELIODOR_DIR"
for round in 1 2 3; do
  case "$round" in
    1) order=(baseline candidate veryl) ;;
    2) order=(veryl candidate baseline) ;;
    3) order=(baseline veryl candidate) ;;
  esac
  for label in "${order[@]}"; do
    log="$HELIODOR_RESULTS_DIR/image-$label-$round.log"
    echo "image=$label round=$round"
    if [[ "$label" == veryl ]]; then
      env VERYL_AOT_CACHE_DIR="$GITHUB_WORKSPACE/target/image-veryl-cache" \
        timeout 600 "$veryl_runner" --project "$HELIODOR_DIR" --test test_soc_linux_boot \
        "${args[@]}" > "$log" 2>&1
    else
      timeout 300 "$CELOX_RUNNER_BIN" --project "$HELIODOR_DIR" --test test_soc_linux_boot \
        --backend native --opt-level o2 --native-image-input "$image_dir/$label.native-image" \
        > "$log" 2>&1
    fi
    tail -n 4 "$log"
  done
done
python3 - "$HELIODOR_RESULTS_DIR" <<'PY'
import json, re, statistics, sys
from pathlib import Path
root = Path(sys.argv[1])
summary = {}
for label in ['baseline', 'candidate', 'veryl']:
    times = []
    for round in range(1, 4):
        path = root / f'image-{label}-{round}.log'
        text = path.read_text()
        match = re.search(r'cy=([0-9a-f]+) x3=([0-9a-f]+) pass=(\d+)', text)
        assert match and tuple(int(v, 16) for v in match.groups()) == (0x87cda0, 0xaa, 1), path
        marker = 'VERYL' if label == 'veryl' else 'CELOX'
        assert f'{marker}_TEST_RESULT test=test_soc_linux_boot status=pass' in text, path
        timing = next(line for line in text.splitlines() if line.startswith(marker + '_TEST_TIMING '))
        times.append(int(re.search(r'\bexecute_ns=(\d+)', timing)[1]) / 1e9)
    summary[label] = {'execute_seconds': times, 'median_seconds': statistics.median(times)}
output = json.dumps({'all_nine_boots_match': True, 'summary': summary}, indent=2)
(root / 'image-summary.log').write_text(output + '\n')
print(output)
PY
