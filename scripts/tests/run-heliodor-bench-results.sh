#!/bin/bash
# Fixture tests for Heliodor result classification and TSV migration.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../run-heliodor-bench.sh
source "$ROOT/scripts/run-heliodor-bench.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_eq() {
    local actual="$1"
    local expected="$2"
    local context="$3"
    [[ "$actual" == "$expected" ]] \
        || fail "$context: expected <$expected>, got <$actual>"
}

write_log() {
    local path="$1"
    shift
    printf '%s\n' "$@" >"$path"
}

pass_log="$TMP/pass.log"
write_log "$pass_log" \
    'diagnostic before result' \
    'CELOX_TEST_TIMING test=boot compile_ns=4 execute_ns=5 jit_execute_ns=4' \
    'CELOX_TEST_RESULT test=boot status=pass elapsed_ns=11'
classify_celox_result "$pass_log" boot 0 0 \
    || fail "well-formed pass marker was rejected: $CELOX_RESULT_DIAGNOSTIC"
assert_eq "$CELOX_SEMANTIC_STATUS" pass "pass semantic status"
assert_eq "$CELOX_REPORTED_ELAPSED_NS" 11 "pass reported elapsed"
assert_eq "$CELOX_COMPILE_ELAPSED_NS" 4 "pass compile elapsed"
assert_eq "$CELOX_EXECUTE_ELAPSED_NS" 5 "pass execute elapsed"
assert_eq "$CELOX_JIT_EXECUTE_ELAPSED_NS" 4 "pass JIT execute elapsed"

cpu_timed_log="$TMP/cpu-timed.log"
write_log "$cpu_timed_log" \
    'CELOX_TEST_TIMING test=boot_cpu compile_ns=6 execute_ns=7 jit_execute_ns=5 execute_cpu_ns=8' \
    'CELOX_TEST_RESULT test=boot_cpu status=pass elapsed_ns=15'
classify_celox_result "$cpu_timed_log" boot_cpu 0 0 \
    || fail "timing marker with CPU time was rejected: $CELOX_RESULT_DIAGNOSTIC"
assert_eq "$CELOX_COMPILE_ELAPSED_NS" 6 "CPU-timed compile elapsed"
assert_eq "$CELOX_EXECUTE_ELAPSED_NS" 7 "CPU-timed execute elapsed"
assert_eq "$CELOX_JIT_EXECUTE_ELAPSED_NS" 5 "CPU-timed JIT execute elapsed"

timed_veryl_log="$TMP/timed-veryl.log"
write_log "$timed_veryl_log" \
    'VERYL_TEST_TIMING test=boot compile_ns=6 execute_ns=7 execute_cpu_ns=8' \
    'VERYL_TEST_RESULT test=boot status=pass elapsed_ns=15'
classify_timed_veryl_result "$timed_veryl_log" boot 0 \
    || fail "CPU-timed Veryl result was rejected: $VERYL_TIMED_RESULT_DIAGNOSTIC"
assert_eq "$VERYL_TIMED_SEMANTIC_STATUS" pass "timed Veryl semantic status"
assert_eq "$VERYL_TIMED_REPORTED_ELAPSED_NS" 15 "timed Veryl reported elapsed"
assert_eq "$VERYL_TIMED_COMPILE_ELAPSED_NS" 6 "timed Veryl compile elapsed"
assert_eq "$VERYL_TIMED_EXECUTE_ELAPSED_NS" 7 "timed Veryl execute elapsed"

timed_veryl_compile_log="$TMP/timed-veryl-compile.log"
write_log "$timed_veryl_compile_log" \
    'VERYL_TEST_TIMING test=boot_compile compile_ns=9 execute_ns=0' \
    'VERYL_TEST_RESULT test=boot_compile status=compile-only elapsed_ns=10'
classify_timed_veryl_result "$timed_veryl_compile_log" boot_compile 0 1 \
    || fail "timed Veryl compile-only result was rejected: $VERYL_TIMED_RESULT_DIAGNOSTIC"
assert_eq "$VERYL_TIMED_SEMANTIC_STATUS" compile-only \
    "timed Veryl compile-only semantic status"
assert_eq "$VERYL_TIMED_COMPILE_ELAPSED_NS" 9 \
    "timed Veryl compile-only compile elapsed"
assert_eq "$VERYL_TIMED_EXECUTE_ELAPSED_NS" 0 \
    "timed Veryl compile-only execute elapsed"

