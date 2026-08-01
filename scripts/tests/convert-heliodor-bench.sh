#!/bin/bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat >"$TMP/results.tsv" <<'EOF'
runner	test	status	elapsed_ns	log	semantic_status	exit_status	process_elapsed_ns	reported_elapsed_ns	compile_elapsed_ns	execute_elapsed_ns	jit_execute_elapsed_ns
veryl-cc-sync	test_soc_linux_boot	0	900	veryl.log	pass	0	900	800	3000000	4000000	NA
celox	test_soc_linux_boot	0	700	celox.log	pass	0	700	600	1000000	2500000	2000000
EOF

node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/results.tsv" "$TMP/results.json" >/dev/null
node "$ROOT/scripts/convert-heliodor-bench.mjs" \
    "$TMP/results.tsv" "$TMP/jit.json" --jit-only >/dev/null

node -e '
const fs = require("fs");
const values = Object.fromEntries(JSON.parse(fs.readFileSync(process.argv[1], "utf8")).map((x) => [x.name, x.value]));
if (values["heliodor-celox-jit/heliodor_linux_boot_execution"] !== 2) process.exit(1);
if (values["heliodor-celox-total/heliodor_linux_boot_execution"] !== 2.5) process.exit(1);
if (values["heliodor-veryl/heliodor_linux_boot_execution"] !== 4) process.exit(1);
' "$TMP/results.json"

node -e '
const fs = require("fs");
const values = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (values.length !== 1 || values[0].name !== "heliodor-celox-jit/heliodor_linux_boot_execution") process.exit(1);
' "$TMP/jit.json"

echo "convert-heliodor-bench fixture test: PASS"
