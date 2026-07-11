#!/bin/bash
# Compare Celox against Veryl's native simulator on Heliodor testbenches.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CELOX_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

HELIODOR_REPO="${HELIODOR_REPO:-https://github.com/dalance/heliodor.git}"
HELIODOR_REF="${HELIODOR_REF:-7ad830fc0f8506c934b61a853ce2eadfa5926b82}"
HELIODOR_DIR="${HELIODOR_DIR:-$CELOX_ROOT/target/heliodor/source}"
HELIODOR_RESULTS_DIR="${HELIODOR_RESULTS_DIR:-$CELOX_ROOT/target/heliodor/results}"
HELIODOR_TOOLS_DIR="${HELIODOR_TOOLS_DIR:-$CELOX_ROOT/target/heliodor/tools}"
HELIODOR_TESTS="${HELIODOR_TESTS:-test_soc_linux_boot}"
HELIODOR_RUNNERS="${HELIODOR_RUNNERS:-veryl-cranelift veryl-cc celox}"
CELOX_OPT_LEVEL="${CELOX_OPT_LEVEL:-O2}"
CELOX_SIR_PASS_OVERRIDES="${CELOX_SIR_PASS_OVERRIDES:-}"
CELOX_RUNNER_BIN="${CELOX_RUNNER_BIN:-$CELOX_ROOT/target/release/examples/run_veryl_project_test}"
HELIODOR_BUILD_CELOX_RUNNER="${HELIODOR_BUILD_CELOX_RUNNER:-1}"
HELIODOR_CELOX_COMPILE_ONLY="${HELIODOR_CELOX_COMPILE_ONLY:-0}"
HELIODOR_CELOX_COMPILE_TIMEOUT_SEC="${HELIODOR_CELOX_COMPILE_TIMEOUT_SEC:-}"
HELIODOR_CELOX_TIMEOUT_MULTIPLIER="${HELIODOR_CELOX_TIMEOUT_MULTIPLIER:-2}"
HELIODOR_INSTALL_TOOLS="${HELIODOR_INSTALL_TOOLS:-1}"
HELIODOR_VERYL_VERSION="${HELIODOR_VERYL_VERSION:-0.20.2}"
VERYL_BIN="${VERYL_BIN:-}"

declare -A BASELINE_ELAPSED_NS=()

RESULTS_HEADER_V1=$'runner\ttest\tstatus\telapsed_ns\tlog'
RESULTS_HEADER_V2=$'runner\ttest\tstatus\telapsed_ns\tlog\tsemantic_status\texit_status\tprocess_elapsed_ns\treported_elapsed_ns'

# Outputs from classify_celox_result. Keep these as globals so callers retain
# the function's structured result without parsing diagnostic text.
CELOX_SEMANTIC_STATUS="unreported"
CELOX_REPORTED_ELAPSED_NS="NA"
CELOX_RESULT_DIAGNOSTIC=""

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
  CELOX_OPT_LEVEL      O0, O1, or O2 for the Celox runner (default: O2)
  CELOX_SIR_PASS_OVERRIDES
                       space-separated SIR pass overrides, e.g. "-vectorize_concat +gvn"
  CELOX_RUNNER_BIN     prebuilt Celox runner path
  HELIODOR_BUILD_CELOX_RUNNER
                       build CELOX_RUNNER_BIN before Celox runs (default: 1)
  HELIODOR_CELOX_COMPILE_ONLY
                       for Celox runners, build the simulator and exit without running the testbench
  HELIODOR_CELOX_COMPILE_TIMEOUT_SEC
                       optional safety timeout for Celox compile-only mode
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

is_uint() {
    [[ "$1" =~ ^[0-9]+$ ]]
}

