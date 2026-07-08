#!/bin/bash
# Compare Celox against Veryl's native simulator on Heliodor testbenches.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CELOX_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

HELIODOR_REPO="${HELIODOR_REPO:-https://github.com/dalance/heliodor.git}"
HELIODOR_REF="${HELIODOR_REF:-7ad830fc0f8506c934b61a853ce2eadfa5926b82}"
HELIODOR_DIR="${HELIODOR_DIR:-$CELOX_ROOT/target/heliodor/source}"
HELIODOR_RESULTS_DIR="${HELIODOR_RESULTS_DIR:-$CELOX_ROOT/target/heliodor/results}"
HELIODOR_TESTS="${HELIODOR_TESTS:-test_soc_linux_boot}"
HELIODOR_RUNNERS="${HELIODOR_RUNNERS:-celox veryl-cranelift veryl-cc}"
CELOX_OPT_LEVEL="${CELOX_OPT_LEVEL:-O1}"
VERYL_BIN="${VERYL_BIN:-veryl}"

usage() {
    cat <<'USAGE'
usage: scripts/run-heliodor-bench.sh [prepare|list|run]

Environment:
  HELIODOR_DIR         checkout/cache directory (default: target/heliodor/source)
  HELIODOR_REF         commit/tag/branch to checkout
  HELIODOR_TESTS       space-separated test modules
  HELIODOR_RUNNERS     space-separated runners: celox veryl-cc veryl-cranelift veryl-interpret
  CELOX_OPT_LEVEL      O0, O1, or O2 for the Celox runner
  VERYL_BIN            veryl executable for Veryl runners

Examples:
  scripts/run-heliodor-bench.sh prepare
  HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" scripts/run-heliodor-bench.sh run
  HELIODOR_RUNNERS="celox veryl-cranelift" scripts/run-heliodor-bench.sh run
USAGE
}

prepare() {
    mkdir -p "$(dirname "$HELIODOR_DIR")" "$HELIODOR_RESULTS_DIR"
    if [[ -d "$HELIODOR_DIR/.git" ]]; then
        git -C "$HELIODOR_DIR" fetch --quiet origin
    elif [[ -e "$HELIODOR_DIR" ]]; then
        echo "error: $HELIODOR_DIR exists but is not a git checkout" >&2
        exit 1
    else
        git clone --quiet "$HELIODOR_REPO" "$HELIODOR_DIR"
    fi
    git -C "$HELIODOR_DIR" checkout --quiet "$HELIODOR_REF"
    rm -f "$HELIODOR_DIR/.build/lock"
    echo "Heliodor: $(git -C "$HELIODOR_DIR" rev-parse HEAD) at $HELIODOR_DIR"
}

list_tests() {
    prepare >/dev/null
    rg -n '^\s*#\[test\(' "$HELIODOR_DIR/tb" --glob '*.veryl' \
        | sed -E 's/.*#\[test\(([^)]*)\)\].*/\1/' \
        | sort
}

veryl_ignored_flag() {
    if "$VERYL_BIN" test --help 2>&1 | rg -q -- '--ignored'; then
        printf '%s\n' '--ignored'
    else
        printf '%s\n' '--include-ignored'
    fi
}

run_one() {
    local runner="$1"
    local test="$2"
    local stamp log status start end elapsed
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    log="$HELIODOR_RESULTS_DIR/${stamp}_${runner}_${test}.log"

    echo "== $runner :: $test =="
    start="$(date +%s%N)"
    set +e
    case "$runner" in
        celox)
            (
                cd "$HELIODOR_DIR"
                cargo run --manifest-path "$CELOX_ROOT/Cargo.toml" -p celox \
                    --example run_veryl_project_test --release -- \
                    --project "$HELIODOR_DIR" --test "$test" --opt-level "$CELOX_OPT_LEVEL"
            ) >"$log" 2>&1
            status="$?"
            ;;
        veryl-cc)
            (
                cd "$HELIODOR_DIR"
                "$VERYL_BIN" test "$(veryl_ignored_flag)" --test "$test" --backend cc
            ) >"$log" 2>&1
            status="$?"
            ;;
        veryl-cranelift)
            (
                cd "$HELIODOR_DIR"
                "$VERYL_BIN" test "$(veryl_ignored_flag)" --test "$test" --backend cranelift
            ) >"$log" 2>&1
            status="$?"
            ;;
        veryl-interpret)
            (
                cd "$HELIODOR_DIR"
                "$VERYL_BIN" test "$(veryl_ignored_flag)" --test "$test" --backend interpret
            ) >"$log" 2>&1
            status="$?"
            ;;
        *)
            echo "unknown runner: $runner" >"$log"
            status=2
            ;;
    esac
    set -e
    end="$(date +%s%N)"
    elapsed="$((end - start))"

    printf '%s\t%s\t%s\t%s\t%s\n' "$runner" "$test" "$status" "$elapsed" "$log" \
        | tee -a "$HELIODOR_RESULTS_DIR/results.tsv"
    if [[ "$status" != 0 ]]; then
        tail -n 40 "$log" >&2 || true
        return "$status"
    fi
}

run_all() {
    prepare
    mkdir -p "$HELIODOR_RESULTS_DIR"
    if [[ ! -f "$HELIODOR_RESULTS_DIR/results.tsv" ]]; then
        printf 'runner\ttest\tstatus\telapsed_ns\tlog\n' >"$HELIODOR_RESULTS_DIR/results.tsv"
    fi
    for test in $HELIODOR_TESTS; do
        for runner in $HELIODOR_RUNNERS; do
            run_one "$runner" "$test"
        done
    done
}

cmd="${1:-run}"
case "$cmd" in
    prepare) prepare ;;
    list) list_tests ;;
    run) run_all ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
esac
