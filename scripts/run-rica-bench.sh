#!/bin/bash
# Benchmark Celox on the Rica RISC-V core's board-memory model.
#
# Rica (https://github.com/fabrica-eda/rica) is a 64-bit in-order RISC-V CPU
# written in Veryl. Its `HardwareCoreTestTop` wraps the core, the instruction
# cache and the dual-clock board memory; the CPU clock runs at twice the memory
# clock, so the host testbench drives two clock events per cycle pair instead
# of one wide clock domain. The workload is Rica's self-checking Rust test
# kernels, which build from the pinned checkout with the pinned Rust toolchain.
# Every kernel runs on one compiled model of the working profile (RV64IMAC,
# Zba/Zbb/Zbs, M/S/U with Sv39, console) at the board's CPU/memory clocks.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CELOX_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

RICA_REPO="${RICA_REPO:-https://github.com/fabrica-eda/rica.git}"
readonly PINNED_RICA_REF="$(cat "$SCRIPT_DIR/rica-revision")"
RICA_REF="${RICA_REF:-$PINNED_RICA_REF}"
RICA_DIR="${RICA_DIR:-$CELOX_ROOT/target/rica/source}"
RICA_DIR="$(realpath -m "$RICA_DIR")"
RICA_RESULTS_DIR="${RICA_RESULTS_DIR:-$CELOX_ROOT/target/rica/results}"
RICA_KERNELS="${RICA_KERNELS:-kernel-tasks kernel-syscalls kernel-atomics kernel-user kernel-supervisor}"
RICA_REPEAT="${RICA_REPEAT:-3}"
RICA_MODEL="${RICA_MODEL:-coincident}"
RICA_HARNESS="${RICA_HARNESS:-resolved}"
RICA_MAX_CYCLES="${RICA_MAX_CYCLES:-2000000}"
CELOX_OPT_LEVEL="${CELOX_OPT_LEVEL:-O2}"
CELOX_SIR_PASS_OVERRIDES="${CELOX_SIR_PASS_OVERRIDES:-}"
RICA_CELOX_CARGO_PROFILE="${RICA_CELOX_CARGO_PROFILE:-release}"
RICA_BUILD_CELOX_RUNNER="${RICA_BUILD_CELOX_RUNNER:-1}"
RICA_ALLOW_UNPINNED="${RICA_ALLOW_UNPINNED:-0}"
# Cargo builds the built-in `dev` and `test` profiles into `target/debug`
# and `bench` into `target/release`.
case "$RICA_CELOX_CARGO_PROFILE" in
    dev|test) RICA_CELOX_PROFILE_DIR=debug ;;
    bench) RICA_CELOX_PROFILE_DIR=release ;;
    *) RICA_CELOX_PROFILE_DIR="$RICA_CELOX_CARGO_PROFILE" ;;
esac
CELOX_RUNNER_BIN="${CELOX_RUNNER_BIN:-$CELOX_ROOT/target/$RICA_CELOX_PROFILE_DIR/celox-rica}"

usage() {
    cat <<'USAGE'
usage: scripts/run-rica-bench.sh [prepare|run]

  prepare   clone the pinned Rica revision and build its Rust test kernels
  run       build the Celox runner and execute the kernels (default)

Environment:
  RICA_DIR              checkout directory (default: target/rica/source)
  RICA_REF              commit to check out (default: scripts/rica-revision)
  RICA_RESULTS_DIR      results directory (default: target/rica/results)
  RICA_KERNELS          space-separated kernel names (default: the M, U and S kernels)
  RICA_REPEAT           executions per kernel on one compiled model (default: 3)
  RICA_MODEL            coincident (tick_events), board (serialized ticks) or timed
  RICA_HARNESS          resolved (typed handles) or rica (name lookup per read)
  RICA_MAX_CYCLES       cycle budget per kernel (default: 2000000)
  CELOX_OPT_LEVEL       O0, O1 or O2 (default: O2)
  CELOX_SIR_PASS_OVERRIDES
                        space-separated +sir:<pass> / -sir:<pass> overrides
  RICA_CELOX_CARGO_PROFILE
                        cargo profile for the runner (default: release)
  CELOX_RUNNER_BIN      prebuilt runner; set RICA_BUILD_CELOX_RUNNER=0 to skip cargo
  RICA_ALLOW_UNPINNED   1 to run a checkout whose HEAD is not RICA_REF
USAGE
}

prepare() {
    if [[ -d "$RICA_DIR/.git" ]]; then
        git -C "$RICA_DIR" fetch --quiet origin "$RICA_REF" || true
    else
        git clone --quiet "$RICA_REPO" "$RICA_DIR"
    fi
    git -C "$RICA_DIR" checkout --quiet "$RICA_REF"
    (cd "$RICA_DIR" && python3 riscv/kernel/build.py >/dev/null && python3 riscv/kernel/build-user.py >/dev/null)
    echo "prepared Rica $(git -C "$RICA_DIR" rev-parse HEAD) in $RICA_DIR"
}

kernel_elf() {
    local name="$1"
    case "$name" in
        kernel-user|kernel-supervisor) echo "$RICA_DIR/riscv/build/user-kernel/$name.elf" ;;
        *) echo "$RICA_DIR/riscv/build/rust-kernel/$name.elf" ;;
    esac
}

run() {
    if [[ ! -d "$RICA_DIR/riscv/src" ]]; then
        echo "error: $RICA_DIR is not a Rica checkout; run 'prepare' first" >&2
        exit 1
    fi
    local head expected
    head="$(git -C "$RICA_DIR" rev-parse HEAD)"
    expected="$(git -C "$RICA_DIR" rev-parse --verify --quiet "$RICA_REF^{commit}" || true)"
    if [[ "$head" != "$expected" && "$RICA_ALLOW_UNPINNED" != 1 ]]; then
        echo "error: $RICA_DIR is at $head, not $RICA_REF; run 'prepare' or set RICA_ALLOW_UNPINNED=1" >&2
        exit 1
    fi
    if [[ "$RICA_BUILD_CELOX_RUNNER" == 1 ]]; then
        (cd "$CELOX_ROOT" && cargo build --quiet --profile "$RICA_CELOX_CARGO_PROFILE" -p celox-bench --bin celox-rica)
    fi
    mkdir -p "$RICA_RESULTS_DIR"
    local -a elfs=()
    local name
    for name in $RICA_KERNELS; do
        local elf
        elf="$(kernel_elf "$name")"
        if [[ ! -f "$elf" ]]; then
            echo "error: $elf is missing; run 'prepare' first" >&2
            exit 1
        fi
        elfs+=(--elf "$elf")
    done
    local -a passes=()
    local override
    for override in $CELOX_SIR_PASS_OVERRIDES; do
        passes+=(--sir-pass "$override")
    done
    local log="$RICA_RESULTS_DIR/celox-$RICA_MODEL-$RICA_HARNESS.log"
    "$CELOX_RUNNER_BIN" \
        --project "$RICA_DIR/riscv" \
        --opt-level "$CELOX_OPT_LEVEL" \
        --model "$RICA_MODEL" \
        --harness "$RICA_HARNESS" \
        --max-cycles "$RICA_MAX_CYCLES" \
        --repeat "$RICA_REPEAT" \
        --a-extension --sv39 --machine-console \
        "${passes[@]}" \
        "${elfs[@]}" | tee "$log"
    echo "results written to $log"
}

case "${1:-run}" in
    prepare) prepare ;;
    run) run ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
esac
