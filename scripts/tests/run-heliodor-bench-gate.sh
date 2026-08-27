#!/bin/bash
# Fixture tests for the fixed Heliodor acceptance gate. No compiler, checkout,
# install, build, or Heliodor process is executed.
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

write_valid_gate_logs() {
    local directory="$1"
    local veryl_compile="${2:-30}"
    local veryl_execute="${3:-40}"
    local celox_compile="${4:-10}"
    local celox_execute="${5:-20}"
    local celox_jit_execute="${6:-$celox_execute}"
    local tiered_startup="${7:-5}"
    local tiered_execute="${8:-18}"
    local veryl_reported="$((veryl_compile + veryl_execute + 1))"
    local celox_reported="$((celox_compile + celox_execute + 1))"
    local tiered_reported="$((tiered_startup + tiered_execute + 1))"
    mkdir -p "$directory"
    printf '%s\n%s\n%s\n%s\n' \
        "v4 SoC linux boot smoke: cy=00d83790 x3=00000000000000aa pass=1" \
        "VERYL_TEST_CONFIG test=$GATE_TEST backend=cc aot_c_async=false compile_only=false" \
        "VERYL_TEST_TIMING test=$GATE_TEST compile_ns=$veryl_compile execute_ns=$veryl_execute" \
        "VERYL_TEST_RESULT test=$GATE_TEST status=pass elapsed_ns=$veryl_reported" \
        >"$directory/veryl.log"
    printf '%s\n%s\n%s\n%s\n' \
        "v4 SoC linux boot smoke: cy=d83790 x3=aa pass=1" \
        "CELOX_TEST_CONFIG test=$GATE_TEST backend=native opt_level=O2 four_state=false compile_only=false" \
        "CELOX_TEST_TIMING test=$GATE_TEST compile_ns=$celox_compile execute_ns=$celox_execute jit_execute_ns=$celox_jit_execute" \
        "CELOX_TEST_RESULT test=$GATE_TEST status=pass elapsed_ns=$celox_reported" \
        >"$directory/celox.log"
    printf '%s\n%s\n%s\n%s\n%s\n' \
        "v4 SoC linux boot smoke: cy=d83790 x3=aa pass=1" \
        "CELOX_TEST_CONFIG test=$GATE_TEST backend=tiered opt_level=O2 four_state=false compile_only=false" \
        "CELOX_TEST_TIMING test=$GATE_TEST compile_ns=$tiered_startup execute_ns=$tiered_execute jit_execute_ns=NA" \
        "CELOX_TIERED_STATS test=$GATE_TEST tier=compiled promotion=promoted interpreted_evaluations=100 compiled_evaluations=200 promoted_after_interpreted_evaluations=100 promotion_elapsed_ns=12 safe_point_polls=101 split_apply_deferrals=0 threshold_deferrals=0" \
        "CELOX_TEST_RESULT test=$GATE_TEST status=pass elapsed_ns=$tiered_reported" \
        >"$directory/celox-tiered.log"
}

write_gate_results() {
    local directory="$1"
    local veryl_elapsed="$2"
    local celox_elapsed="$3"
    local veryl_compile="${4:-30}"
    local veryl_execute="${5:-40}"
    local celox_compile="${6:-10}"
    local celox_execute="${7:-20}"
    local celox_jit_execute="${8:-$celox_execute}"
    local tiered_startup="${9:-5}"
    local tiered_execute="${10:-18}"
    local veryl_reported="$((veryl_compile + veryl_execute + 1))"
    local celox_reported="$((celox_compile + celox_execute + 1))"
    local tiered_reported="$((tiered_startup + tiered_execute + 1))"
    write_valid_gate_logs "$directory" "$veryl_compile" "$veryl_execute" \
        "$celox_compile" "$celox_execute" "$celox_jit_execute"
    printf '%s\n' "$RESULTS_HEADER_V4" >"$directory/results.tsv"
    append_result_row "$directory/results.tsv" veryl-cc-sync "$GATE_TEST" 0 \
        "$veryl_elapsed" "$directory/veryl.log" pass "$veryl_elapsed" \
        "$veryl_reported" "$veryl_compile" "$veryl_execute" NA >/dev/null
    append_result_row "$directory/results.tsv" celox "$GATE_TEST" 0 \
        "$celox_elapsed" "$directory/celox.log" pass "$celox_elapsed" \
        "$celox_reported" "$celox_compile" "$celox_execute" "$celox_jit_execute" >/dev/null
    append_result_row "$directory/results.tsv" celox-tiered "$GATE_TEST" 0 \
        90 "$directory/celox-tiered.log" pass 90 \
        "$tiered_reported" "$tiered_startup" "$tiered_execute" NA >/dev/null
}

