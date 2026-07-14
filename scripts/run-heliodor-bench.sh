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
HELIODOR_CELOX_CARGO_PROFILE="${HELIODOR_CELOX_CARGO_PROFILE:-heliodor-dev}"
CELOX_RUNNER_BIN="${CELOX_RUNNER_BIN:-$CELOX_ROOT/target/$HELIODOR_CELOX_CARGO_PROFILE/examples/run_veryl_project_test}"
HELIODOR_BUILD_CELOX_RUNNER="${HELIODOR_BUILD_CELOX_RUNNER:-1}"
HELIODOR_CELOX_TARGET_DIR="${HELIODOR_CELOX_TARGET_DIR:-}"
HELIODOR_CELOX_COMPILE_ONLY="${HELIODOR_CELOX_COMPILE_ONLY:-0}"
HELIODOR_CELOX_COMPILE_TIMEOUT_SEC="${HELIODOR_CELOX_COMPILE_TIMEOUT_SEC:-}"
HELIODOR_CELOX_TIMEOUT_MULTIPLIER="${HELIODOR_CELOX_TIMEOUT_MULTIPLIER:-2}"
HELIODOR_INSTALL_TOOLS="${HELIODOR_INSTALL_TOOLS:-1}"
HELIODOR_VERYL_VERSION="${HELIODOR_VERYL_VERSION:-0.20.2}"
VERYL_BIN="${VERYL_BIN:-}"

readonly GATE_HELIODOR_REF=7ad830fc0f8506c934b61a853ce2eadfa5926b82
readonly GATE_VERYL_VERSION=0.20.2
readonly GATE_TEST=test_soc_linux_boot
readonly GATE_TIMEOUT_SEC=300

# Populated only while `gate` owns detached Heliodor worktrees. Keeping these
# paths global lets the direct-execution EXIT trap clean up an interrupted gate.
GATE_WORKTREE_REPO=""
GATE_WORKTREE_ROOT=""

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
usage: scripts/run-heliodor-bench.sh [prepare|list|run|gate]

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
  HELIODOR_CELOX_CARGO_PROFILE
                       Cargo profile for the Celox runner (default: heliodor-dev)
  HELIODOR_BUILD_CELOX_RUNNER
                       build CELOX_RUNNER_BIN before Celox runs (default: 1)
  HELIODOR_CELOX_TARGET_DIR
                       optional explicit Cargo target directory for the Celox build
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
  scripts/run-heliodor-bench.sh gate

`gate` is the fixed acceptance comparison. Unlike diagnostic `run`, it ignores
runner/test/configuration overrides, uses isolated generated trees, and exits
successfully only when full native O2 Celox passes no slower than veryl-cc.
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

