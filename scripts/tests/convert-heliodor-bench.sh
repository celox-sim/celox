#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat >"$TMP/results.tsv" <<'EOF'
runner	test	status	elapsed_ns	log	semantic_status	exit_status	process_elapsed_ns	reported_elapsed_ns	compile_elapsed_ns	execute_elapsed_ns	jit_execute_elapsed_ns
veryl-cc-sync	test_soc_linux_boot	0	900	veryl.log	pass	0	900	800	3000000	4000000	NA
celox	test_soc_linux_boot	0	700	celox.log	pass	0	700	600	1000000	2500000	2000000
celox-tiered	test_soc_linux_boot	0	650	tiered.log	pass	0	650	5500000	500000	5000000	NA
veryl-cc-tiered	test_soc_linux_boot	0	10000000	veryl-tiered.log	pass	0	10000000	9500000	1500000	8000000	NA
EOF

node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/results.tsv" "$TMP/results.json" >/dev/null
node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/results.tsv" "$TMP/jit.json" --jit-only >/dev/null

grep -vE '^(celox-tiered|veryl-cc-tiered)' "$TMP/results.tsv" >"$TMP/default-results.tsv"
node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/default-results.tsv" "$TMP/default-results.json" >/dev/null

cat >"$TMP/arm64-results.tsv" <<'EOF'
runner	test	status	elapsed_ns	log	semantic_status	exit_status	process_elapsed_ns	reported_elapsed_ns	compile_elapsed_ns	execute_elapsed_ns	jit_execute_elapsed_ns
celox	test_soc_linux_boot	0	1150	arm64.log	pass	0	1150	1100	7000000	8000000	7500000
veryl-cc-sync	test_soc_linux_boot	0	1550	veryl-arm64.log	pass	0	1550	1500	11000000	12000000	NA
celox-tiered	test_soc_linux_boot	0	17000000	celox-tiered-arm64.log	pass	0	17000000	16500000	3500000	13000000	NA
veryl-cc-tiered	test_soc_linux_boot	0	21000000	veryl-tiered-arm64.log	pass	0	21000000	20500000	4500000	16000000	NA
EOF

node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/results.tsv" "$TMP/platform-results.json" \
    --arm64-results "$TMP/arm64-results.tsv" --require-tiered >/dev/null

node -e '
const fs = require("fs");
const values = Object.fromEntries(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).map((x) => [x.name, x.value]));
if (values["heliodor-celox-jit/heliodor_linux_boot_execution"] !== 2) process.exit(1);
if (values["heliodor-celox-total/heliodor_linux_boot_execution"] !== 2.5) process.exit(1);
if (values["heliodor-veryl/heliodor_linux_boot_execution"] !== 4) process.exit(1);
if (values["heliodor-celox-tiered/heliodor_linux_boot_end_to_end"] !== 5.5) process.exit(1);
if (values["heliodor-celox-tiered/heliodor_linux_boot_startup"] !== 0.5) process.exit(1);
if (values["heliodor-celox-tiered/heliodor_linux_boot_execution"] !== 5) process.exit(1);
' "$TMP/results.json"

node -e '
const fs = require("fs");
const values = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (values.length !== 1 || values[0].name !== "heliodor-celox-jit/heliodor_linux_boot_execution") process.exit(1);
' "$TMP/jit.json"

node -e '
const fs = require("fs");
const values = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (values.length !== 5) process.exit(1);
if (values.some((entry) => entry.name.includes("celox-tiered"))) process.exit(1);
' "$TMP/default-results.json"

node -e '
const fs = require("fs");
const values = Object.fromEntries(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).map((x) => [x.name, x.value]));
if (Object.keys(values).length !== 25) process.exit(1);
const expected = {
  "heliodor-veryl-tiered-x86_64/heliodor_linux_boot_startup": 1.5,
  "heliodor-veryl-tiered-x86_64/heliodor_linux_boot_execution": 8,
  "heliodor-veryl-tiered-x86_64/heliodor_linux_boot_end_to_end": 9.5,
  "heliodor-celox-tiered-aarch64/heliodor_linux_boot_startup": 3.5,
  "heliodor-celox-tiered-aarch64/heliodor_linux_boot_execution": 13,
  "heliodor-celox-tiered-aarch64/heliodor_linux_boot_end_to_end": 16.5,
  "heliodor-veryl-tiered-aarch64/heliodor_linux_boot_startup": 4.5,
  "heliodor-veryl-tiered-aarch64/heliodor_linux_boot_execution": 16,
  "heliodor-veryl-tiered-aarch64/heliodor_linux_boot_end_to_end": 20.5,
  "heliodor-native-x86_64/heliodor_linux_boot_compilation": 1,
  "heliodor-native-x86_64/heliodor_linux_boot_execution": 2.5,
  "heliodor-veryl-cc-x86_64/heliodor_linux_boot_compilation": 3,
  "heliodor-veryl-cc-x86_64/heliodor_linux_boot_execution": 4,
  "heliodor-native-aarch64/heliodor_linux_boot_compilation": 7,
  "heliodor-native-aarch64/heliodor_linux_boot_execution": 8,
  "heliodor-veryl-cc-aarch64/heliodor_linux_boot_compilation": 11,
  "heliodor-veryl-cc-aarch64/heliodor_linux_boot_execution": 12,
};
for (const [name, value] of Object.entries(expected)) {
  if (values[name] !== value) process.exit(1);
}
' "$TMP/platform-results.json"