unit="$TMP/unit"
write_gate_results "$unit" 200 100
validate_gate_results "$unit/results.tsv" "$unit" \
    || fail "valid gate result set was rejected"
assert_eq "$GATE_VERYL_COMPILE_NS" 30 "Veryl gate compile interval"
assert_eq "$GATE_VERYL_EXECUTE_NS" 40 "Veryl gate execute interval"
assert_eq "$GATE_CELOX_COMPILE_NS" 10 "Celox gate compile interval"
assert_eq "$GATE_CELOX_EXECUTE_NS" 20 "Celox gate execute interval"
assert_eq "$GATE_CELOX_JIT_EXECUTE_NS" 20 "Celox gate JIT execute interval"
assert_eq "$GATE_TIERED_STARTUP_NS" 5 "tiered gate startup interval"
assert_eq "$GATE_TIERED_EXECUTE_NS" 18 "tiered gate execute interval"
assert_eq "$GATE_TIERED_TOTAL_NS" 24 "tiered gate end-to-end interval"

trailing_empty="$TMP/trailing-empty"
write_gate_results "$trailing_empty" 200 100
sed -i $'2s/$/\t/' "$trailing_empty/results.tsv"
if validate_gate_results "$trailing_empty/results.tsv" "$trailing_empty" 2>/dev/null; then
    fail "gate accepted an empty trailing TSV field"
fi

leading_empty="$TMP/leading-empty"
write_gate_results "$leading_empty" 200 100
sed -i $'2s/^/\t/' "$leading_empty/results.tsv"
if validate_gate_results "$leading_empty/results.tsv" "$leading_empty" 2>/dev/null; then
    fail "gate accepted an empty leading TSV field"
fi

if validate_gate_results "$unit/results.tsv" "$TMP/not-this-invocation" 2>/dev/null; then
    fail "gate accepted logs from a different invocation"
fi

traversal="$TMP/traversal"
outside="$TMP/outside"
mkdir -p "$traversal" "$outside"
write_valid_gate_logs "$outside"
printf '%s\n' "$RESULTS_HEADER_V4" >"$traversal/results.tsv"
append_result_row "$traversal/results.tsv" veryl-cc-sync "$GATE_TEST" 0 200 \
    "$traversal/../outside/veryl.log" pass 200 71 30 40 NA >/dev/null
append_result_row "$traversal/results.tsv" celox "$GATE_TEST" 0 100 \
    "$traversal/../outside/celox.log" pass 100 31 10 20 20 >/dev/null
if validate_gate_results "$traversal/results.tsv" "$traversal" 2>/dev/null; then
    fail "gate accepted path traversal to logs outside the invocation"
fi

slower_jit="$TMP/slower-jit"
write_gate_results "$slower_jit" 200 100 30 40 10 45 41
validate_gate_results "$slower_jit/results.tsv" "$slower_jit" \
    || fail "gate treated slower Celox generated-JIT execution as a correctness failure"

slower_harness="$TMP/slower-harness"
write_gate_results "$slower_harness" 200 100 30 40 10 45 35
validate_gate_results "$slower_harness/results.tsv" "$slower_harness" \
    || fail "gate used testbench overhead instead of generated-JIT execution"

slower_compile="$TMP/slower-compile"
write_gate_results "$slower_compile" 200 100 30 40 50 20
validate_gate_results "$slower_compile/results.tsv" "$slower_compile" \
    || fail "gate conflated slower Celox code generation with execution throughput"

zero_execute="$TMP/zero-execute"
write_gate_results "$zero_execute" 200 100 30 40 10 0
if validate_gate_results "$zero_execute/results.tsv" "$zero_execute" 2>/dev/null; then
    fail "gate accepted a zero-length full-test execution interval"
fi

reversed="$TMP/reversed"
write_valid_gate_logs "$reversed"
printf '%s\n' "$RESULTS_HEADER_V4" >"$reversed/results.tsv"
append_result_row "$reversed/results.tsv" celox "$GATE_TEST" 0 100 \
    "$reversed/celox.log" pass 100 31 10 20 20 >/dev/null
append_result_row "$reversed/results.tsv" veryl-cc-sync "$GATE_TEST" 0 200 \
    "$reversed/veryl.log" pass 200 71 30 40 NA >/dev/null
if validate_gate_results "$reversed/results.tsv" "$reversed" 2>/dev/null; then
    fail "gate accepted reversed runner order"
fi