wrong_timed_veryl_log="$TMP/wrong-timed-veryl.log"
write_log "$wrong_timed_veryl_log" \
    'VERYL_TEST_TIMING test=other compile_ns=6 execute_ns=7' \
    'VERYL_TEST_RESULT test=boot status=pass elapsed_ns=15'
if classify_timed_veryl_result "$wrong_timed_veryl_log" boot 0; then
    fail "timed Veryl result with a mismatched timing test was accepted"
fi
assert_eq "$VERYL_TIMED_SEMANTIC_STATUS" invalid \
    "wrong timed Veryl test semantic status"

missing_timing_log="$TMP/missing-timing.log"
write_log "$missing_timing_log" 'CELOX_TEST_RESULT test=boot status=pass elapsed_ns=11'
if classify_celox_result "$missing_timing_log" boot 0 0; then
    fail "current result without split timing was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "missing timing semantic status"

compile_log="$TMP/compile.log"
write_log "$compile_log" \
    'CELOX_TEST_TIMING test=boot_compile compile_ns=20 execute_ns=0 jit_execute_ns=0' \
    'CELOX_TEST_RESULT test=boot_compile status=compile-only elapsed_ns=22'
classify_celox_result "$compile_log" boot_compile 0 1 \
    || fail "well-formed compile-only marker was rejected: $CELOX_RESULT_DIAGNOSTIC"
assert_eq "$CELOX_SEMANTIC_STATUS" compile-only "compile-only semantic status"
assert_eq "$(full_pass_elapsed_ns "$CELOX_SEMANTIC_STATUS" 0 50)" NA \
    "compile-only must not expose a speed elapsed value"

fail_log="$TMP/fail.log"
write_log "$fail_log" \
    'CELOX_TEST_TIMING test=boot_fail compile_ns=10 execute_ns=20 jit_execute_ns=18' \
    'CELOX_TEST_RESULT test=boot_fail status=fail elapsed_ns=33'
classify_celox_result "$fail_log" boot_fail 1 0 \
    || fail "well-formed fail marker was rejected: $CELOX_RESULT_DIAGNOSTIC"
assert_eq "$CELOX_SEMANTIC_STATUS" fail "fail semantic status"
assert_eq "$(full_pass_elapsed_ns pass 0 123)" 123 "full pass elapsed"
assert_eq "$(full_pass_elapsed_ns pass 1 123)" NA "failed process elapsed"

missing_log="$TMP/missing.log"
write_log "$missing_log" 'process timed out before reporting a result'
if classify_celox_result "$missing_log" boot 124 0; then
    fail "missing result marker was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" unreported "missing marker semantic status"

malformed_log="$TMP/malformed.log"
write_log "$malformed_log" 'CELOX_TEST_RESULT test=boot status=success elapsed_ns=44'
if classify_celox_result "$malformed_log" boot 0 0; then
    fail "malformed result marker was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "malformed marker semantic status"

trailing_log="$TMP/trailing.log"
write_log "$trailing_log" 'CELOX_TEST_RESULT test=boot status=pass elapsed_ns=44 extra=true'
if classify_celox_result "$trailing_log" boot 0 0; then
    fail "result marker with trailing fields was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "trailing marker semantic status"

duplicate_log="$TMP/duplicate.log"
write_log "$duplicate_log" \
    'CELOX_TEST_RESULT test=boot status=pass elapsed_ns=1' \
    'CELOX_TEST_RESULT test=boot status=pass elapsed_ns=2'
if classify_celox_result "$duplicate_log" boot 0 0; then
    fail "duplicate result markers were accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "duplicate marker semantic status"

wrong_test_log="$TMP/wrong-test.log"
write_log "$wrong_test_log" 'CELOX_TEST_RESULT test=other status=pass elapsed_ns=55'
if classify_celox_result "$wrong_test_log" boot 0 0; then
    fail "result marker for another test was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "wrong-test semantic status"

if classify_celox_result "$compile_log" boot_compile 0 0; then
    fail "compile-only marker was accepted for a full run"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "unexpected compile-only semantic status"

if classify_celox_result "$pass_log" boot 1 0; then
    fail "pass marker with a failing process exit was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "pass/exit contradiction status"

if classify_celox_result "$fail_log" boot_fail 0 0; then
    fail "fail marker with process exit 0 was accepted"
fi
assert_eq "$CELOX_SEMANTIC_STATUS" invalid "fail/exit contradiction status"

