#!/bin/bash
# Exercise orchestration without building the CPU or compiling kernels.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/source/riscv/src" "$tmp/source/riscv/build/rust-kernel"
git -C "$tmp/source" init -q
git -C "$tmp/source" -c user.name=Test -c user.email=test@example.com commit -qm initial --allow-empty
touch "$tmp/source/riscv/build/rust-kernel/kernel-tasks.elf"
cat > "$tmp/exporter" <<'EOF'
#!/bin/bash
set -eu
while (($#)); do
    if [[ "$1" == --emit-native-tb ]]; then mkdir -p "$2/kernel-tasks"; exit; fi
    shift
done
exit 1
EOF
cat > "$tmp/runner" <<'EOF'
#!/bin/bash
set -eu
printf '%s\n' "$*" >> "$CALLS"
[[ "$*" == *'--backend native --two-state'* ]]
[[ "$*" == *'--unlimited --report'* ]]
if [[ "${FAKE_RESULT:-pass}" != missing-timing ]]; then
    echo 'CELOX_TEST_TIMING test=test_rica compile_ns=1 execute_ns=1 jit_execute_ns=1'
fi
case "${FAKE_RESULT:-pass}" in
    pass|missing-timing) echo 'RICA_COMPLETE kernel-tasks cycles=100 commits=50'; echo 'CELOX_TEST_RESULT test=test_rica status=pass elapsed_ns=1' ;;
    missing) echo 'CELOX_TEST_RESULT test=test_rica status=pass elapsed_ns=1' ;;
    fail) echo 'RICA_COMPLETE kernel-tasks cycles=100 commits=50'; echo 'CELOX_TEST_RESULT test=test_rica status=fail elapsed_ns=1' ;;
    crash) exit 1 ;;
esac
EOF
chmod +x "$tmp/exporter" "$tmp/runner"
export RICA_DIR="$tmp/source" RICA_REF=HEAD RICA_RESULTS_DIR="$tmp/results"
export RICA_KERNELS=kernel-tasks RICA_REPEAT=3 RICA_BUILD_CELOX_RUNNER=0
export CELOX_RUNNER_BIN="$tmp/exporter" CELOX_TB_RUNNER_BIN="$tmp/runner" CALLS="$tmp/calls"
bash "$root/scripts/run-rica-bench.sh" run > "$tmp/pass.log"
[[ "$(wc -l < "$CALLS")" == 3 ]]
for result in missing missing-timing fail crash; do
    if FAKE_RESULT="$result" bash "$root/scripts/run-rica-bench.sh" run > "$tmp/$result.log" 2>&1; then
        echo "accepted $result" >&2; exit 1
    fi
done
echo 'rica native TB runner checks passed'