extra="$TMP/extra"
write_gate_results "$extra" 200 100
append_result_row "$extra/results.tsv" celox "$GATE_TEST" 0 100 \
    "$extra/celox.log" pass 100 31 10 20 20 >/dev/null
if validate_gate_results "$extra/results.tsv" "$extra" 2>/dev/null; then
    fail "gate accepted an extra result row"
fi

bad_config="$TMP/bad-config"
write_gate_results "$bad_config" 200 100
sed -i 's/backend=native/backend=cranelift/' "$bad_config/celox.log"
if validate_gate_results "$bad_config/results.tsv" "$bad_config" 2>/dev/null; then
    fail "gate accepted a non-native Celox config"
fi

unpromoted_tiered="$TMP/unpromoted-tiered"
write_gate_results "$unpromoted_tiered" 200 100
sed -i \
    's/tier=compiled promotion=promoted/tier=interpreter promotion=pending/' \
    "$unpromoted_tiered/celox-tiered.log"
if validate_gate_results "$unpromoted_tiered/results.tsv" "$unpromoted_tiered" 2>/dev/null; then
    fail "gate accepted a tiered run that never promoted"
fi

duplicate_config="$TMP/duplicate-config"
write_gate_results "$duplicate_config" 200 100
sed -n '2p' "$duplicate_config/celox.log" >>"$duplicate_config/celox.log"
if validate_gate_results "$duplicate_config/results.tsv" "$duplicate_config" 2>/dev/null; then
    fail "gate accepted duplicate Celox config records"
fi

bad_veryl_config="$TMP/bad-veryl-config"
write_gate_results "$bad_veryl_config" 200 100
sed -i 's/aot_c_async=false/aot_c_async=true/' "$bad_veryl_config/veryl.log"
if validate_gate_results "$bad_veryl_config/results.tsv" "$bad_veryl_config" 2>/dev/null; then
    fail "gate accepted asynchronous timed Veryl configuration"
fi

duplicate_veryl_config="$TMP/duplicate-veryl-config"
write_gate_results "$duplicate_veryl_config" 200 100
sed -n '2p' "$duplicate_veryl_config/veryl.log" >>"$duplicate_veryl_config/veryl.log"
if validate_gate_results "$duplicate_veryl_config/results.tsv" "$duplicate_veryl_config" 2>/dev/null; then
    fail "gate accepted duplicate timed Veryl config records"
fi

compile_only="$TMP/compile-only"
write_valid_gate_logs "$compile_only"
printf '%s\n%s\n%s\n' \
    "CELOX_TEST_CONFIG test=$GATE_TEST backend=native opt_level=O2 four_state=false compile_only=true" \
    "CELOX_TEST_TIMING test=$GATE_TEST compile_ns=30 execute_ns=0 jit_execute_ns=0" \
    "CELOX_TEST_RESULT test=$GATE_TEST status=compile-only elapsed_ns=31" \
    >"$compile_only/celox.log"
printf '%s\n' "$RESULTS_HEADER_V4" >"$compile_only/results.tsv"
append_result_row "$compile_only/results.tsv" veryl-cc-sync "$GATE_TEST" 0 200 \
    "$compile_only/veryl.log" pass 200 71 30 40 NA >/dev/null
append_result_row "$compile_only/results.tsv" celox "$GATE_TEST" 0 NA \
    "$compile_only/celox.log" compile-only 100 31 30 0 0 >/dev/null
if validate_gate_results "$compile_only/results.tsv" "$compile_only" 2>/dev/null; then
    fail "gate accepted compile-only Celox"
fi

bad_veryl="$TMP/bad-veryl"
write_gate_results "$bad_veryl" 200 100
sed -i 's/status=pass/status=fail/' "$bad_veryl/veryl.log"
if validate_gate_results "$bad_veryl/results.tsv" "$bad_veryl" 2>/dev/null; then
    fail "gate accepted a contradictory timed Veryl result"
fi

duplicate_veryl="$TMP/duplicate-veryl"
write_gate_results "$duplicate_veryl" 200 100
sed -n '4p' "$duplicate_veryl/veryl.log" >>"$duplicate_veryl/veryl.log"
if validate_gate_results "$duplicate_veryl/results.tsv" "$duplicate_veryl" 2>/dev/null; then
    fail "gate accepted duplicate timed Veryl result records"
fi

bad_veryl_cycle="$TMP/bad-veryl-cycle"
write_gate_results "$bad_veryl_cycle" 200 100
sed -i 's/cy=00d83790/cy=00d83800/' "$bad_veryl_cycle/veryl.log"
if validate_gate_results "$bad_veryl_cycle/results.tsv" "$bad_veryl_cycle" 2>/dev/null; then
    fail "gate accepted the wrong Veryl architectural completion cycle"
