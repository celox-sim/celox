#!/bin/bash
# Compare Celox against Veryl's native simulator on Heliodor testbenches.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CELOX_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

HELIODOR_REPO="${HELIODOR_REPO:-https://github.com/dalance/heliodor.git}"
HELIODOR_REF="${HELIODOR_REF:-7ad830fc0f8506c934b61a853ce2eadfa5926b82}"
HELIODOR_DIR="${HELIODOR_DIR:-$CELOX_ROOT/target/heliodor/source}"
HELIODOR_RESULTS_DIR="${HELIODOR_RESULTS_DIR:-$CELOX_ROOT/target/heliodor/results}"
HELIODOR_TOOLS_DIR="${HELIODOR_TOOLS_DIR:-$CELOX_ROOT/target/heliodor/tools}"
HELIODOR_TESTS="${HELIODOR_TESTS:-test_soc_linux_boot}"
HELIODOR_RUNNERS="${HELIODOR_RUNNERS:-veryl-cranelift veryl-cc celox}"
CELOX_OPT_LEVEL="${CELOX_OPT_LEVEL:-O1}"
CELOX_RUNNER_BIN="${CELOX_RUNNER_BIN:-$CELOX_ROOT/target/release/examples/run_veryl_project_test}"
HELIODOR_CELOX_TIMEOUT_MULTIPLIER="${HELIODOR_CELOX_TIMEOUT_MULTIPLIER:-2}"
HELIODOR_INSTALL_TOOLS="${HELIODOR_INSTALL_TOOLS:-1}"
HELIODOR_VERYL_VERSION="${HELIODOR_VERYL_VERSION:-0.20.2}"
VERYL_BIN="${VERYL_BIN:-}"

declare -A BASELINE_ELAPSED_NS=()

usage() {
    cat <<'USAGE'
usage: scripts/run-heliodor-bench.sh [prepare|list|run]

Environment:
  HELIODOR_DIR         checkout/cache directory (default: target/heliodor/source)
  HELIODOR_TOOLS_DIR   benchmark-owned tool install directory
  HELIODOR_REF         commit/tag/branch to checkout
  HELIODOR_TESTS       space-separated test modules
  HELIODOR_RUNNERS     space-separated runners (default: veryl-cranelift veryl-cc celox)
                       Celox runners: celox, celox-cranelift
  HELIODOR_TIMEOUT_SEC absolute timeout for every runner/test
  HELIODOR_CELOX_TIMEOUT_MULTIPLIER
                       timeout Celox after N times the fastest successful Veryl baseline
  CELOX_OPT_LEVEL      O0, O1, or O2 for the Celox runner
  CELOX_RUNNER_BIN     prebuilt Celox runner path
  HELIODOR_INSTALL_TOOLS
                       install missing tools into HELIODOR_TOOLS_DIR (default: 1)
  HELIODOR_VERYL_VERSION
                       cargo-install version for the Veryl CLI
  VERYL_BIN            explicit veryl executable for Veryl runners

Examples:
  scripts/run-heliodor-bench.sh prepare
  HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" scripts/run-heliodor-bench.sh run
  HELIODOR_RUNNERS="veryl-cranelift celox" scripts/run-heliodor-bench.sh run
USAGE
}

