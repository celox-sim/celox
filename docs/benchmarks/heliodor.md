# Heliodor Macro Benchmark

Heliodor is a large Veryl RISC-V processor project with ignored Linux boot tests. It is useful as a macro benchmark because it stresses project loading, large memories initialized by `$readmemh`, native testbench scheduling, and long-running sequential simulation.

This benchmark is not part of normal CI. It checks out Heliodor under `target/heliodor/source`, installs a missing Veryl CLI into `target/heliodor/tools`, builds the Celox runner before timing, runs Veryl baselines before Celox by default, and writes a TSV summary plus full logs under `target/heliodor/results`.

## Run

```bash
scripts/run-heliodor-bench.sh prepare
scripts/run-heliodor-bench.sh run
```

`run` is a configurable diagnostic command. It appends measurements, but does
not decide whether Celox meets the performance requirement. Use the fixed
`gate` command for that decision.

By default this runs `test_soc_linux_boot` with Veryl Cranelift, Veryl cc, then Celox. Celox is timed out after `HELIODOR_CELOX_TIMEOUT_MULTIPLIER` times the fastest successful Veryl baseline for that test.

```bash
HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" \
HELIODOR_RUNNERS="celox veryl-cranelift veryl-cc" \
scripts/run-heliodor-bench.sh run
```

The script pins Heliodor to commit `7ad830fc0f8506c934b61a853ce2eadfa5926b82` unless `HELIODOR_REF` is set.

## Compile/execution split

Use the synchronous Veryl-CC runner when diagnosing generated-code throughput:

```bash
HELIODOR_RUNNERS="veryl-cc-sync celox" \
HELIODOR_TIMEOUT_SEC=300 \
scripts/run-heliodor-bench.sh run
```

Both runners report build/prepare separately from testbench execution. The
compile interval includes frontend analysis, optimization, native code
generation, simulator initialization, and initial-testbench lowering; it ends
immediately before the already-built testbench is executed. The Veryl runner
uses the same synchronous AOT-C setup as
Veryl 0.20.2's Heliodor benchmark (`aot_c_async=false`), so compilation is
finished before execution starts. This avoids measuring an input-dependent
Cranelift-to-C hot-swap point. Every Veryl-CC run receives a new empty temporary
AOT cache, so a shared cached `.so` cannot turn a code-generation measurement
into a cache-hit measurement. Source loading and process startup remain in
`process_elapsed_ns`, outside both internal intervals.

The first non-LTO split run reached the exact
`cy=9ae070 x3=aa pass=1` completion with these internal times:

| Runner | Compile | Execute |
|---|---:|---:|
| `veryl-cc-sync` | 58.354 s | 54.282 s |
| `celox` | 40.450 s | 137.675 s |

Thus the current generated-code execution gap is `2.536x`; Celox's cold compile
interval is `0.693x` the Veryl interval in this run. The earlier `2.605x`
fixed-gate result is an end-to-end process ratio, not an execution-only ratio.
Use `execute_elapsed_ns` to retain or reject native runtime optimizations, and
`compile_elapsed_ns` to evaluate compiler latency. This run is recorded under
`target/heliodor/results/split_timing_aligned_20260716T021500Z`.

## Acceptance gate

Run the reproducible end-to-end comparison from a clean, committed Celox
checkout:

```bash
scripts/run-heliodor-bench.sh gate
```

The gate is deliberately not configurable. It forces all of the following:

- Heliodor commit `7ad830fc0f8506c934b61a853ce2eadfa5926b82`
  from the official repository, with a clean checkout;
- benchmark-owned Veryl `0.20.2`, selected by its exact path and checked with
  `--version` rather than taken from `PATH` or `VERYL_BIN`;
- a clean, unchanged Celox `HEAD`, a locked release/LTO build in a fresh
  invocation-owned Cargo target directory, and execution of that exact built
  binary;
- `test_soc_linux_boot`, runners `veryl-cc` then `celox`, and a fixed 300-second
  timeout for each;
- a new empty Veryl AOT cache for the invocation, removed after the run;
- Celox native backend, `O2`, two-state mode, full execution, and no SIR pass
  overrides; and
- separate detached Heliodor worktrees for the two runners so project-local
  generated files cannot flow from one runner into the other.

The gate writes a new isolated `gate_<timestamp>.<suffix>` directory under
`target/heliodor/results`. It accepts exactly two result rows from that
invocation. Veryl must exit successfully and log exactly one success for the
requested test plus `1 passed, 0 failed`. Celox must exit successfully and log
exactly one native/O2/`four_state=false`/`compile_only=false` config record and
one full-pass result record. Source manifests, checkout identities, and runner
executable hashes are checked before and after execution. Both logs must also
contain exactly one architectural completion marker equal to
`cy=9ae070 x3=aa pass=1`; leading hexadecimal zeroes are ignored.

Subprocess elapsed time is measured with a monotonic nanosecond clock. The gate
exits successfully only if both semantic checks pass and the Celox process time
is no greater than the Veryl process time. Compile-only completion, a partial
window, runner-reported internal time, or process exit zero without the exact
markers is a failure. GNU `timeout` with `--kill-after` and Python 3 are required.
This fixed gate remains an end-to-end acceptance check; use the synchronous
runner above when the compile and execution components must be compared.