is_exit_status() {
    local value="$1"
    [[ "$value" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
    ((${#value} < 3)) && return 0
    ((${#value} == 3)) && ((10#$value <= 255))
}

full_pass_elapsed_ns() {
    local semantic_status="$1"
    local exit_status="$2"
    local process_elapsed_ns="$3"
    if [[ "$semantic_status" == pass && "$exit_status" == 0 ]] \
        && is_uint "$process_elapsed_ns"; then
        printf '%s\n' "$process_elapsed_ns"
    else
        printf '%s\n' NA
    fi
}

validate_result_fields() {
    local runner="$1"
    local test="$2"
    local legacy_status="$3"
    local elapsed_ns="$4"
    local log="$5"
    local semantic_status="$6"
    local exit_status="$7"
    local process_elapsed_ns="$8"
    local reported_elapsed_ns="$9"

    if [[ -z "$runner" || -z "$test" || -z "$log" ]]; then
        echo "result row has an empty runner, test, or log field" >&2
        return 1
    fi
    if ! is_exit_status "$legacy_status" || ! is_exit_status "$exit_status" \
        || [[ "$legacy_status" != "$exit_status" ]]; then
        echo "result row has inconsistent legacy/explicit exit status: $legacy_status vs $exit_status" >&2
        return 1
    fi
    if ! is_uint "$process_elapsed_ns"; then
        echo "result row has invalid process_elapsed_ns: $process_elapsed_ns" >&2
        return 1
    fi
    if [[ "$reported_elapsed_ns" != NA ]] && ! is_uint "$reported_elapsed_ns"; then
        echo "result row has invalid reported_elapsed_ns: $reported_elapsed_ns" >&2
        return 1
    fi

    case "$semantic_status" in
        pass)
            if [[ "$exit_status" != 0 || "$elapsed_ns" != "$process_elapsed_ns" ]] \
                || ! is_uint "$elapsed_ns"; then
                echo "pass result must have exit status 0 and elapsed_ns equal to process_elapsed_ns" >&2
                return 1
            fi
            ;;
        compile-only)
            if [[ "$exit_status" != 0 || "$elapsed_ns" != NA ]]; then
                echo "compile-only result must have exit status 0 and elapsed_ns=NA" >&2
                return 1
            fi
            ;;
        fail)
            if [[ "$exit_status" == 0 || "$elapsed_ns" != NA ]]; then
                echo "fail result must have nonzero exit status and elapsed_ns=NA" >&2
                return 1
            fi
            ;;
        unreported|invalid)
            if [[ "$elapsed_ns" != NA ]]; then
                echo "$semantic_status result must have elapsed_ns=NA" >&2
                return 1
            fi
            ;;
        *)
            echo "result row has unknown semantic_status: $semantic_status" >&2
            return 1
            ;;
    esac
}

# Parse the runner's machine-readable terminal record. Exactly one result line
# must exist and it must match both the requested test and the complete grammar.
parse_celox_result_marker() {
    local log="$1"
    local expected_test="$2"
    local line marker_count=0 marker_valid=0 marker_test="" marker_status="" marker_elapsed=""
    local pattern='^CELOX_TEST_RESULT test=([^[:space:]]+) status=(pass|fail|compile-only) elapsed_ns=([0-9]+)$'

    CELOX_SEMANTIC_STATUS="unreported"
    CELOX_REPORTED_ELAPSED_NS="NA"
    CELOX_RESULT_DIAGNOSTIC=""
    if [[ ! -f "$log" ]]; then
        CELOX_RESULT_DIAGNOSTIC="Celox log does not exist: $log"
        return 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" != CELOX_TEST_RESULT* ]]; then
            continue
        fi
        marker_count=$((marker_count + 1))
        if [[ "$line" =~ $pattern ]]; then
            marker_valid=$((marker_valid + 1))
            marker_test="${BASH_REMATCH[1]}"
            marker_status="${BASH_REMATCH[2]}"
            marker_elapsed="${BASH_REMATCH[3]}"
        fi
    done <"$log"
    if ((marker_count == 0)); then
        CELOX_RESULT_DIAGNOSTIC="Celox log has no CELOX_TEST_RESULT record"
        return 1
    fi
    if ((marker_count != 1 || marker_valid != 1)); then
        CELOX_SEMANTIC_STATUS="invalid"
        CELOX_RESULT_DIAGNOSTIC="Celox log must contain exactly one well-formed CELOX_TEST_RESULT record (records=$marker_count valid=$marker_valid)"
        return 1
    fi
    CELOX_REPORTED_ELAPSED_NS="$marker_elapsed"
    if [[ "$marker_test" != "$expected_test" ]]; then
        CELOX_SEMANTIC_STATUS="invalid"
        CELOX_RESULT_DIAGNOSTIC="Celox result names test $marker_test, expected $expected_test"
        return 1
    fi
    CELOX_SEMANTIC_STATUS="$marker_status"
}

# Check the marker independently against the process exit status and requested
# run mode. A contradiction is an invalid result, never a process-level pass.
classify_celox_result() {
    local log="$1"
    local expected_test="$2"
    local exit_status="$3"
    local expected_compile_only="$4"
    parse_celox_result_marker "$log" "$expected_test" || return 1
    local marker_status="$CELOX_SEMANTIC_STATUS"

    if ! is_exit_status "$exit_status"; then
        CELOX_SEMANTIC_STATUS="invalid"
        CELOX_RESULT_DIAGNOSTIC="Celox process exit status is not an unsigned byte: $exit_status"
        return 1
    fi
    if [[ "$expected_compile_only" != 0 && "$expected_compile_only" != 1 ]]; then
        CELOX_SEMANTIC_STATUS="invalid"
        CELOX_RESULT_DIAGNOSTIC="expected compile-only mode must be 0 or 1, got $expected_compile_only"
        return 1
    fi

    case "$marker_status:$exit_status:$expected_compile_only" in
        pass:0:0|compile-only:0:1|fail:[1-9]*:0) return 0 ;;
        pass:*:1)
            CELOX_RESULT_DIAGNOSTIC="Celox reported pass during a requested compile-only run"
            ;;
        compile-only:*:0)
            CELOX_RESULT_DIAGNOSTIC="Celox reported compile-only during a full test run"
            ;;
        fail:*:1)
            CELOX_RESULT_DIAGNOSTIC="Celox reported a semantic test failure during a requested compile-only run"
            ;;
        pass:*:*) CELOX_RESULT_DIAGNOSTIC="Celox reported pass but exited with status $exit_status" ;;
        compile-only:*:*) CELOX_RESULT_DIAGNOSTIC="Celox reported compile-only but exited with status $exit_status" ;;
        fail:0:*) CELOX_RESULT_DIAGNOSTIC="Celox reported fail but exited successfully" ;;
        *) CELOX_RESULT_DIAGNOSTIC="Celox result is inconsistent with its process or mode" ;;
    esac
    CELOX_SEMANTIC_STATUS="invalid"
    return 1
}