# Historical synchronous-only results remain readable, while publishing requires
# both tiered runners. Reject missing, failed, duplicate, and mismatched rows.
if node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/default-results.tsv" "$TMP/rejected.json" --require-tiered >/dev/null 2>&1; then
    echo "missing required tiered rows were accepted" >&2
    exit 1
fi
for source in results arm64-results; do
    for failure in missing failed duplicate wrong-test zero-time overlapping-time; do
        cp "$TMP/$source.tsv" "$TMP/invalid.tsv"
        case "$failure" in
            missing) sed -i '/^veryl-cc-tiered/d' "$TMP/invalid.tsv" ;;
            failed) sed -i '/^veryl-cc-tiered/s/\tpass\t/\tfail\t/' "$TMP/invalid.tsv" ;;
            duplicate) sed -n '/^veryl-cc-tiered/p' "$TMP/$source.tsv" >>"$TMP/invalid.tsv" ;;
            wrong-test) sed -i '/^veryl-cc-tiered/s/test_soc_linux_boot/other_test/' "$TMP/invalid.tsv" ;;
            zero-time) awk -F '\t' 'BEGIN { OFS="\t" } $1 == "veryl-cc-tiered" { $10=0 } { print }' "$TMP/$source.tsv" >"$TMP/invalid.tsv" ;;
            overlapping-time) awk -F '\t' 'BEGIN { OFS="\t" } $1 == "veryl-cc-tiered" { $9=1 } { print }' "$TMP/$source.tsv" >"$TMP/invalid.tsv" ;;
        esac
        if [[ "$source" == results ]]; then
            args=("$TMP/invalid.tsv" "$TMP/rejected.json" --require-tiered)
        else
            args=("$TMP/results.tsv" "$TMP/rejected.json" --arm64-results "$TMP/invalid.tsv" --require-tiered)
        fi
        if node "$ROOT/scripts/convert-heliodor-bench.mjs" "${args[@]}" >/dev/null 2>&1; then
            echo "$source/$failure was accepted" >&2
            exit 1
        fi
    done
done

echo "convert-heliodor-bench fixture test: PASS"

# Multiple kernels and hart counts must remain separate series on both hosts.
for arch in results arm64-results; do
    cp "$TMP/$arch.tsv" "$TMP/suite-$arch.tsv"
    for test in test_soc_66_linux_boot test_soc_71_smp_linux_boot_4hart test_soc_smp_linux_boot_8hart; do
        tail -n +2 "$TMP/$arch.tsv" | sed "s/test_soc_linux_boot/$test/g" >> "$TMP/suite-$arch.tsv"
    done
done
node "$ROOT/scripts/convert-heliodor-bench.mjs" "$TMP/suite-results.tsv" "$TMP/suite.json" \
    --arm64-results "$TMP/suite-arm64-results.tsv" --require-tiered --suite >/dev/null
node - "$TMP/suite.json" <<'JS'
const assert = require("node:assert/strict");
const rows = require(process.argv[2]);
assert.equal(rows.length, 25 * 4);
assert.equal(new Set(rows.map(row => row.name)).size, rows.length);
assert.equal(rows.find(row => row.name === "heliodor-native-aarch64/heliodor_suite_71_smp_linux_boot_4hart_execution").value, 8);
JS
sed '/test_soc_smp_linux_boot_8hart/d' "$TMP/suite-arm64-results.tsv" > "$TMP/missing.tsv"
if node "$ROOT/scripts/convert-heliodor-bench.mjs" "$TMP/suite-results.tsv" "$TMP/bad.json" \
    --arm64-results "$TMP/missing.tsv" --suite >/dev/null 2>&1; then
    echo "accepted mismatched architecture workloads" >&2
    exit 1
fi
echo "convert-heliodor-bench suite fixtures: PASS"