fi

bad_celox_cycle="$TMP/bad-celox-cycle"
write_gate_results "$bad_celox_cycle" 200 100
sed -i 's/cy=d83790/cy=d83800/' "$bad_celox_cycle/celox.log"
if validate_gate_results "$bad_celox_cycle/results.tsv" "$bad_celox_cycle" 2>/dev/null; then
    fail "gate accepted the wrong Celox architectural completion cycle"
fi

duplicate_celox_cycle="$TMP/duplicate-celox-cycle"
write_gate_results "$duplicate_celox_cycle" 200 100
sed -n '1p' "$duplicate_celox_cycle/celox.log" >>"$duplicate_celox_cycle/celox.log"
if validate_gate_results "$duplicate_celox_cycle/results.tsv" "$duplicate_celox_cycle" 2>/dev/null; then
    fail "gate accepted duplicate Celox architectural completion records"
fi

reported_mismatch="$TMP/reported-mismatch"
write_gate_results "$reported_mismatch" 200 100
sed -i $'s/\t31\t10\t20\t20$/\t32\t10\t20\t20/' "$reported_mismatch/results.tsv"
if validate_gate_results "$reported_mismatch/results.tsv" "$reported_mismatch" 2>/dev/null; then
    fail "gate accepted a Celox row/log reported-time mismatch"
fi

# Prove that the real build wrapper passes an explicit target directory after
# an externally supplied CARGO_TARGET_DIR. The cargo executable is a fixture
# that only records argv.
mock_cargo_dir="$TMP/mock-cargo"
mkdir -p "$mock_cargo_dir" "$TMP/mock-build-results"
printf '%s\n' \
    '#!/bin/sh' \
    'for arg in "$@"; do' \
    '    printf "%s\n" "$arg"' \
    'done >"$CARGO_ARGS_LOG"' \
    'printf "%s\n%s\n" "${CARGO_TARGET_DIR-unset}" "${CARGO_BUILD_TARGET-unset}" >"$CARGO_ENV_LOG"' \
    'mkdir -p "$(dirname "$MOCK_CARGO_RUNNER")"' \
    'printf "%s\n" "#!/bin/sh" >"$MOCK_CARGO_RUNNER"' \
    'chmod +x "$MOCK_CARGO_RUNNER"' \
    >"$mock_cargo_dir/cargo"
chmod +x "$mock_cargo_dir/cargo"
(
    export PATH="$mock_cargo_dir:$PATH"
    export CARGO_ARGS_LOG="$TMP/cargo-args"
    export CARGO_ENV_LOG="$TMP/cargo-env"
    export MOCK_CARGO_RUNNER="$TMP/fixed-gate-target/release/celox-heliodor"
    export CARGO_TARGET_DIR="$TMP/hostile-cargo-target"
    export CARGO_BUILD_TARGET=hostile-target-triple
    HELIODOR_RESULTS_DIR="$TMP/mock-build-results"
    HELIODOR_BUILD_CELOX_RUNNER=1
    export HELIODOR_CELOX_CARGO_FEATURES=
    HELIODOR_CELOX_TARGET_DIR="$TMP/fixed-gate-target"
    HELIODOR_CELOX_CARGO_PROFILE=release
    CELOX_RUNNER_BIN="$TMP/fixed-gate-target/release/celox-heliodor"
    build_celox_runner >/dev/null
)
target_arg_line="$(grep -n -x -- '--target-dir' "$TMP/cargo-args" | cut -d: -f1)"
[[ -n "$target_arg_line" ]] || fail "Celox build omitted --target-dir"
assert_eq "$(sed -n "$((target_arg_line + 1))p" "$TMP/cargo-args")" \
    "$TMP/fixed-gate-target" "explicit Cargo target directory argument"
grep -qx -- '--locked' "$TMP/cargo-args" || fail "Celox build omitted --locked"
if grep -qx -- '--features' "$TMP/cargo-args"; then
    fail "default Celox build unexpectedly passed --features"
fi
grep -qx -- 'celox-bench' "$TMP/cargo-args" || fail "Celox build did not select celox-bench"
grep -qx -- 'celox-heliodor' "$TMP/cargo-args" || fail "Celox build did not select celox-heliodor"
assert_eq "$(sed -n '1p' "$TMP/cargo-env")" unset "CARGO_TARGET_DIR neutralization"
assert_eq "$(sed -n '2p' "$TMP/cargo-env")" unset "CARGO_BUILD_TARGET neutralization"