results="$TMP/results.tsv"
cat >"$results" <<EOF
$RESULTS_HEADER_V1
celox	boot	0	100	$pass_log
celox	boot_compile	0	50	$compile_log
celox	boot_timeout	124	30	$missing_log
celox	boot_zero_unreported	0	40	$missing_log
veryl-cc	boot	0	200	$TMP/veryl-pass.log
veryl-cc	boot_fail	1	25	$TMP/veryl-fail.log
EOF
cp "$results" "$TMP/original-v1.tsv"

ensure_results_schema "$results"
[[ -f "${results}.v1.bak" ]] || fail "v1 migration did not create a backup"
cmp -s "$TMP/original-v1.tsv" "${results}.v1.bak" \
    || fail "v1 migration backup differs from the original"

expected="$TMP/expected-v4.tsv"
cat >"$expected" <<EOF
$RESULTS_HEADER_V4
celox	boot	0	100	$pass_log	pass	0	100	11	4	5	4
celox	boot_compile	0	NA	$compile_log	compile-only	0	50	22	20	0	0
celox	boot_timeout	124	NA	$missing_log	unreported	124	30	NA	NA	NA	NA
celox	boot_zero_unreported	0	NA	$missing_log	unreported	0	40	NA	NA	NA	NA
veryl-cc	boot	0	200	$TMP/veryl-pass.log	pass	0	200	NA	NA	NA	NA
veryl-cc	boot_fail	1	NA	$TMP/veryl-fail.log	fail	1	25	NA	NA	NA	NA
EOF
cmp -s "$expected" "$results" || {
    diff -u "$expected" "$results" >&2 || true
    fail "migrated v4 results differ from expected"
}

cp "$results" "$TMP/before-idempotent.tsv"
ensure_results_schema "$results"
cmp -s "$TMP/before-idempotent.tsv" "$results" \
    || fail "ensuring an existing v4 schema is not idempotent"

v2_results="$TMP/v2-results.tsv"
printf '%s\n%s\n' "$RESULTS_HEADER_V2" \
    $'celox\tboot\t0\t100\t'"$pass_log"$'\tpass\t0\t100\t11' \
    >"$v2_results"
ensure_results_schema "$v2_results"
[[ -f "${v2_results}.v2.bak" ]] || fail "v2 migration did not create a backup"
assert_eq "$(sed -n '1p' "$v2_results")" "$RESULTS_HEADER_V4" "v2 migration header"
assert_eq "$(awk -F '\t' 'NR == 2 { print $10 }' "$v2_results")" 4 \
    "v2 migration recovered compile elapsed"
assert_eq "$(awk -F '\t' 'NR == 2 { print $11 }' "$v2_results")" 5 \
    "v2 migration recovered execute elapsed"
assert_eq "$(awk -F '\t' 'NR == 2 { print $12 }' "$v2_results")" 4 \
    "v2 migration recovered JIT execute elapsed"

v3_results="$TMP/v3-results.tsv"
printf '%s\n%s\n' "$RESULTS_HEADER_V3" \
    $'celox\tboot\t0\t100\t'"$pass_log"$'\tpass\t0\t100\t11\t4\t5' \
    >"$v3_results"
ensure_results_schema "$v3_results"
[[ -f "${v3_results}.v3.bak" ]] || fail "v3 migration did not create a backup"
assert_eq "$(sed -n '1p' "$v3_results")" "$RESULTS_HEADER_V4" "v3 migration header"
assert_eq "$(awk -F '\t' 'NR == 2 { print $12 }' "$v3_results")" 4 \
    "v3 migration recovered JIT execute elapsed"

new_results="$TMP/new-results.tsv"
ensure_results_schema "$new_results"
assert_eq "$(sed -n '1p' "$new_results")" "$RESULTS_HEADER_V4" "new results header"
assert_eq "$(wc -l <"$new_results")" 1 "new results line count"

append_result_row "$new_results" celox boot_compile 0 NA "$compile_log" \
    compile-only 50 22 20 0 0 >/dev/null
assert_eq "$(awk -F '\t' 'NR == 2 { print NF }' "$new_results")" 12 \
    "appended v4 field count"
assert_eq "$(awk -F '\t' 'NR == 2 { print $4 }' "$new_results")" NA \
    "compile-only appended speed elapsed"
before_invalid_append="$(wc -l <"$new_results")"
if append_result_row "$new_results" celox impossible 0 1 "$compile_log" \
    compile-only 1 1 1 0 0 >/dev/null 2>&1; then
    fail "append accepted compile-only with a numeric speed elapsed"