prepare() {
    mkdir -p "$(dirname "$HELIODOR_DIR")" "$HELIODOR_RESULTS_DIR" "$HELIODOR_TOOLS_DIR"
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

installed_veryl_bin() {
    printf '%s\n' "$HELIODOR_TOOLS_DIR/veryl-$HELIODOR_VERYL_VERSION/bin/veryl"
}

resolve_veryl_bin() {
    if [[ -n "$VERYL_BIN" ]]; then
        if [[ -x "$VERYL_BIN" ]] || command -v "$VERYL_BIN" >/dev/null; then
            printf '%s\n' "$VERYL_BIN"
            return
        fi
        echo "error: VERYL_BIN is set but not executable: $VERYL_BIN" >&2
        return 127
    fi

    if command -v veryl >/dev/null; then
        command -v veryl
        return
    fi

    local installed
    installed="$(installed_veryl_bin)"
    if [[ -x "$installed" ]]; then
        printf '%s\n' "$installed"
        return
    fi

    if [[ "$HELIODOR_INSTALL_TOOLS" != 1 ]]; then
        echo "error: veryl not found and HELIODOR_INSTALL_TOOLS=0" >&2
        return 127
    fi

    if ! install_veryl; then
        return 1
    fi
    if [[ ! -x "$installed" ]]; then
        echo "error: Veryl install did not produce $installed" >&2
        return 1
    fi
    printf '%s\n' "$installed"
}

install_veryl() {
    local root log bin
    root="$HELIODOR_TOOLS_DIR/veryl-$HELIODOR_VERYL_VERSION"
    log="$HELIODOR_RESULTS_DIR/veryl_install_$HELIODOR_VERYL_VERSION.log"
    bin="$root/bin/veryl"
    if [[ -x "$bin" ]]; then
        return
    fi

    echo "Installing Veryl CLI $HELIODOR_VERYL_VERSION: $bin" >&2
    rm -rf "$root"
    mkdir -p "$root"
    if ! cargo install veryl --version "$HELIODOR_VERYL_VERSION" --locked --root "$root" \
        >"$log" 2>&1; then
        tail -n 80 "$log" >&2 || true
        return 1
    fi
}

list_tests() {
    prepare >/dev/null
    rg -n '^\s*#\[test\(' "$HELIODOR_DIR/tb" --glob '*.veryl' \
        | sed -E 's/.*#\[test\(([^)]*)\)\].*/\1/' \
        | sort
}

veryl_ignored_flag() {
    if "$RESOLVED_VERYL_BIN" test --help 2>&1 | rg -q -- '--ignored'; then
        printf '%s\n' '--ignored'
    else
        printf '%s\n' '--include-ignored'
    fi
}

runner_enabled() {
    local needle="$1"
    local runner
    for runner in $HELIODOR_RUNNERS; do
        if [[ "$runner" == "$needle" ]]; then
            return 0
        fi
    done
    return 1
}

any_veryl_runner_enabled() {
    local runner
    for runner in $HELIODOR_RUNNERS; do
        case "$runner" in
            veryl-*) return 0 ;;
        esac
    done
    return 1
}

build_celox_runner() {
    local log="$HELIODOR_RESULTS_DIR/celox_runner_build.log"
    echo "Building Celox runner: $CELOX_RUNNER_BIN"
    cargo build --manifest-path "$CELOX_ROOT/Cargo.toml" -p celox \
        --example run_veryl_project_test --release >"$log" 2>&1
}

test_source_files() {
    local test="$1"
    local tb_file
    tb_file="$(rg -l "^\\s*#\\[test\\(${test}\\)\\]" "$HELIODOR_DIR/tb" --glob '*.veryl' | head -n 1)"
    if [[ -z "$tb_file" ]]; then
        echo "error: could not find #[test($test)] under $HELIODOR_DIR/tb" >&2
        return 1
    fi
    (
        cd "$HELIODOR_DIR"
        find src -type f -name '*.veryl' | sort
        realpath --relative-to="$HELIODOR_DIR" "$tb_file"
    )
}

fallback_timeout_sec() {
    local test="$1"
    case "$test" in
        test_soc_smp_linux_boot_8hart) printf '%s\n' 3600 ;;
        test_soc_smp_linux_boot_4hart|test_soc_smp_linux_boot_66_4hart|test_soc_smp_linux_boot_71_4hart) printf '%s\n' 1800 ;;
        test_soc_smp_linux_boot_2hart|test_soc_smp_linux_boot_66_2hart|test_soc_smp_linux_boot_71_2hart) printf '%s\n' 600 ;;
        test_soc_linux_boot|test_soc_linux_boot_66|test_soc_linux_boot_71|test_soc_linux_boot_71v) printf '%s\n' 300 ;;
        test_soc_hvlinux) printf '%s\n' 900 ;;
        *) printf '%s\n' 600 ;;
    esac
}