tsv_has_exact_field_count() {
    local line="$1"
    local expected_fields="$2"
    local without_tabs
    if [[ "$line" == *$'\x1f'* ]]; then
        return 1
    fi
    without_tabs="${line//$'\t'/}"
    ((${#line} - ${#without_tabs} == expected_fields - 1))
}

# Python exposes the platform monotonic clock directly. The gate refuses to
# substitute wall-clock time because an NTP/clock adjustment can reverse or
# corrupt the comparison. Keeping this behind one function also makes the
# boundary deterministic in fixture tests.
monotonic_ns() {
    if ! command -v python3 >/dev/null; then
        echo "error: python3 is required for monotonic Heliodor timing" >&2
        return 127
    fi
    python3 -c 'import time; print(time.monotonic_ns())'
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

validate_gate_celox_config() {
    local log="$1"
    local expected_test="$2"
    local expected="CELOX_TEST_CONFIG test=$expected_test backend=native opt_level=O2 four_state=false compile_only=false"
    local line config_count=0 valid_count=0

    if [[ ! -f "$log" ]]; then
        echo "error: Celox gate log does not exist: $log" >&2
        return 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" != CELOX_TEST_CONFIG* ]]; then
            continue
        fi
        config_count=$((config_count + 1))
        if [[ "$line" == "$expected" ]]; then
            valid_count=$((valid_count + 1))
        fi
    done <"$log"
    if ((config_count != 1 || valid_count != 1)); then
        echo "error: Celox gate log must contain exactly one exact config record" >&2
        echo "expected: $expected" >&2
        echo "records=$config_count exact=$valid_count" >&2
        return 1
    fi
}

validate_veryl_completion() {
    local log="$1"
    local expected_test="$2"
    local expected_success="[INFO ]    Succeeded test ($expected_test)"
    local expected_completion="[INFO ]    Completed tests : 1 passed, 0 failed"
    local line success_count=0 exact_success_count=0 completion_count=0 exact_completion_count=0

    if [[ ! -f "$log" ]]; then
        echo "error: Veryl gate log does not exist: $log" >&2
        return 1
    fi
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" == *"Succeeded test ("* ]]; then
            success_count=$((success_count + 1))
            if [[ "$line" == "$expected_success" ]]; then
                exact_success_count=$((exact_success_count + 1))
            fi
        fi
        if [[ "$line" == *"Completed tests :"* ]]; then
            completion_count=$((completion_count + 1))
            if [[ "$line" == "$expected_completion" ]]; then
                exact_completion_count=$((exact_completion_count + 1))
            fi
        fi
    done <"$log"
    if ((success_count != 1 || exact_success_count != 1 \
        || completion_count != 1 || exact_completion_count != 1)); then
        echo "error: Veryl gate log must report exactly $expected_test and 1 passed, 0 failed" >&2
        echo "success records=$success_count exact=$exact_success_count; completion records=$completion_count exact=$exact_completion_count" >&2
        return 1
    fi
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
    local line normalized
    local runner test legacy_status elapsed log semantic_status exit_status process_elapsed reported_elapsed extra
    while IFS= read -r line || [[ -n "$line" ]]; do
        line_number=$((line_number + 1))
        if ! tsv_has_exact_field_count "$line" 9; then
            echo "error: invalid v2 field count at $results_file:$line_number" >&2
            return 1
        fi
        normalized="${line//$'\t'/$'\x1f'}"
        IFS=$'\x1f' read -r runner test legacy_status elapsed log semantic_status exit_status process_elapsed reported_elapsed extra \
            <<<"$normalized"
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
    if ! mkdir -p "$(dirname "$HELIODOR_DIR")" "$HELIODOR_RESULTS_DIR" "$HELIODOR_TOOLS_DIR"; then
        echo "error: could not create Heliodor benchmark directories" >&2
        return 1
    fi
    if [[ -d "$HELIODOR_DIR/.git" ]]; then
        if ! git -C "$HELIODOR_DIR" fetch --quiet origin; then
            echo "error: could not fetch Heliodor origin" >&2
            return 1
        fi
    elif [[ -e "$HELIODOR_DIR" ]]; then
        echo "error: $HELIODOR_DIR exists but is not a git checkout" >&2
        return 1
    else
        if ! git clone --quiet "$HELIODOR_REPO" "$HELIODOR_DIR"; then
            echo "error: could not clone Heliodor" >&2
            return 1
        fi
    fi
    if ! git -C "$HELIODOR_DIR" checkout --quiet "$HELIODOR_REF"; then
        echo "error: could not check out Heliodor ref $HELIODOR_REF" >&2
        return 1
    fi
    if ! rm -f "$HELIODOR_DIR/.build/lock"; then
        echo "error: could not remove stale Heliodor build lock" >&2
        return 1
    fi
    local head
    if ! head="$(git -C "$HELIODOR_DIR" rev-parse HEAD)"; then
        echo "error: could not resolve Heliodor HEAD" >&2
        return 1
    fi
    echo "Heliodor: $head at $HELIODOR_DIR"
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
    local -a target_dir_args=()
    if [[ "$HELIODOR_BUILD_CELOX_RUNNER" != 1 ]]; then
        if [[ ! -x "$CELOX_RUNNER_BIN" ]]; then
            echo "error: HELIODOR_BUILD_CELOX_RUNNER=0 but CELOX_RUNNER_BIN is not executable: $CELOX_RUNNER_BIN" >&2
            return 127
        fi
        echo "Using prebuilt Celox runner: $CELOX_RUNNER_BIN"
        return
    fi
    echo "Building Celox runner: $CELOX_RUNNER_BIN"
    if [[ -n "$HELIODOR_CELOX_TARGET_DIR" ]]; then
        target_dir_args=(--target-dir "$HELIODOR_CELOX_TARGET_DIR")
    fi
    if ! env -u CARGO_TARGET_DIR -u CARGO_BUILD_TARGET \
        cargo build --manifest-path "$CELOX_ROOT/Cargo.toml" -p celox \
        --example run_veryl_project_test --profile "$HELIODOR_CELOX_CARGO_PROFILE" --locked \
        "${target_dir_args[@]}" >"$log" 2>&1; then
        tail -n 80 "$log" >&2 || true
        return 1
    fi
    if [[ ! -f "$CELOX_RUNNER_BIN" || ! -x "$CELOX_RUNNER_BIN" ]]; then
        echo "error: Celox build did not produce the selected runner: $CELOX_RUNNER_BIN" >&2
        return 1
    fi
}

test_source_files() {
    local test="$1"
    local tb_output tb_file source_output relative_tb
    local -a tb_files=()
    if ! tb_output="$(rg -l "^\\s*#\\[test\\(${test}\\)\\]" \
        "$HELIODOR_DIR/tb" --glob '*.veryl')"; then
        echo "error: could not find #[test($test)] under $HELIODOR_DIR/tb" >&2
        return 1
    fi
    mapfile -t tb_files <<<"$tb_output"
    if ((${#tb_files[@]} != 1)) || [[ -z "${tb_files[0]}" ]]; then
        echo "error: expected exactly one #[test($test)] file, found ${#tb_files[@]}" >&2
        return 1
    fi
    tb_file="${tb_files[0]}"
    if ! source_output="$({
        cd "$HELIODOR_DIR" || exit 1
        [[ -d src ]] || exit 1
        find src -type f -name '*.veryl' | LC_ALL=C sort
    })"; then
        echo "error: could not enumerate tracked sources under $HELIODOR_DIR/src" >&2
        return 1
    fi
    if [[ -z "$source_output" ]]; then
        echo "error: no Veryl sources found under $HELIODOR_DIR/src" >&2
        return 1
    fi
    if ! relative_tb="$(realpath --relative-to="$HELIODOR_DIR" "$tb_file")"; then
        echo "error: could not canonicalize test source $tb_file" >&2
        return 1
    fi
    printf '%s\n%s\n' "$source_output" "$relative_tb"
}

collect_test_source_files() {
    local test="$1"
    local output_name="$2"
    local source_list path
    local -n output="$output_name"

    if ! source_list="$(test_source_files "$test")"; then
        echo "error: could not enumerate source files for $test" >&2
        return 1
    fi
    if [[ -z "$source_list" ]]; then
        echo "error: source-file list is empty for $test" >&2
        return 1
    fi
    mapfile -t output <<<"$source_list"
    if ((${#output[@]} == 0)); then
        echo "error: source-file list is empty for $test" >&2
        return 1
    fi
    for path in "${output[@]}"; do
        if [[ -z "$path" || "$path" == *$'\t'* || "$path" == *$'\n'* ]]; then
            echo "error: invalid source-file path for $test" >&2
            return 1
        fi
    done
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
    local stamp log process_status start end process_elapsed timeout_sec
    local semantic_status reported_elapsed full_elapsed result_valid
    local -a source_files celox_args
    collect_test_source_files "$test" source_files || return "$?"
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
    if ! start="$(monotonic_ns)" || ! is_uint "$start"; then
        echo "error: could not read a monotonic start timestamp" >&2
        return 1
    fi
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
                "$RESOLVED_VERYL_BIN" test --test "$test" --backend cc \
                "${source_files[@]}"
            process_status="$?"
            ;;
        veryl-cranelift)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test --test "$test" --backend cranelift \
                "${source_files[@]}"
            process_status="$?"
            ;;
        veryl-interpret)
            run_in_heliodor "$timeout_sec" "$log" \
                "$RESOLVED_VERYL_BIN" test --test "$test" --backend interpret \
                "${source_files[@]}"
            process_status="$?"
            ;;
        *)
            echo "unknown runner: $runner" >"$log"
            process_status=2
            ;;
    esac
    set -e
    if ! end="$(monotonic_ns)" || ! is_uint "$end"; then
        echo "error: could not read a monotonic end timestamp" >&2
        return 1
    fi
    if ((end < start)); then
        echo "error: monotonic clock moved backwards ($start -> $end)" >&2
        return 1
    fi
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
            if [[ "$process_status" == 0 ]] && validate_veryl_completion "$log" "$test"; then
                semantic_status="pass"
            else
                semantic_status="fail"
                result_valid=0
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

GATE_CELOX_HEAD=""
GATE_VERYL_ELAPSED_NS=""
GATE_CELOX_ELAPSED_NS=""

gate_require_clean_checkout() {
    local directory="$1"
    local label="$2"
    local status
    if ! git -C "$directory" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        echo "error: $label is not a Git checkout: $directory" >&2
        return 1
    fi
    if ! status="$(git -C "$directory" status --porcelain --untracked-files=all)"; then
        echo "error: could not inspect $label checkout status: $directory" >&2
        return 1
    fi
    if [[ -n "$status" ]]; then
        echo "error: $label checkout must be clean: $directory" >&2
        printf '%s\n' "$status" >&2
        return 1
    fi
}

gate_record_celox_checkout() {
    gate_require_clean_checkout "$CELOX_ROOT" Celox || return "$?"
    GATE_CELOX_HEAD="$(git -C "$CELOX_ROOT" rev-parse HEAD)"
    if [[ ! "$GATE_CELOX_HEAD" =~ ^[0-9a-f]{40}$ ]]; then
        echo "error: Celox HEAD is not a full commit ID: $GATE_CELOX_HEAD" >&2
        return 1
    fi
}

gate_verify_celox_checkout() {
    local current_head
    gate_require_clean_checkout "$CELOX_ROOT" Celox || return "$?"
    current_head="$(git -C "$CELOX_ROOT" rev-parse HEAD)"
    if [[ -z "$GATE_CELOX_HEAD" || "$current_head" != "$GATE_CELOX_HEAD" ]]; then
        echo "error: Celox HEAD changed during the gate: ${GATE_CELOX_HEAD:-<unset>} -> $current_head" >&2
        return 1
    fi
}

gate_verify_heliodor_checkout() {
    local directory="$1"
    local head
    gate_require_clean_checkout "$directory" Heliodor || return "$?"
    head="$(git -C "$directory" rev-parse HEAD)"
    if [[ "$head" != "$GATE_HELIODOR_REF" ]]; then
        echo "error: Heliodor gate requires $GATE_HELIODOR_REF, found $head" >&2
        return 1
    fi
}

resolve_gate_veryl_bin() {
    local bin version
    bin="$(installed_veryl_bin)"
    if [[ ! -x "$bin" ]]; then
        install_veryl || return "$?"
    fi
    if [[ ! -x "$bin" || -L "$bin" ]]; then
        echo "error: gate Veryl must be a benchmark-owned regular executable: $bin" >&2
        return 1
    fi
    version="$("$bin" --version 2>&1)" || {
        echo "error: could not execute gate Veryl: $bin" >&2
        return 1
    }
    if [[ "$version" != "veryl $GATE_VERYL_VERSION" ]]; then
        echo "error: gate requires 'veryl $GATE_VERYL_VERSION', found '$version' at $bin" >&2
        return 1
    fi
    printf '%s\n' "$bin"
}

gate_file_hash() {
    local path="$1"
    if [[ ! -f "$path" || ! -x "$path" ]]; then
        echo "error: gate runner is not an executable regular file: $path" >&2
        return 1
    fi
    git hash-object --no-filters "$path"
}

gate_verify_file_hash() {
    local path="$1"
    local expected="$2"
    local label="$3"
    local actual
    actual="$(gate_file_hash "$path")" || return "$?"
    if [[ "$actual" != "$expected" ]]; then
        echo "error: $label executable changed during the gate: $path" >&2
        return 1
    fi
}

# Print the exact metadata and source-file manifest consumed for one test. The
# detached worktree HEAD check proves all other tracked inputs; this manifest
# additionally proves that runner-side generation did not alter the selected
# source set or its contents.
gate_source_manifest() {
    local test="$1"
    local path object_id
    local -a paths=()
    collect_test_source_files "$test" paths || return "$?"
    paths+=(Veryl.toml Veryl.lock)
    for path in "${paths[@]}"; do
        if [[ "$path" == /* || "$path" == *$'\t'* || "$path" == *$'\n'* ]]; then
            echo "error: invalid Heliodor manifest path: $path" >&2
            return 1
        fi
        if ! git -C "$HELIODOR_DIR" ls-files --error-unmatch -- "$path" >/dev/null 2>&1; then
            echo "error: Heliodor gate input is not tracked: $path" >&2
            return 1
        fi
        object_id="$(git -C "$HELIODOR_DIR" hash-object --no-filters -- "$path")" \
            || return "$?"
        printf '%s\t%s\n' "$path" "$object_id"
    done | LC_ALL=C sort
}

gate_create_worktrees() {
    local runner path
    mkdir -p "$GATE_WORKTREE_ROOT"
    for runner in veryl-cc celox; do
        path="$GATE_WORKTREE_ROOT/$runner"
        if ! git -C "$GATE_WORKTREE_REPO" worktree add --quiet --detach \
            "$path" "$GATE_HELIODOR_REF"; then
            echo "error: could not create isolated Heliodor worktree for $runner" >&2
            return 1
        fi
    done
}

gate_cleanup_worktrees() {
    local runner path
    if [[ -z "$GATE_WORKTREE_REPO" || -z "$GATE_WORKTREE_ROOT" ]]; then
        return
    fi
    for runner in veryl-cc celox; do
        path="$GATE_WORKTREE_ROOT/$runner"
        if [[ -e "$path" ]]; then
            git -C "$GATE_WORKTREE_REPO" worktree remove --force "$path" \
                >/dev/null 2>&1 || true
        fi
    done
    git -C "$GATE_WORKTREE_REPO" worktree prune >/dev/null 2>&1 || true
    rmdir "$GATE_WORKTREE_ROOT" >/dev/null 2>&1 || true
    GATE_WORKTREE_REPO=""
    GATE_WORKTREE_ROOT=""
}

validate_gate_results() {
    local results_file="$1"
    local invocation_dir="$2"
    local header row_count=0 canonical_invocation canonical_results canonical_log line normalized
    local runner test legacy_status elapsed log semantic_status exit_status process_elapsed reported_elapsed extra
    local expected_runner

    GATE_VERYL_ELAPSED_NS=""
    GATE_CELOX_ELAPSED_NS=""
    if [[ ! -f "$results_file" ]]; then
        echo "error: gate result file does not exist: $results_file" >&2
        return 1
    fi
    canonical_invocation="$(realpath -e "$invocation_dir")" || {
        echo "error: gate invocation directory cannot be canonicalized: $invocation_dir" >&2
        return 1
    }
    canonical_results="$(realpath -e "$results_file")" || {
        echo "error: gate result file cannot be canonicalized: $results_file" >&2
        return 1
    }
    if [[ "$canonical_results" != "$canonical_invocation/results.tsv" ]]; then
        echo "error: gate result file is not owned by this invocation: $results_file" >&2
        return 1
    fi
    IFS= read -r header <"$results_file" || true
    if [[ "$header" != "$RESULTS_HEADER_V2" ]]; then
        echo "error: gate result file has the wrong schema" >&2
        return 1
    fi

    while IFS= read -r line || [[ -n "$line" ]]; do
        row_count=$((row_count + 1))
        case "$row_count" in
            1) expected_runner=veryl-cc ;;
            2) expected_runner=celox ;;
            *)
                echo "error: gate result file contains more than two rows" >&2
                return 1
                ;;
        esac
        if ! tsv_has_exact_field_count "$line" 9; then
            echo "error: gate result row $row_count does not have exactly nine fields" >&2
            return 1
        fi
        normalized="${line//$'\t'/$'\x1f'}"
        IFS=$'\x1f' read -r runner test legacy_status elapsed log semantic_status exit_status process_elapsed reported_elapsed extra \
            <<<"$normalized"
        if [[ -n "$extra" ]] || ! validate_result_fields \
            "$runner" "$test" "$legacy_status" "$elapsed" "$log" \
            "$semantic_status" "$exit_status" "$process_elapsed" "$reported_elapsed"; then
            echo "error: invalid gate result row $row_count" >&2
            return 1
        fi
        if [[ "$runner" != "$expected_runner" || "$test" != "$GATE_TEST" ]]; then
            echo "error: gate row $row_count must be $expected_runner/$GATE_TEST, found $runner/$test" >&2
            return 1
        fi
        if [[ "$semantic_status" != pass || "$exit_status" != 0 \
            || "$elapsed" != "$process_elapsed" || "$process_elapsed" == 0 ]]; then
            echo "error: gate row $row_count is not a full semantic pass with positive process time" >&2
            return 1
        fi
        if [[ ! -f "$log" || -L "$log" ]]; then
            echo "error: gate row $row_count log is not a regular invocation-owned file: $log" >&2
            return 1
        fi
        canonical_log="$(realpath -e "$log")" || {
            echo "error: gate row $row_count log cannot be canonicalized: $log" >&2
            return 1
        }
        if [[ "$(dirname "$canonical_log")" != "$canonical_invocation" ]]; then
            echo "error: gate row $row_count references a log outside this invocation: $log" >&2
            return 1
        fi
        case "$runner" in
            veryl-cc)
                [[ "$reported_elapsed" == NA ]] || {
                    echo "error: Veryl gate row has an unexpected reported elapsed value" >&2
                    return 1
                }
                validate_veryl_completion "$log" "$GATE_TEST" || return "$?"
                GATE_VERYL_ELAPSED_NS="$process_elapsed"
                ;;
            celox)
                validate_gate_celox_config "$log" "$GATE_TEST" || return "$?"
                classify_celox_result "$log" "$GATE_TEST" "$exit_status" 0 || {
                    echo "error: $CELOX_RESULT_DIAGNOSTIC" >&2
                    return 1
                }
                [[ "$CELOX_SEMANTIC_STATUS" == pass ]] || {
                    echo "error: Celox gate did not report a full pass" >&2
                    return 1
                }
                [[ "$reported_elapsed" == "$CELOX_REPORTED_ELAPSED_NS" ]] || {
                    echo "error: Celox gate row/report elapsed values disagree" >&2
                    return 1
                }
                GATE_CELOX_ELAPSED_NS="$process_elapsed"
                ;;
        esac
    done < <(tail -n +2 "$results_file")

    if ((row_count != 2)) || [[ -z "$GATE_VERYL_ELAPSED_NS" || -z "$GATE_CELOX_ELAPSED_NS" ]]; then
        echo "error: gate must produce exactly the paired veryl-cc and Celox rows" >&2
        return 1
    fi
    if ((GATE_CELOX_ELAPSED_NS > GATE_VERYL_ELAPSED_NS)); then
        echo "error: Heliodor gate failed: Celox ${GATE_CELOX_ELAPSED_NS}ns > Veryl ${GATE_VERYL_ELAPSED_NS}ns" >&2
        return 1
    fi
}

run_gate() {
    local base_results_dir="$CELOX_ROOT/target/heliodor/results"
    local source_checkout invocation_dir runner executable expected_hash
    local before_manifest after_manifest start_probe end_probe timeout_help
    local overall=0

    # The acceptance comparison is deliberately not configurable. Diagnostic
    # experiments remain available through `run` and cannot silently weaken
    # this contract.
    HELIODOR_REPO=https://github.com/dalance/heliodor.git
    HELIODOR_REF="$GATE_HELIODOR_REF"
    HELIODOR_DIR="$CELOX_ROOT/target/heliodor/source"
    HELIODOR_RESULTS_DIR="$base_results_dir"
    HELIODOR_TOOLS_DIR="$CELOX_ROOT/target/heliodor/tools"
    HELIODOR_TESTS="$GATE_TEST"
    HELIODOR_RUNNERS="veryl-cc celox"
    HELIODOR_TIMEOUT_SEC="$GATE_TIMEOUT_SEC"
    HELIODOR_CELOX_TIMEOUT_MULTIPLIER=1
    HELIODOR_CELOX_COMPILE_ONLY=0
    HELIODOR_CELOX_COMPILE_TIMEOUT_SEC=""
    HELIODOR_BUILD_CELOX_RUNNER=1
    HELIODOR_INSTALL_TOOLS=1
    HELIODOR_VERYL_VERSION="$GATE_VERYL_VERSION"
    VERYL_BIN=""
    CELOX_OPT_LEVEL=O2
    CELOX_SIR_PASS_OVERRIDES=""
    HELIODOR_CELOX_CARGO_PROFILE=release
    HELIODOR_CELOX_TARGET_DIR=""
    CELOX_RUNNER_BIN=""

    gate_record_celox_checkout || return "$?"
    if ! command -v timeout >/dev/null; then
        echo "error: the fixed 300s gate requires GNU timeout" >&2
        return 127
    fi
    if ! timeout_help="$(timeout --help 2>&1)" || [[ "$timeout_help" != *--kill-after* ]]; then
        echo "error: the fixed 300s gate requires timeout --kill-after support" >&2
        return 1
    fi
    if ! start_probe="$(monotonic_ns)" || ! end_probe="$(monotonic_ns)" \
        || ! is_uint "$start_probe" || ! is_uint "$end_probe" \
        || ((end_probe < start_probe)); then
        echo "error: a working monotonic nanosecond clock is required by the gate" >&2
        return 1
    fi
    prepare || return "$?"
    source_checkout="$HELIODOR_DIR"
    gate_verify_heliodor_checkout "$source_checkout" || return "$?"

    mkdir -p "$base_results_dir"
    invocation_dir="$(mktemp -d "$base_results_dir/gate_$(date -u +%Y%m%dT%H%M%SZ).XXXXXX")"
    HELIODOR_RESULTS_DIR="$invocation_dir"
    HELIODOR_CELOX_TARGET_DIR="$invocation_dir/celox-target"
    CELOX_RUNNER_BIN="$HELIODOR_CELOX_TARGET_DIR/release/examples/run_veryl_project_test"
    ensure_results_schema "$HELIODOR_RESULTS_DIR/results.tsv" || return "$?"

    build_celox_runner || return "$?"
    gate_verify_celox_checkout || return "$?"
    RESOLVED_VERYL_BIN="$(resolve_gate_veryl_bin)" || return "$?"
    echo "Using gate Veryl: $RESOLVED_VERYL_BIN"

    GATE_WORKTREE_REPO="$source_checkout"
    GATE_WORKTREE_ROOT="$invocation_dir/worktrees"
    if ! gate_create_worktrees; then
        gate_cleanup_worktrees
        return 1
    fi

    for runner in veryl-cc celox; do
        HELIODOR_DIR="$GATE_WORKTREE_ROOT/$runner"
        gate_verify_heliodor_checkout "$HELIODOR_DIR" || overall=1
        before_manifest="$invocation_dir/${runner}.source.before"
        after_manifest="$invocation_dir/${runner}.source.after"
        if ! gate_source_manifest "$GATE_TEST" >"$before_manifest"; then
            overall=1
        fi
        case "$runner" in
            veryl-cc) executable="$RESOLVED_VERYL_BIN" ;;
            celox) executable="$CELOX_RUNNER_BIN" ;;
        esac
        if ! expected_hash="$(gate_file_hash "$executable")"; then
            overall=1
            expected_hash=""
        fi

        run_one "$runner" "$GATE_TEST" || overall=1

        if ! gate_source_manifest "$GATE_TEST" >"$after_manifest" \
            || ! cmp -s "$before_manifest" "$after_manifest"; then
            echo "error: $runner changed its Heliodor source manifest" >&2
            diff -u "$before_manifest" "$after_manifest" >&2 || true
            overall=1
        fi
        gate_verify_heliodor_checkout "$HELIODOR_DIR" || overall=1
        if [[ -n "$expected_hash" ]]; then
            gate_verify_file_hash "$executable" "$expected_hash" "$runner" || overall=1
        fi
    done

    HELIODOR_DIR="$source_checkout"
    gate_cleanup_worktrees
    gate_verify_heliodor_checkout "$source_checkout" || overall=1
    gate_verify_celox_checkout || overall=1
    if ! validate_gate_results "$HELIODOR_RESULTS_DIR/results.tsv" "$invocation_dir"; then
        overall=1
    fi
    if ((overall != 0)); then
        echo "Heliodor gate: FAIL (artifacts: $invocation_dir)" >&2
        return 1
    fi
    echo "Heliodor gate: PASS (Celox ${GATE_CELOX_ELAPSED_NS}ns <= Veryl ${GATE_VERYL_ELAPSED_NS}ns)"
    echo "Artifacts: $invocation_dir"
}

main() {
    local cmd="${1:-run}"
    case "$cmd" in
        prepare) prepare ;;
        list) list_tests ;;
        run) run_all ;;
        gate) run_gate ;;
        -h|--help|help) usage ;;
        *) usage >&2; return 2 ;;
    esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    trap gate_cleanup_worktrees EXIT
    main "$@"
fi