fi
assert_eq "$(wc -l <"$new_results")" "$before_invalid_append" \
    "invalid append changed the results file"

bad_results="$TMP/bad-results.tsv"
printf '%s\n%s\n' "$RESULTS_HEADER_V4" $'celox\tboot\t0\t100\tlog' >"$bad_results"
if ensure_results_schema "$bad_results" 2>/dev/null; then
    fail "v4 header with a legacy-width row was accepted"
fi

bad_semantics="$TMP/bad-semantics.tsv"
printf '%s\n%s\n' "$RESULTS_HEADER_V4" \
    $'celox\tboot\t0\t100\tlog\tcompile-only\t0\t100\t50\t40\t0\t0' >"$bad_semantics"
if ensure_results_schema "$bad_semantics" 2>/dev/null; then
    fail "v4 compile-only row with a numeric speed elapsed was accepted"
fi

# A Veryl baseline should only cap runners intended for direct performance
# comparison. Slower diagnostic backends retain the test fallback timeout.
HELIODOR_COMPILE_ONLY=0
HELIODOR_TIMEOUT_SEC=""
HELIODOR_CELOX_TIMEOUT_MULTIPLIER=2
BASELINE_ELAPSED_NS[test_soc_linux_boot]=1000000000
assert_eq "$(timeout_sec_for celox test_soc_linux_boot)" 2 \
    "native baseline-derived timeout"
assert_eq "$(timeout_sec_for celox-tiered test_soc_linux_boot)" 2 \
    "tiered baseline-derived timeout"
assert_eq "$(timeout_sec_for celox-cranelift test_soc_linux_boot)" 300 \
    "Cranelift fallback timeout"
assert_eq "$(timeout_sec_for celox-interpreter test_soc_linux_boot)" 300 \
    "interpreter fallback timeout"

# Exercise run_one without Heliodor or either compiler. These overrides emit
# fixture logs at the same boundary as the real subprocess wrapper.
integration_results="$TMP/integration-results"
mkdir -p "$integration_results"
HELIODOR_RESULTS_DIR="$integration_results"
CELOX_RUNNER_BIN=/bin/true
VERYL_TIMED_RUNNER_BIN=/bin/true
RESOLVED_VERYL_BIN=/bin/true
CELOX_SIR_PASS_OVERRIDES=""
HELIODOR_COMPILE_TIMEOUT_SEC=""
FIXTURE_RESULT_LINE=""
FIXTURE_EXIT_STATUS=0
FIXTURE_AOT_CACHE_DIR=""
FIXTURE_RUN_ARGS=()

test_source_files() {
    printf '%s\n' dummy.veryl
}

timeout_sec_for() {
    printf '%s\n' 0
}

run_in_heliodor() {
    local _timeout="$1"
    local log="$2"
    shift 2
    FIXTURE_RUN_ARGS=("$@")
    if [[ "${1:-}" == env && "${2:-}" == VERYL_AOT_CACHE_DIR=* ]]; then
        FIXTURE_AOT_CACHE_DIR="${2#VERYL_AOT_CACHE_DIR=}"
        [[ -d "$FIXTURE_AOT_CACHE_DIR" ]] \
            || fail "run_one did not create the isolated Veryl AOT cache"
    fi
    if [[ -n "$FIXTURE_RESULT_LINE" ]]; then
        printf '%s\n' "$FIXTURE_RESULT_LINE" >"$log"
    else
        printf '%s\n' 'fixture process exited without a semantic result' >"$log"
    fi
    return "$FIXTURE_EXIT_STATUS"
}

ensure_results_schema "$integration_results/results.tsv"
HELIODOR_COMPILE_ONLY=0
CELOX_OPT_LEVEL=O2
CELOX_SIR_PASS_OVERRIDES="-branchify_mux +gvn"
FIXTURE_RESULT_LINE=$'CELOX_TEST_TIMING test=integration_pass compile_ns=20 execute_ns=30 jit_execute_ns=25\nCELOX_TEST_RESULT test=integration_pass status=pass elapsed_ns=71'
run_one celox integration_pass >/dev/null \
    || fail "run_one rejected a fixture full pass"
fixture_arg_count="${#FIXTURE_RUN_ARGS[@]}"
assert_eq "${FIXTURE_RUN_ARGS[$((fixture_arg_count - 2))]}" --opt-level \
    "Celox optimization flag"
assert_eq "${FIXTURE_RUN_ARGS[$((fixture_arg_count - 1))]}" o2 \
    "normalized Celox optimization level"
