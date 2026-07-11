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
    'CELOX_TEST_RESULT test=boot status=pass elapsed_ns=11'
classify_celox_result "$pass_log" boot 0 0 \
    || fail "well-formed pass marker was rejected: $CELOX_RESULT_DIAGNOSTIC"
assert_eq "$CELOX_SEMANTIC_STATUS" pass "pass semantic status"
assert_eq "$CELOX_REPORTED_ELAPSED_NS" 11 "pass reported elapsed"

compile_log="$TMP/compile.log"
write_log "$compile_log" 'CELOX_TEST_RESULT test=boot_compile status=compile-only elapsed_ns=22'
classify_celox_result "$compile_log" boot_compile 0 1 \
    || fail "well-formed compile-only marker was rejected: $CELOX_RESULT_DIAGNOSTIC"
assert_eq "$CELOX_SEMANTIC_STATUS" compile-only "compile-only semantic status"
assert_eq "$(full_pass_elapsed_ns "$CELOX_SEMANTIC_STATUS" 0 50)" NA \
    "compile-only must not expose a speed elapsed value"

fail_log="$TMP/fail.log"
write_log "$fail_log" 'CELOX_TEST_RESULT test=boot_fail status=fail elapsed_ns=33'
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

expected="$TMP/expected-v2.tsv"
cat >"$expected" <<EOF
$RESULTS_HEADER_V2
celox	boot	0	100	$pass_log	pass	0	100	11
celox	boot_compile	0	NA	$compile_log	compile-only	0	50	22
celox	boot_timeout	124	NA	$missing_log	unreported	124	30	NA
celox	boot_zero_unreported	0	NA	$missing_log	unreported	0	40	NA
veryl-cc	boot	0	200	$TMP/veryl-pass.log	pass	0	200	NA
veryl-cc	boot_fail	1	NA	$TMP/veryl-fail.log	fail	1	25	NA
EOF
cmp -s "$expected" "$results" || {
    diff -u "$expected" "$results" >&2 || true
    fail "migrated v2 results differ from expected"
}

cp "$results" "$TMP/before-idempotent.tsv"
ensure_results_schema "$results"
cmp -s "$TMP/before-idempotent.tsv" "$results" \
    || fail "ensuring an existing v2 schema is not idempotent"

new_results="$TMP/new-results.tsv"
ensure_results_schema "$new_results"
assert_eq "$(sed -n '1p' "$new_results")" "$RESULTS_HEADER_V2" "new results header"
assert_eq "$(wc -l <"$new_results")" 1 "new results line count"

append_result_row "$new_results" celox boot_compile 0 NA "$compile_log" \
    compile-only 50 22 >/dev/null
assert_eq "$(awk -F '\t' 'NR == 2 { print NF }' "$new_results")" 9 \
    "appended v2 field count"
assert_eq "$(awk -F '\t' 'NR == 2 { print $4 }' "$new_results")" NA \
    "compile-only appended speed elapsed"
before_invalid_append="$(wc -l <"$new_results")"
if append_result_row "$new_results" celox impossible 0 1 "$compile_log" \
    compile-only 1 1 >/dev/null 2>&1; then
    fail "append accepted compile-only with a numeric speed elapsed"
fi
assert_eq "$(wc -l <"$new_results")" "$before_invalid_append" \
    "invalid append changed the results file"

bad_results="$TMP/bad-results.tsv"
printf '%s\n%s\n' "$RESULTS_HEADER_V2" $'celox\tboot\t0\t100\tlog' >"$bad_results"
if ensure_results_schema "$bad_results" 2>/dev/null; then
    fail "v2 header with a legacy-width row was accepted"
fi

bad_semantics="$TMP/bad-semantics.tsv"
printf '%s\n%s\n' "$RESULTS_HEADER_V2" \
    $'celox\tboot\t0\t100\tlog\tcompile-only\t0\t100\t50' >"$bad_semantics"
if ensure_results_schema "$bad_semantics" 2>/dev/null; then
    fail "v2 compile-only row with a numeric speed elapsed was accepted"
fi

# Exercise run_one without Heliodor or either compiler. These overrides emit
# fixture logs at the same boundary as the real subprocess wrapper.
integration_results="$TMP/integration-results"
mkdir -p "$integration_results"
HELIODOR_RESULTS_DIR="$integration_results"
CELOX_RUNNER_BIN=/bin/true
CELOX_SIR_PASS_OVERRIDES=""
HELIODOR_CELOX_COMPILE_TIMEOUT_SEC=""
FIXTURE_RESULT_LINE=""
FIXTURE_EXIT_STATUS=0

test_source_files() {
    printf '%s\n' dummy.veryl
}

timeout_sec_for() {
    printf '%s\n' 0
}

run_in_heliodor() {
    local _timeout="$1"
    local log="$2"
    if [[ -n "$FIXTURE_RESULT_LINE" ]]; then
        printf '%s\n' "$FIXTURE_RESULT_LINE" >"$log"
    else
        printf '%s\n' 'fixture process exited without a semantic result' >"$log"
    fi
    return "$FIXTURE_EXIT_STATUS"
}

ensure_results_schema "$integration_results/results.tsv"
HELIODOR_CELOX_COMPILE_ONLY=0
FIXTURE_RESULT_LINE='CELOX_TEST_RESULT test=integration_pass status=pass elapsed_ns=71'
run_one celox integration_pass >/dev/null \
    || fail "run_one rejected a fixture full pass"
assert_eq "$(awk -F '\t' 'NR == 2 { print $6 }' "$integration_results/results.tsv")" pass \
    "run_one pass semantic status"
[[ "$(awk -F '\t' 'NR == 2 { print $4 }' "$integration_results/results.tsv")" =~ ^[0-9]+$ ]] \
    || fail "run_one full pass did not expose a numeric speed elapsed"

HELIODOR_CELOX_COMPILE_ONLY=1
FIXTURE_RESULT_LINE='CELOX_TEST_RESULT test=integration_compile status=compile-only elapsed_ns=72'
run_one celox integration_compile >/dev/null \
    || fail "run_one rejected a fixture compile-only completion"
assert_eq "$(awk -F '\t' 'NR == 3 { print $6 }' "$integration_results/results.tsv")" \
    compile-only "run_one compile-only semantic status"
assert_eq "$(awk -F '\t' 'NR == 3 { print $4 }' "$integration_results/results.tsv")" NA \
    "run_one compile-only speed elapsed"

HELIODOR_CELOX_COMPILE_ONLY=0
FIXTURE_EXIT_STATUS=1
FIXTURE_RESULT_LINE='CELOX_TEST_RESULT test=integration_fail status=fail elapsed_ns=73'
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

echo "run-heliodor-bench result fixture tests: PASS"
