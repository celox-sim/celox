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
- a clean, unchanged Celox `HEAD`, a locked release build in a fresh
  invocation-owned Cargo target directory, and execution of that exact built
  binary;
- `test_soc_linux_boot`, runners `veryl-cc` then `celox`, and a fixed 300-second
  timeout for each;
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
executable hashes are checked before and after execution.

Subprocess elapsed time is measured with a monotonic nanosecond clock. The gate
exits successfully only if both semantic checks pass and the Celox process time
is no greater than the Veryl process time. Compile-only completion, a partial
window, runner-reported internal time, or process exit zero without the exact
markers is a failure. GNU `timeout` with `--kill-after` and Python 3 are required.

Celox has not yet produced a competitive successful full Linux-boot result on
this gate. The existence of the command makes the acceptance decision
executable; it is not itself evidence that the performance requirement passes.

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
| `celox` | `target/release/examples/run_veryl_project_test --project ... --test ...` |
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
| `reported_elapsed_ns` | Celox runner's internal elapsed value, or `NA` when unavailable |

The original `runner`, `test`, `status`, `elapsed_ns`, and `log` columns remain
in their original positions. A speed result exists only when
`semantic_status=pass`, `exit_status=0`, and `elapsed_ns` is numeric.
`process_elapsed_ns` and `reported_elapsed_ns` are diagnostics and must not be
used to claim full-test performance for `compile-only`, `fail`, `unreported`,
or `invalid` rows.

For Celox, the script requires exactly one complete log line in this form:

```text
CELOX_TEST_RESULT test=<requested-test> status=pass|fail|compile-only elapsed_ns=<integer>
```

Malformed, duplicate, missing, wrong-test, mode-inconsistent, or
exit-status-inconsistent records cannot become a pass. An intentional
`HELIODOR_CELOX_COMPILE_ONLY=1` run may finish successfully, but its
`semantic_status` is `compile-only` and its `elapsed_ns` is `NA`.

An existing five-column TSV is migrated atomically on the next run. The script
keeps its first copy as `results.tsv.v1.bak`, recovers Celox semantics from the
referenced logs where possible, and marks records without conclusive evidence
as `unreported` or `invalid`. The migration never promotes process exit zero
alone to a Celox full pass.

The parser/migration and acceptance-gate fixtures run without checking out or
executing Heliodor or either compiler:

```bash
bash scripts/tests/run-heliodor-bench-results.sh
bash scripts/tests/run-heliodor-bench-gate.sh
```

## Current Caveat

Celox currently records the semantic result, process result, and wall-clock time for this macro benchmark. Heliodor prints simulated cycle counts with `$display`; the current Celox detailed test runner suppresses display events, so cycle extraction is only available from Veryl runner logs until display forwarding is exposed in the Celox runner.