# Migration uses the historical record itself rather than today's requested
# compile-only mode. This recovers exact Celox semantics when the old log still
# exists, while refusing to call an absent or contradictory marker a pass.
classify_legacy_celox_result() {
    local log="$1"
    local expected_test="$2"
    local exit_status="$3"
    parse_celox_result_marker "$log" "$expected_test" || return 1
    local marker_status="$CELOX_SEMANTIC_STATUS"
    case "$marker_status:$exit_status" in
        pass:0|compile-only:0)
            return 0
            ;;
        fail:0)
            CELOX_SEMANTIC_STATUS="invalid"
            CELOX_RESULT_DIAGNOSTIC="legacy Celox result reports fail with exit status 0"
            return 1
            ;;
        fail:*)
            CELOX_SEMANTIC_STATUS="fail"
            return 0
            ;;
        *)
            CELOX_SEMANTIC_STATUS="invalid"
            CELOX_RESULT_DIAGNOSTIC="Celox result $marker_status contradicts exit status $exit_status"
            return 1
            ;;
    esac
}

append_result_row() {
    local results_file="$1"
    local runner="$2"
    local test="$3"
    local exit_status="$4"
    local elapsed_ns="$5"
    local log="$6"
    local semantic_status="$7"
    local process_elapsed_ns="$8"
    local reported_elapsed_ns="$9"
    validate_result_fields \
        "$runner" "$test" "$exit_status" "$elapsed_ns" "$log" \
        "$semantic_status" "$exit_status" "$process_elapsed_ns" "$reported_elapsed_ns" \
        || return 1
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$runner" "$test" "$exit_status" "$elapsed_ns" "$log" \
        "$semantic_status" "$exit_status" "$process_elapsed_ns" "$reported_elapsed_ns" \
        | tee -a "$results_file"
}