# A source enumeration error must stop before any runner subprocess is started;
# process substitution must not turn it into an empty successful list.
(
    HELIODOR_RESULTS_DIR="$TMP/source-enumeration-results"
    CELOX_RUNNER_BIN=/bin/true
    HELIODOR_COMPILE_ONLY=0
    mkdir -p "$HELIODOR_RESULTS_DIR"
    ensure_results_schema "$HELIODOR_RESULTS_DIR/results.tsv"
    test_source_files() {
        return 1
    }
    run_in_heliodor() {
        : >"$TMP/source-enumeration-subprocess-ran"
        return 0
    }
    if run_one celox "$GATE_TEST" >/dev/null 2>&1; then
        fail "run_one accepted a source enumeration failure"
    fi
    [[ ! -e "$TMP/source-enumeration-subprocess-ran" ]] \
        || fail "run_one started a subprocess after source enumeration failed"
    if gate_source_manifest "$GATE_TEST" >/dev/null 2>&1; then
        fail "gate manifest accepted a source enumeration failure"
    fi
)

(
    HELIODOR_DIR="$TMP/intermediate-enumeration-failure"
    mkdir -p "$HELIODOR_DIR/tb"
    printf '%s\n' '#[test(test_soc_linux_boot)]' \
        >"$HELIODOR_DIR/tb/test.veryl"
    if test_source_files "$GATE_TEST" >/dev/null 2>&1; then
        fail "test_source_files hid a missing src directory"
    fi
    mkdir -p "$HELIODOR_DIR/src"
    printf '%s\n' 'module dummy {}' >"$HELIODOR_DIR/src/dummy.veryl"
    mkdir -p "$TMP/failing-find"
    printf '%s\n' '#!/bin/sh' 'exit 7' >"$TMP/failing-find/find"
    chmod +x "$TMP/failing-find/find"
    saved_path="$PATH"
    PATH="$TMP/failing-find:$PATH"
    if test_source_files "$GATE_TEST" >/dev/null 2>&1; then
        fail "test_source_files hid a find pipeline failure"
    fi
    PATH="$saved_path"
    mkdir -p "$TMP/failing-rg"
    printf '%s\n' '#!/bin/sh' 'exit 127' >"$TMP/failing-rg/rg"
    chmod +x "$TMP/failing-rg/rg"
    PATH="$TMP/failing-rg:$PATH"
    assert_eq "$(test_source_files "$GATE_TEST")" \
        $'src/dummy.veryl\ntb/test.veryl' \
        "source enumeration without ripgrep"
    PATH="$saved_path"
    mkdir -p "$HELIODOR_DIR/tb/duplicate"
    printf '%s\n' '#[test(test_soc_linux_boot)]' \
        >"$HELIODOR_DIR/tb/duplicate/test.veryl"
    if test_source_files "$GATE_TEST" >/dev/null 2>&1; then
        fail "test_source_files accepted duplicate test definitions"
    fi
)

# Exercise the real checkout/head and detached-worktree cleanup boundaries with
# a local Git fixture.
git_fixture="$TMP/git-fixture"
git init -q "$git_fixture"
git -C "$git_fixture" config user.name fixture
git -C "$git_fixture" config user.email fixture@example.invalid
printf '%s\n' fixture >"$git_fixture/tracked.txt"
git -C "$git_fixture" add tracked.txt
git -C "$git_fixture" commit -q -m initial
gate_require_clean_checkout "$git_fixture" fixture \
    || fail "clean Git fixture was rejected"
printf '%s\n' untracked >"$git_fixture/untracked.txt"
if gate_require_clean_checkout "$git_fixture" fixture >/dev/null 2>&1; then
    fail "dirty Git fixture was accepted"
fi
rm "$git_fixture/untracked.txt"
cp "$git_fixture/.git/index" "$TMP/git-index"
printf '%s\n' corrupt-index >"$git_fixture/.git/index"
if gate_require_clean_checkout "$git_fixture" fixture >/dev/null 2>&1; then
    fail "checkout status inspection failure was accepted as clean"