[[ " ${FIXTURE_RUN_ARGS[*]} " == *" --sir-pass=-branchify_mux "* ]] \
    || fail "Celox disabled pass override was not kept as one argument"
[[ " ${FIXTURE_RUN_ARGS[*]} " == *" --sir-pass=+gvn "* ]] \
    || fail "Celox enabled pass override was not kept as one argument"
CELOX_SIR_PASS_OVERRIDES=""
assert_eq "$(awk -F '\t' 'NR == 2 { print $6 }' "$integration_results/results.tsv")" pass \
    "run_one pass semantic status"
[[ "$(awk -F '\t' 'NR == 2 { print $4 }' "$integration_results/results.tsv")" =~ ^[0-9]+$ ]] \
    || fail "run_one full pass did not expose a numeric speed elapsed"
assert_eq "$(awk -F '\t' 'NR == 2 { print $10 }' "$integration_results/results.tsv")" 20 \
    "run_one pass compile elapsed"
assert_eq "$(awk -F '\t' 'NR == 2 { print $11 }' "$integration_results/results.tsv")" 30 \
    "run_one pass execute elapsed"
assert_eq "$(awk -F '\t' 'NR == 2 { print $12 }' "$integration_results/results.tsv")" 25 \
    "run_one pass JIT execute elapsed"

HELIODOR_COMPILE_ONLY=1
FIXTURE_RESULT_LINE=$'CELOX_TEST_TIMING test=integration_compile compile_ns=70 execute_ns=0 jit_execute_ns=0\nCELOX_TEST_RESULT test=integration_compile status=compile-only elapsed_ns=72'
run_one celox integration_compile >/dev/null \
    || fail "run_one rejected a fixture compile-only completion"
assert_eq "$(awk -F '\t' 'NR == 3 { print $6 }' "$integration_results/results.tsv")" \
    compile-only "run_one compile-only semantic status"
assert_eq "$(awk -F '\t' 'NR == 3 { print $4 }' "$integration_results/results.tsv")" NA \
    "run_one compile-only speed elapsed"

HELIODOR_COMPILE_ONLY=0
FIXTURE_EXIT_STATUS=1
FIXTURE_RESULT_LINE=$'CELOX_TEST_TIMING test=integration_fail compile_ns=20 execute_ns=30 jit_execute_ns=25\nCELOX_TEST_RESULT test=integration_fail status=fail elapsed_ns=73'
if run_one celox integration_fail >/dev/null 2>&1; then
    fail "run_one returned success for a semantic test failure"
fi
assert_eq "$(awk -F '\t' 'NR == 4 { print $6 }' "$integration_results/results.tsv")" \
    fail "run_one fail semantic status"
assert_eq "$(awk -F '\t' 'NR == 4 { print $4 }' "$integration_results/results.tsv")" NA \
    "run_one fail speed elapsed"

FIXTURE_EXIT_STATUS=0
FIXTURE_RESULT_LINE=""
if run_one celox integration_missing >/dev/null 2>&1; then
    fail "run_one accepted exit 0 without a semantic result marker"
fi
assert_eq "$(awk -F '\t' 'NR == 5 { print $6 }' "$integration_results/results.tsv")" \
    unreported "run_one missing-result semantic status"
assert_eq "$(awk -F '\t' 'NR == 5 { print $4 }' "$integration_results/results.tsv")" NA \
    "run_one missing-result speed elapsed"

FIXTURE_RESULT_LINE=$'VERYL_TEST_TIMING test=integration_veryl compile_ns=40 execute_ns=50\nVERYL_TEST_RESULT test=integration_veryl status=pass elapsed_ns=91'
# Exercise the real runner's path handling with a relative results directory;
# the cache passed through a Heliodor chdir must still be absolute.
HELIODOR_RESULTS_DIR="$(realpath --relative-to="$PWD" "$integration_results")"
run_one veryl-cc-sync integration_veryl >/dev/null \
    || fail "run_one rejected a fixture timed Veryl pass"
[[ -n "$FIXTURE_AOT_CACHE_DIR" ]] \
    || fail "run_one did not pass an isolated cache to timed Veryl"