migrate_results_v1() {
    local results_file="$1"
    local backup_file="${results_file}.v1.bak"
    local temporary_file
    temporary_file="$(mktemp "${results_file}.migrate.XXXXXX")"
    printf '%s\n' "$RESULTS_HEADER_V2" >"$temporary_file"

    local runner test legacy_status legacy_elapsed log extra
    local semantic_status process_elapsed reported_elapsed full_elapsed
    while IFS=$'\t' read -r runner test legacy_status legacy_elapsed log extra; do
        if [[ -z "$runner$test$legacy_status$legacy_elapsed$log$extra" ]]; then
            continue
        fi
        if [[ -n "$extra" || -z "$runner" || -z "$test" || -z "$log" ]] \
            || ! is_uint "$legacy_status" || ! is_uint "$legacy_elapsed"; then
            rm -f "$temporary_file"
            echo "error: cannot migrate malformed v1 results row for runner=${runner:-<empty>} test=${test:-<empty>}" >&2
            return 1
        fi

        process_elapsed="$legacy_elapsed"
        reported_elapsed="NA"
        case "$runner" in
            celox*)
                classify_legacy_celox_result "$log" "$test" "$legacy_status" || true
                semantic_status="$CELOX_SEMANTIC_STATUS"
                reported_elapsed="$CELOX_REPORTED_ELAPSED_NS"
                ;;
            veryl-*)
                if [[ "$legacy_status" == 0 ]]; then
                    semantic_status="pass"
                else
                    semantic_status="fail"
                fi
                ;;
            *) semantic_status="unreported" ;;
        esac
        full_elapsed="$(full_pass_elapsed_ns "$semantic_status" "$legacy_status" "$process_elapsed")"
        if ! validate_result_fields \
            "$runner" "$test" "$legacy_status" "$full_elapsed" "$log" \
            "$semantic_status" "$legacy_status" "$process_elapsed" "$reported_elapsed"; then
            rm -f "$temporary_file"
            echo "error: cannot migrate inconsistent v1 results row for runner=$runner test=$test" >&2
            return 1
        fi
        printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
            "$runner" "$test" "$legacy_status" "$full_elapsed" "$log" \
            "$semantic_status" "$legacy_status" "$process_elapsed" "$reported_elapsed" \
            >>"$temporary_file"
    done < <(tail -n +2 "$results_file")

    if [[ ! -e "$backup_file" ]]; then
        if ! cp "$results_file" "$backup_file"; then
            rm -f "$temporary_file"
            return 1
        fi
    fi
    chmod --reference="$results_file" "$temporary_file" 2>/dev/null || true
    if ! mv "$temporary_file" "$results_file"; then
        rm -f "$temporary_file"
        return 1
    fi
}

validate_results_v2() {
    local results_file="$1"
    local line_number=1
    local runner test legacy_status elapsed log semantic_status exit_status process_elapsed reported_elapsed extra
    while IFS=$'\t' read -r runner test legacy_status elapsed log semantic_status exit_status process_elapsed reported_elapsed extra; do
        line_number=$((line_number + 1))
        if [[ -n "$extra" ]] || ! validate_result_fields \
            "$runner" "$test" "$legacy_status" "$elapsed" "$log" \
            "$semantic_status" "$exit_status" "$process_elapsed" "$reported_elapsed"; then
            echo "error: invalid v2 result row at $results_file:$line_number" >&2
            return 1
        fi
    done < <(tail -n +2 "$results_file")
}

ensure_results_schema() {
    local results_file="$1"
    if [[ ! -e "$results_file" || ! -s "$results_file" ]]; then
        printf '%s\n' "$RESULTS_HEADER_V2" >"$results_file"
        return
    fi

    local header
    IFS= read -r header <"$results_file" || true
    case "$header" in
        "$RESULTS_HEADER_V2")
            validate_results_v2 "$results_file"
            ;;
        "$RESULTS_HEADER_V1") migrate_results_v1 "$results_file" ;;
        *)
            echo "error: unsupported Heliodor results schema in $results_file" >&2
            echo "found: $header" >&2
            return 1
            ;;
    esac
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
    if [[ "$HELIODOR_BUILD_CELOX_RUNNER" != 1 ]]; then
        if [[ ! -x "$CELOX_RUNNER_BIN" ]]; then
            echo "error: HELIODOR_BUILD_CELOX_RUNNER=0 but CELOX_RUNNER_BIN is not executable: $CELOX_RUNNER_BIN" >&2
            return 127
        fi
        echo "Using prebuilt Celox runner: $CELOX_RUNNER_BIN"
        return
    fi
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
    if [[ "$runner" == celox* && "$HELIODOR_CELOX_COMPILE_ONLY" == 1 ]]; then
        printf '%s\n' "$HELIODOR_CELOX_COMPILE_TIMEOUT_SEC"
        return
    fi
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
    if [[ -n "$timeout_sec" && "$timeout_sec" != 0 ]] && command -v timeout >/dev/null; then
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
    local semantic_status="$3"
    local exit_status="$4"
    local elapsed="$5"
    if [[ "$semantic_status" != pass || "$exit_status" != 0 || "$runner" != veryl-* ]]; then
        return
    fi
    if [[ -z "${BASELINE_ELAPSED_NS[$test]:-}" || "$elapsed" -lt "${BASELINE_ELAPSED_NS[$test]}" ]]; then
        BASELINE_ELAPSED_NS[$test]="$elapsed"
    fi
}