fi
mv "$TMP/git-index" "$git_fixture/.git/index"
(
    CELOX_ROOT="$git_fixture"
    gate_record_celox_checkout || fail "could not record clean fixture HEAD"
    printf '%s\n' second >>"$git_fixture/tracked.txt"
    git -C "$git_fixture" add tracked.txt
    git -C "$git_fixture" commit -q -m second
    if gate_verify_celox_checkout >/dev/null 2>&1; then
        fail "Celox HEAD mutation was accepted"
    fi
)
GATE_WORKTREE_REPO="$git_fixture"
GATE_WORKTREE_ROOT="$TMP/git-worktrees"
mkdir -p "$GATE_WORKTREE_ROOT"
git -C "$git_fixture" worktree add -q --detach "$GATE_WORKTREE_ROOT/veryl-cc-sync" HEAD
git -C "$git_fixture" worktree add -q --detach "$GATE_WORKTREE_ROOT/celox" HEAD
git -C "$git_fixture" worktree add -q --detach "$GATE_WORKTREE_ROOT/celox-tiered" HEAD
gate_cleanup_worktrees
[[ ! -e "$TMP/git-worktrees/veryl-cc-sync" && ! -e "$TMP/git-worktrees/celox" \
    && ! -e "$TMP/git-worktrees/celox-tiered" ]] \
    || fail "gate cleanup left detached worktrees behind"

# Full run_gate fixture. Replace every external boundary while retaining the
# fixed configuration, runner order, manifest/hash checks, and result
# validation in run_gate itself.
REAL_CELOX_ROOT="$CELOX_ROOT"
CELOX_ROOT="$TMP/fake-celox"
HELIODOR_DIR="$TMP/fake-heliodor"
HELIODOR_RESULTS_DIR="$TMP/gate-results"
HELIODOR_TOOLS_DIR="$TMP/fake-tools"
mkdir -p "$CELOX_ROOT/target/release" "$HELIODOR_DIR" "$HELIODOR_TOOLS_DIR"

MOCK_RUNNERS=""
MOCK_CELOX_ELAPSED=100
MOCK_CELOX_COMPILE=10
MOCK_CELOX_EXECUTE=20
MOCK_CELOX_STATUS=pass
MOCK_MUTATE_SOURCE=0
MOCK_MUTATE_RUNNER=0
LAST_GATE_RESULTS_ROOT=""

gate_record_celox_checkout() {
    GATE_CELOX_HEAD=0123456789012345678901234567890123456789
}

gate_verify_celox_checkout() {
    return 0
}

prepare() {
    mkdir -p "$HELIODOR_DIR" "$HELIODOR_RESULTS_DIR" "$HELIODOR_TOOLS_DIR"
}

gate_verify_heliodor_checkout() {
    [[ -d "$1" ]]
}

build_celox_runner() {
    assert_eq "$HELIODOR_CELOX_CARGO_FEATURES" "" \
        "fixed gate Cargo features"
    assert_eq "$HELIODOR_CELOX_TARGET_DIR" \
        "$HELIODOR_RESULTS_DIR/celox-target" \
        "fresh invocation-owned gate Cargo target directory"
    assert_eq "$CELOX_RUNNER_BIN" \
        "$HELIODOR_CELOX_TARGET_DIR/release/celox-heliodor" \
        "gate executes the just-built target-dir artifact"
    [[ ! -e "$HELIODOR_CELOX_TARGET_DIR" ]] \
        || fail "gate Cargo target directory was not fresh"
    [[ "$CELOX_RUNNER_BIN" != "$CELOX_ROOT/target/release/celox-heliodor" ]] \
        || fail "gate selected a stale default-target runner"
    mkdir -p "$(dirname "$CELOX_RUNNER_BIN")"
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$CELOX_RUNNER_BIN"
    chmod +x "$CELOX_RUNNER_BIN"
}

build_timed_veryl_runner() {
    assert_eq "$VERYL_TIMED_RUNNER_BIN" \
        "$HELIODOR_CELOX_TARGET_DIR/release/veryl-heliodor" \
        "gate executes the just-built timed Veryl target-dir artifact"
    mkdir -p "$(dirname "$VERYL_TIMED_RUNNER_BIN")"
    printf '%s\n' '#!/bin/sh' 'exit 0' >"$VERYL_TIMED_RUNNER_BIN"
    chmod +x "$VERYL_TIMED_RUNNER_BIN"
}

monotonic_ns() {
    printf '%s\n' 10
}

gate_create_worktrees() {
    mkdir -p "$GATE_WORKTREE_ROOT/veryl-cc-sync" "$GATE_WORKTREE_ROOT/celox" \
        "$GATE_WORKTREE_ROOT/celox-tiered"
}

gate_cleanup_worktrees() {
    if [[ -n "$GATE_WORKTREE_ROOT" ]]; then
        rm -rf "$GATE_WORKTREE_ROOT"
    fi
    GATE_WORKTREE_REPO=""
    GATE_WORKTREE_ROOT=""
}