[[ "$FIXTURE_AOT_CACHE_DIR" == /* ]] \
    || fail "run_one passed a relative Veryl AOT cache path"
[[ ! -e "$FIXTURE_AOT_CACHE_DIR" ]] \
    || fail "run_one did not remove the isolated Veryl AOT cache"
assert_eq "$(awk -F '\t' 'NR == 6 { print $6 }' "$integration_results/results.tsv")" pass \
    "run_one timed Veryl semantic status"
assert_eq "$(awk -F '\t' 'NR == 6 { print $10 }' "$integration_results/results.tsv")" 40 \
    "run_one timed Veryl compile elapsed"
assert_eq "$(awk -F '\t' 'NR == 6 { print $11 }' "$integration_results/results.tsv")" 50 \
    "run_one timed Veryl execute elapsed"

HELIODOR_COMPILE_ONLY=1
FIXTURE_RESULT_LINE=$'VERYL_TEST_TIMING test=integration_veryl_compile compile_ns=60 execute_ns=0\nVERYL_TEST_RESULT test=integration_veryl_compile status=compile-only elapsed_ns=61'
run_one veryl-cc-sync integration_veryl_compile >/dev/null \
    || fail "run_one rejected a fixture timed Veryl compile-only result"
[[ " ${FIXTURE_RUN_ARGS[*]} " == *" --compile-only "* ]] \
    || fail "run_one did not request timed Veryl compile-only mode"
assert_eq "$(awk -F '\t' 'NR == 7 { print $6 }' "$integration_results/results.tsv")" \
    compile-only "run_one timed Veryl compile-only semantic status"
assert_eq "$(awk -F '\t' 'NR == 7 { print $4 }' "$integration_results/results.tsv")" NA \
    "run_one timed Veryl compile-only speed elapsed"
HELIODOR_COMPILE_ONLY=0

FIXTURE_AOT_CACHE_DIR=""
FIXTURE_RESULT_LINE=$'[INFO ]    Succeeded test (integration_veryl_cli)\n[INFO ]    Completed tests : 1 passed, 0 failed'
run_one veryl-cc integration_veryl_cli >/dev/null \
    || fail "run_one rejected a fixture Veryl CLI pass"
[[ "$FIXTURE_AOT_CACHE_DIR" == /* ]] \
    || fail "run_one did not pass an absolute isolated cache to the Veryl CLI"
[[ ! -e "$FIXTURE_AOT_CACHE_DIR" ]] \
    || fail "run_one did not remove the Veryl CLI AOT cache"
assert_eq "$(awk -F '\t' 'NR == 8 { print $10 }' "$integration_results/results.tsv")" NA \
    "run_one Veryl CLI compile elapsed"
assert_eq "$(awk -F '\t' 'NR == 8 { print $11 }' "$integration_results/results.tsv")" NA \
    "run_one Veryl CLI execute elapsed"

FIXTURE_RESULT_LINE=$'CELOX_TEST_TIMING test=integration_tiered compile_ns=12 execute_ns=34 jit_execute_ns=NA\nCELOX_TIERED_STATS test=integration_tiered tier=compiled promotion=promoted interpreted_evaluations=10 compiled_evaluations=20 promoted_after_interpreted_evaluations=10 promotion_elapsed_ns=15 safe_point_polls=11 split_apply_deferrals=0 threshold_deferrals=0\nCELOX_TEST_RESULT test=integration_tiered status=pass elapsed_ns=48'
run_one celox-tiered integration_tiered >/dev/null \
    || fail "run_one rejected a fixture tiered pass"
[[ " ${FIXTURE_RUN_ARGS[*]} " == *" --backend tiered "* ]] \
    || fail "tiered runner did not select the tiered backend"
assert_eq "$(awk -F '\t' 'NR == 9 { print $1 }' "$integration_results/results.tsv")" \
    celox-tiered "run_one tiered runner name"
assert_eq "$(awk -F '\t' 'NR == 9 { print $12 }' "$integration_results/results.tsv")" NA \
    "run_one tiered generated-only elapsed"

FIXTURE_RESULT_LINE=$'CELOX_TEST_TIMING test=integration_interpreter compile_ns=8 execute_ns=55 jit_execute_ns=NA\nCELOX_TEST_RESULT test=integration_interpreter status=pass elapsed_ns=65'
run_one celox-interpreter integration_interpreter >/dev/null \
    || fail "run_one rejected a fixture interpreter pass"
[[ " ${FIXTURE_RUN_ARGS[*]} " == *" --backend interpreter "* ]] \
    || fail "interpreter runner did not select the interpreter backend"
assert_eq "$(awk -F '\t' 'NR == 10 { print $1 }' "$integration_results/results.tsv")" \
    celox-interpreter "run_one interpreter runner name"

echo "run-heliodor-bench result fixture tests: PASS"
