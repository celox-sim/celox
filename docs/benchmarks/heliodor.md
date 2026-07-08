# Heliodor Macro Benchmark

Heliodor is a large Veryl RISC-V processor project with ignored Linux boot tests. It is useful as a macro benchmark because it stresses project loading, large memories initialized by `$readmemh`, native testbench scheduling, and long-running sequential simulation.

This benchmark is not part of normal CI. It checks out Heliodor under `target/heliodor/source`, runs selected ignored tests, and writes a TSV summary plus full logs under `target/heliodor/results`.

The TSV elapsed time is the outer command wall time. Run once to warm Cargo/Veryl build caches before comparing simulator throughput.

## Run

```bash
scripts/run-heliodor-bench.sh prepare
scripts/run-heliodor-bench.sh run
```

By default this runs `test_soc_linux_boot` with Celox, Veryl Cranelift, and Veryl cc:

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
| `celox` | `cargo run -p celox --example run_veryl_project_test --release -- --project ... --test ...` |
| `veryl-cc` | `veryl test --ignored --test ... --backend cc` |
| `veryl-cranelift` | `veryl test --ignored --test ... --backend cranelift` |
| `veryl-interpret` | `veryl test --ignored --test ... --backend interpret` |

The Celox runner uses the default Celox backend, which is native x86-64 on x86-64 hosts. Set `CELOX_OPT_LEVEL=O0|O1|O2` to change optimizer presets.

## Current Caveat

Celox currently records pass/fail and wall-clock time for this macro benchmark. Heliodor prints simulated cycle counts with `$display`; the current Celox detailed test runner suppresses display events, so cycle extraction is only available from Veryl runner logs until display forwarding is exposed in the Celox runner.