gate_source_manifest() {
    printf '%s\n' 'Veryl.toml fixture-object'
    if [[ "$MOCK_MUTATE_SOURCE" == 1 && -e "$HELIODOR_DIR/.fixture-mutated" ]]; then
        printf '%s\n' 'src/top.veryl mutated-object'
    else
        printf '%s\n' 'src/top.veryl fixture-object'
    fi
}

run_one() {
    local runner="$1"
    local test="$2"
    local log="$HELIODOR_RESULTS_DIR/$runner.log"
    local process_status=0 semantic_status=pass elapsed reported=NA
    local compile_elapsed=NA execute_elapsed=NA jit_execute_elapsed=NA

    assert_eq "$test" "$GATE_TEST" "fixed gate test"
    assert_eq "$HELIODOR_TESTS" "$GATE_TEST" "fixed test list"
    assert_eq "$HELIODOR_RUNNERS" "veryl-cc-sync celox celox-tiered" "fixed runner list"
    assert_eq "$HELIODOR_REPO" "https://github.com/dalance/heliodor.git" \
        "fixed Heliodor repository"
    assert_eq "$HELIODOR_REF" "$GATE_HELIODOR_REF" "pinned Heliodor commit"
    assert_eq "$HELIODOR_TOOLS_DIR" "$CELOX_ROOT/target/heliodor/tools" \
        "benchmark-owned tools directory"
    assert_eq "$GATE_TIMEOUT_SEC" 420 "fixed gate timeout constant"
    assert_eq "$HELIODOR_TIMEOUT_SEC" "$GATE_TIMEOUT_SEC" "fixed timeout"
    assert_eq "$HELIODOR_COMPILE_ONLY" 0 "full compiler execution"
    assert_eq "$CELOX_OPT_LEVEL" O2 "fixed Celox optimization"
    assert_eq "$CELOX_SIR_PASS_OVERRIDES" "" "no pass overrides"
    [[ "$HELIODOR_DIR" == "$HELIODOR_RESULTS_DIR/worktrees/$runner" ]] \
        || fail "$runner did not use its isolated Heliodor worktree: $HELIODOR_DIR"
    case "$runner" in
        veryl-cc-sync)
            elapsed=200
            reported=71
            compile_elapsed=30
            execute_elapsed=40
            printf '%s\n%s\n%s\n%s\n' \
                'v4 SoC linux boot smoke: cy=00d83790 x3=00000000000000aa pass=1' \
                "VERYL_TEST_CONFIG test=$GATE_TEST backend=cc aot_c_async=false compile_only=false" \
                "VERYL_TEST_TIMING test=$GATE_TEST compile_ns=$compile_elapsed execute_ns=$execute_elapsed" \
                "VERYL_TEST_RESULT test=$GATE_TEST status=pass elapsed_ns=$reported" >"$log"
            ;;
        celox)
            elapsed="$MOCK_CELOX_ELAPSED"
            compile_elapsed="$MOCK_CELOX_COMPILE"
            execute_elapsed="$MOCK_CELOX_EXECUTE"
            jit_execute_elapsed="$MOCK_CELOX_EXECUTE"
            reported="$((compile_elapsed + execute_elapsed + 1))"
            if [[ "$MOCK_CELOX_STATUS" == fail ]]; then
                process_status=1
                semantic_status=fail
                elapsed=NA
            fi
            printf '%s\n%s\n%s\n%s\n' \
                'v4 SoC linux boot smoke: cy=d83790 x3=aa pass=1' \
                "CELOX_TEST_CONFIG test=$GATE_TEST backend=native opt_level=O2 four_state=false compile_only=false" \
                "CELOX_TEST_TIMING test=$GATE_TEST compile_ns=$compile_elapsed execute_ns=$execute_elapsed jit_execute_ns=$jit_execute_elapsed" \
                "CELOX_TEST_RESULT test=$GATE_TEST status=$MOCK_CELOX_STATUS elapsed_ns=$reported" >"$log"
            if [[ "$MOCK_MUTATE_SOURCE" == 1 ]]; then
                : >"$HELIODOR_DIR/.fixture-mutated"
            fi
            if [[ "$MOCK_MUTATE_RUNNER" == 1 ]]; then
                printf '%s\n' '# mutation' >>"$CELOX_RUNNER_BIN"
            fi
            ;;
        celox-tiered)
            elapsed="$MOCK_CELOX_ELAPSED"
            compile_elapsed=5
            execute_elapsed=18
            reported=24
            if [[ "$MOCK_CELOX_STATUS" == fail ]]; then
                process_status=1
                semantic_status=fail
                elapsed=NA
            fi
            printf '%s\n%s\n%s\n%s\n%s\n' \
                'v4 SoC linux boot smoke: cy=d83790 x3=aa pass=1' \
                "CELOX_TEST_CONFIG test=$GATE_TEST backend=tiered opt_level=O2 four_state=false compile_only=false" \
                "CELOX_TEST_TIMING test=$GATE_TEST compile_ns=$compile_elapsed execute_ns=$execute_elapsed jit_execute_ns=NA" \
                "CELOX_TIERED_STATS test=$GATE_TEST tier=compiled promotion=promoted interpreted_evaluations=100 compiled_evaluations=200 promoted_after_interpreted_evaluations=100 promotion_elapsed_ns=12 safe_point_polls=101 split_apply_deferrals=0 threshold_deferrals=0" \
                "CELOX_TEST_RESULT test=$GATE_TEST status=$MOCK_CELOX_STATUS elapsed_ns=$reported" >"$log"
            ;;
        *) fail "unexpected mock runner: $runner" ;;
    esac
    MOCK_RUNNERS="${MOCK_RUNNERS:+$MOCK_RUNNERS }$runner"
    append_result_row "$HELIODOR_RESULTS_DIR/results.tsv" "$runner" "$test" \
        "$process_status" "$elapsed" "$log" "$semantic_status" \
        "${elapsed/NA/100}" "$reported" "$compile_elapsed" "$execute_elapsed" \
        "$jit_execute_elapsed" >/dev/null
    [[ "$process_status" == 0 ]]
}

