# Heliodor Macro Benchmark

Heliodor is a large Veryl RISC-V processor project with ignored Linux boot tests. It is useful as a macro benchmark because it stresses project loading, large memories initialized by `$readmemh`, native testbench scheduling, and long-running sequential simulation.

This benchmark is not part of normal CI. It checks out Heliodor under `target/heliodor/source`, installs a missing Veryl CLI into `target/heliodor/tools`, builds the Celox runner before timing, runs Veryl baselines before Celox by default, and writes a TSV summary plus full logs under `target/heliodor/results`.

## Run

```bash
scripts/run-heliodor-bench.sh prepare
scripts/run-heliodor-bench.sh run
```

By default this runs `test_soc_linux_boot` with Veryl Cranelift, Veryl cc, then Celox. Celox is timed out after `HELIODOR_CELOX_TIMEOUT_MULTIPLIER` times the fastest successful Veryl baseline for that test.

```bash
HELIODOR_TESTS="test_soc_linux_boot test_soc_smp_linux_boot_2hart" \
HELIODOR_RUNNERS="celox veryl-cranelift veryl-cc" \
scripts/run-heliodor-bench.sh run
```

The script pins Heliodor to commit `7ad830fc0f8506c934b61a853ce2eadfa5926b82` unless `HELIODOR_REF` is set.

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

## Current Caveat

Celox currently records pass/fail and wall-clock time for this macro benchmark. Heliodor prints simulated cycle counts with `$display`; the current Celox detailed test runner suppresses display events, so cycle extraction is only available from Veryl runner logs until display forwarding is exposed in the Celox runner.