run_one() {
    local runner="$1"
    local test="$2"
    local stamp log process_status start end process_elapsed ignored_flag timeout_sec
    local semantic_status reported_elapsed full_elapsed result_valid
    local -a source_files celox_args
    mapfile -t source_files < <(test_source_files "$test")
    celox_args=()
    for source_file in "${source_files[@]}"; do
        celox_args+=(--source-file "$source_file")
    done
    for pass_override in $CELOX_SIR_PASS_OVERRIDES; do
        celox_args+=(--sir-pass "$pass_override")
    done
    if [[ "$HELIODOR_CELOX_COMPILE_ONLY" == 1 ]]; then
        celox_args+=(--compile-only)
    fi
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    log="$HELIODOR_RESULTS_DIR/${stamp}_${runner}_${test}.log"
    timeout_sec="$(timeout_sec_for "$runner" "$test")"
    ignored_flag=""
    case "$runner" in
        veryl-*) ignored_flag="$(veryl_ignored_flag)" ;;
    esac

    if [[ "$runner" == celox* && "$HELIODOR_CELOX_COMPILE_ONLY" == 1 ]]; then
        if [[ -n "$timeout_sec" && "$timeout_sec" != 0 ]]; then
            echo "== $runner :: $test (compile-only, safety timeout ${timeout_sec}s) =="
        else
            echo "== $runner :: $test (compile-only) =="
        fi
    elif [[ -n "$timeout_sec" && "$timeout_sec" != 0 ]]; then
        echo "== $runner :: $test (timeout ${timeout_sec}s) =="
    else
        echo "== $runner :: $test =="
    fi
    start="$(date +%s%N)"
    set +e
    case "$runner" in
        celox)
            run_in_heliodor "$timeout_sec" "$log" \
                "$CELOX_RUNNER_BIN" --project "$HELIODOR_DIR" --test "$test" \
                "${celox_args[@]}" --backend native --opt-level "$CELOX_OPT_LEVEL"
            process_status="$?"
            ;;
        celox-cranelift)
            run_in_heliodor "$timeout_sec" "$log" \
                "$CELOX_RUNNER_BIN" --project "$HELIODOR_DIR" --test "$test" \
                "${celox_args[@]}" --backend cranelift --opt-level "$CELOX_OPT_LEVEL"
            process_status="$?"
            ;;
        veryl-cc)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test "$ignored_flag" --test "$test" --backend cc \
                "${source_files[@]}"
            process_status="$?"
            ;;
        veryl-cranelift)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test "$ignored_flag" --test "$test" --backend cranelift \
                "${source_files[@]}"
            process_status="$?"
            ;;
        veryl-interpret)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test "$ignored_flag" --test "$test" --backend interpret \
                "${source_files[@]}"
            process_status="$?"
            ;;
        *)
            echo "unknown runner: $runner" >"$log"
            process_status=2
            ;;
    esac
    set -e
    end="$(date +%s%N)"
    process_elapsed="$((end - start))"
    semantic_status="unreported"
    reported_elapsed="NA"
    result_valid=1
    case "$runner" in
        celox*)
            if classify_celox_result \
                "$log" "$test" "$process_status" "$HELIODOR_CELOX_COMPILE_ONLY"; then
                semantic_status="$CELOX_SEMANTIC_STATUS"
                reported_elapsed="$CELOX_REPORTED_ELAPSED_NS"
            else
                semantic_status="$CELOX_SEMANTIC_STATUS"
                reported_elapsed="$CELOX_REPORTED_ELAPSED_NS"
                result_valid=0
                echo "error: $CELOX_RESULT_DIAGNOSTIC" >&2
            fi
            ;;
        veryl-*)
            if [[ "$process_status" == 0 ]]; then
                semantic_status="pass"
            else
                semantic_status="fail"
            fi
            ;;
    esac
    full_elapsed="$(full_pass_elapsed_ns "$semantic_status" "$process_status" "$process_elapsed")"
    record_baseline "$runner" "$test" "$semantic_status" "$process_status" "$process_elapsed"

    if ! append_result_row "$HELIODOR_RESULTS_DIR/results.tsv" \
        "$runner" "$test" "$process_status" "$full_elapsed" "$log" \
        "$semantic_status" "$process_elapsed" "$reported_elapsed"; then
        echo "error: refusing to append an invalid Heliodor result row" >&2
        return 1
    fi
    if [[ "$process_status" != 0 || "$result_valid" != 1 ]]; then
        tail -n 40 "$log" >&2 || true
        if [[ "$process_status" != 0 ]]; then
            return "$process_status"
        fi
        return 1
    fi
}

run_all() {
    prepare
    mkdir -p "$HELIODOR_RESULTS_DIR"
    ensure_results_schema "$HELIODOR_RESULTS_DIR/results.tsv" || return "$?"
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

main() {
    local cmd="${1:-run}"
    case "$cmd" in
        prepare) prepare ;;
        list) list_tests ;;
        run) run_all ;;
        -h|--help|help) usage ;;
        *) usage >&2; return 2 ;;
    esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