run_gate_fixture() {
    local name="$1"
    local expect_success="$2"
    CELOX_ROOT="$TMP/fake-celox-$name"
    mkdir -p "$CELOX_ROOT"
    mkdir -p "$CELOX_ROOT/target/release"
    printf '%s\n' '#!/bin/sh' 'echo stale runner; exit 9' \
        >"$CELOX_ROOT/target/release/celox-heliodor"
    chmod +x "$CELOX_ROOT/target/release/celox-heliodor"
    HELIODOR_DIR="$TMP/hostile-heliodor"
    HELIODOR_RESULTS_DIR="$TMP/hostile-results"
    HELIODOR_TOOLS_DIR="$TMP/hostile-tools"
    HELIODOR_REF=hostile-ref
    HELIODOR_TESTS=hostile-test
    HELIODOR_RUNNERS=celox-cranelift
    HELIODOR_TIMEOUT_SEC=1
    HELIODOR_COMPILE_ONLY=1
    HELIODOR_VERYL_VERSION=99.0.0
    VERYL_BIN="$TMP/hostile-path-veryl"
    CELOX_OPT_LEVEL=O0
    CELOX_SIR_PASS_OVERRIDES='+hostile'
    HELIODOR_CELOX_CARGO_FEATURES=hostile-feature
    MOCK_RUNNERS=""
    LAST_GATE_RESULTS_ROOT="$CELOX_ROOT/target/heliodor/results"
    if run_gate >"$TMP/$name.stdout" 2>"$TMP/$name.stderr"; then
        [[ "$expect_success" == 1 ]] || fail "$name unexpectedly passed"
    else
        [[ "$expect_success" == 0 ]] || {
            sed -n '1,200p' "$TMP/$name.stderr" >&2
            fail "$name unexpectedly failed"
        }
    fi
    assert_eq "$MOCK_RUNNERS" "veryl-cc-sync celox celox-tiered" "$name runner order"
}

run_gate_fixture success 1
assert_eq "$(find "$LAST_GATE_RESULTS_ROOT" -name results.tsv | wc -l)" 1 \
    "one isolated result file"
success_results="$(find "$LAST_GATE_RESULTS_ROOT" -name results.tsv)"
assert_eq "$(wc -l <"$success_results")" 4 "exact benchmark result rows"

MOCK_CELOX_EXECUTE=41
run_gate_fixture slower-integration 1
MOCK_CELOX_EXECUTE=20

MOCK_CELOX_STATUS=fail
run_gate_fixture semantic-failure 0
MOCK_CELOX_STATUS=pass

MOCK_MUTATE_SOURCE=1
run_gate_fixture source-mutation 0
MOCK_MUTATE_SOURCE=0

MOCK_MUTATE_RUNNER=1
run_gate_fixture runner-mutation 0
MOCK_MUTATE_RUNNER=0

CELOX_ROOT="$REAL_CELOX_ROOT"
echo "run-heliodor-bench gate fixture tests: PASS"