timeout_sec_for() {
    local runner="$1"
    local test="$2"
    if [[ -n "${HELIODOR_TIMEOUT_SEC:-}" ]]; then
        printf '%s\n' "$HELIODOR_TIMEOUT_SEC"
        return
    fi
    if [[ "$runner" == "celox" && -n "${BASELINE_ELAPSED_NS[$test]:-}" ]]; then
        local baseline_ns="${BASELINE_ELAPSED_NS[$test]}"
        local timeout_sec
        timeout_sec="$(((baseline_ns * HELIODOR_CELOX_TIMEOUT_MULTIPLIER + 999999999) / 1000000000))"
        if ((timeout_sec < 1)); then
            timeout_sec=1
        fi
        printf '%s\n' "$timeout_sec"
        return
    fi
    fallback_timeout_sec "$test"
}

run_in_heliodor() {
    local timeout_sec="$1"
    local log="$2"
    shift 2
    if command -v timeout >/dev/null; then
        timeout --kill-after=10s "${timeout_sec}s" \
            bash -c 'cd "$1" || exit; shift; exec "$@"' bash "$HELIODOR_DIR" "$@" \
            >"$log" 2>&1
    else
        (
            cd "$HELIODOR_DIR"
            "$@"
        ) >"$log" 2>&1
    fi
}

record_baseline() {
    local runner="$1"
    local test="$2"
    local status="$3"
    local elapsed="$4"
    if [[ "$status" != 0 || "$runner" != veryl-* ]]; then
        return
    fi
    if [[ -z "${BASELINE_ELAPSED_NS[$test]:-}" || "$elapsed" -lt "${BASELINE_ELAPSED_NS[$test]}" ]]; then
        BASELINE_ELAPSED_NS[$test]="$elapsed"
    fi
}

run_one() {
    local runner="$1"
    local test="$2"
    local stamp log status start end elapsed ignored_flag timeout_sec
    local -a source_files celox_args
    mapfile -t source_files < <(test_source_files "$test")
    celox_args=()
    for source_file in "${source_files[@]}"; do
        celox_args+=(--source-file "$source_file")
    done
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    log="$HELIODOR_RESULTS_DIR/${stamp}_${runner}_${test}.log"
    timeout_sec="$(timeout_sec_for "$runner" "$test")"
    ignored_flag=""
    case "$runner" in
        veryl-*) ignored_flag="$(veryl_ignored_flag)" ;;
    esac

    echo "== $runner :: $test (timeout ${timeout_sec}s) =="
    start="$(date +%s%N)"
    set +e
    case "$runner" in
        celox)
            run_in_heliodor "$timeout_sec" "$log" \
                "$CELOX_RUNNER_BIN" --project "$HELIODOR_DIR" --test "$test" \
                "${celox_args[@]}" --backend native --opt-level "$CELOX_OPT_LEVEL"
            status="$?"
            ;;
        celox-cranelift)
            run_in_heliodor "$timeout_sec" "$log" \
                "$CELOX_RUNNER_BIN" --project "$HELIODOR_DIR" --test "$test" \
                "${celox_args[@]}" --backend cranelift --opt-level "$CELOX_OPT_LEVEL"
            status="$?"
            ;;
        veryl-cc)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test "$ignored_flag" --test "$test" --backend cc \
                "${source_files[@]}"
            status="$?"
            ;;
        veryl-cranelift)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test "$ignored_flag" --test "$test" --backend cranelift \
                "${source_files[@]}"
            status="$?"
            ;;
        veryl-interpret)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test "$ignored_flag" --test "$test" --backend interpret \
                "${source_files[@]}"
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
    record_baseline "$runner" "$test" "$status" "$elapsed"

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
    if runner_enabled celox || runner_enabled celox-cranelift; then
        build_celox_runner
    fi
    if any_veryl_runner_enabled; then
        RESOLVED_VERYL_BIN="$(resolve_veryl_bin)" || return "$?"
        echo "Using Veryl: $RESOLVED_VERYL_BIN"
    fi
    local overall=0
    for test in $HELIODOR_TESTS; do
        for runner in $HELIODOR_RUNNERS; do
            run_one "$runner" "$test" || overall="$?"
        done
    done
    return "$overall"
}

cmd="${1:-run}"
case "$cmd" in
    prepare) prepare ;;
    list) list_tests ;;
    run) run_all ;;
    -h|--help|help) usage ;;
    *) usage >&2; exit 2 ;;
esac