The latest iterative non-LTO comparison completed the same
`cy=9ae070` workload in `76.446 s` with Veryl-CC and `184.652 s` with Celox.
The final fixed gate was then run from clean Celox commit `e917489e` with its
fresh locked release/LTO runner. Veryl-CC took `68.409 s`; Celox took
`178.223 s` process time and reported `178.019 s` internally. Both semantic
checks passed and both logs contained exactly one
`cy=9ae070 x3=aa pass=1` marker, but Celox was `2.605x` slower. The gate
therefore failed only its no-slower-than-Veryl performance condition. Its
artifacts are in
`target/heliodor/results/gate_20260716T010312Z.tcVUZd`. Routine development
runs continue to use the non-LTO `heliodor-dev` profile.

## Tests

List available Heliodor `#[test]` modules:

```bash
scripts/run-heliodor-bench.sh list
```

Useful long tests include:

| Test | Meaning |
|---|---|
| `test_soc_linux_boot` | Linux 5.15 single-hart boot |
| `test_soc_smp_linux_boot_2hart` | Linux 5.15 SMP 2-hart boot |
| `test_soc_smp_linux_boot_4hart` | Linux 5.15 SMP 4-hart boot |
| `test_soc_linux_boot_71` | Linux 7.1 single-hart boot |
| `test_soc_smp_linux_boot_71_2hart` | Linux 7.1 SMP 2-hart boot |
| `test_soc_linux_boot_71v` | Linux 7.1 vector-enabled boot |

## Runners

`HELIODOR_RUNNERS` accepts:

| Runner | Command |
|---|---|
| `celox` | `target/<profile>/examples/run_veryl_project_test --project ... --test ...` |
| `veryl-cc-sync` | Direct Veryl 0.20.2 synchronous AOT-C runner with split timing |
| `veryl-cc` | `veryl test --ignored --test ... --backend cc` |
| `veryl-cranelift` | `veryl test --ignored --test ... --backend cranelift` |
| `veryl-interpret` | `veryl test --ignored --test ... --backend interpret` |

The Celox runner uses the default Celox backend, which is native x86-64 on x86-64 hosts. Set `CELOX_OPT_LEVEL=O0|O1|O2` to change optimizer presets.

Set `HELIODOR_TIMEOUT_SEC` to override all per-test timeouts. Without a measured Veryl baseline, Linux boot tests use conservative fixed fallbacks such as 300s for single-hart boot, 600s for 2-hart SMP boot, and 1800s for 4-hart SMP boot.

If `veryl` is not on `PATH`, the script installs `cargo install veryl --version 0.20.2 --locked` into `target/heliodor/tools/veryl-0.20.2`. Override with `VERYL_BIN`, `HELIODOR_VERYL_VERSION`, or set `HELIODOR_INSTALL_TOOLS=0` to disable automatic installs.

## Result semantics

`target/heliodor/results/results.tsv` distinguishes the subprocess exit status
from the simulated test result. Its columns are:

| Column | Meaning |
|---|---|
| `runner` | Runner name |
| `test` | Requested Heliodor test |
| `status` | Legacy alias of `exit_status`, retained as the third column for existing readers |
| `elapsed_ns` | Full-pass wall time, or `NA` for every non-pass result |
| `log` | Full runner log |
| `semantic_status` | `pass`, `fail`, `compile-only`, `unreported`, or `invalid` |
| `exit_status` | Subprocess exit status |
| `process_elapsed_ns` | Monotonic elapsed time of the subprocess, including failed and compile-only runs |
| `reported_elapsed_ns` | Runner's internal total elapsed value, or `NA` when unavailable |
| `compile_elapsed_ns` | Internal build/prepare time through simulator and testbench construction, or `NA` when unavailable |
| `execute_elapsed_ns` | Internal testbench execution time after code generation, or `NA` when unavailable |

The original `runner`, `test`, `status`, `elapsed_ns`, and `log` columns remain
in their original positions. A speed result exists only when
`semantic_status=pass`, `exit_status=0`, and `elapsed_ns` is numeric.
`process_elapsed_ns` and `reported_elapsed_ns` are diagnostics and must not be
used to claim full-test performance for `compile-only`, `fail`, `unreported`,
or `invalid` rows.

For Celox, the script requires exactly one timing line and one result line:

```text
CELOX_TEST_TIMING test=<requested-test> compile_ns=<integer> execute_ns=<integer>
CELOX_TEST_RESULT test=<requested-test> status=pass|fail|compile-only elapsed_ns=<integer>
```

`veryl-cc-sync` uses the corresponding `VERYL_TEST_TIMING` and
`VERYL_TEST_RESULT` records. A compile-only Celox result must report
`execute_ns=0`, and every split interval must fit within its internal total.

Malformed, duplicate, missing, wrong-test, mode-inconsistent, or
exit-status-inconsistent records cannot become a pass. An intentional
`HELIODOR_CELOX_COMPILE_ONLY=1` run may finish successfully, but its
`semantic_status` is `compile-only` and its `elapsed_ns` is `NA`.

Existing five- and nine-column TSV files are migrated atomically on the next
run. The script keeps the first copy as `results.tsv.v1.bak` or
`results.tsv.v2.bak`, recovers split timing from referenced logs where possible,
and marks unavailable timing as `NA`. Migration never promotes process exit
zero alone to a Celox full pass.

The parser/migration and acceptance-gate fixtures run without checking out or
executing Heliodor or either compiler:

```bash
bash scripts/tests/run-heliodor-bench-results.sh
bash scripts/tests/run-heliodor-bench-gate.sh
```

## Architectural completion marker

The Celox testbench runner forwards Heliodor's `$display` output, so both Celox
and Veryl logs contain the simulated cycle, architectural result register, and
pass bit. The fixed gate validates `cy=9ae070 x3=aa pass=1` independently of
the process exit and test-result records. This check is required: an earlier
native ISel width bug still powered down with `pass=1`, but at `cy=9ab960`.
