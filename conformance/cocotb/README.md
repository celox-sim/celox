# cocotb upstream tests

This gate executes unmodified tests and HDL from cocotb 2.1.0, pinned by
[`revision`](revision), using Celox's attached VPI executable. The default
selection covers 22 upstream groups in 23 configurations, including both
inertial-write modes: **534 passed, 0 failed, 5 upstream skips**.
The main `test_cocotb` regression contributes 436 passes and 4 skips.

The selection includes bridge/resume threads, force/release, explicit and
inertial deposits, interfaces, packed unions, array indexing, hierarchy
iteration, random test order, test selection, include directories, long names,
expected exceptions and the main scheduler/trigger regression. Sources and test
bodies are never rewritten. Test-selection and environment options are used
only where the group's own upstream Makefile specifies them. No new expected
failures or simulator-name substitutions are applied.

From the repository root, with the development shell active:

```sh
git clone --branch v2.1.0 --depth 1 https://github.com/cocotb/cocotb.git work/cocotb
python3 -m venv work/cocotb-python
work/cocotb-python/bin/python -m pip install -r conformance/cocotb/requirements.txt
cargo build --locked -p celox-vpi
python3 conformance/cocotb/run.py work/cocotb \
  --python "$PWD/work/cocotb-python/bin/python"
```

The Python environment must use the same platform libraries as the development
shell. Keep the virtual environment's interpreter path when passing `--python`;
resolving its symlink loses the virtual environment. `psutil` is needed for the
optional upstream memory stress group.

Select a configuration with `--case test_async_bridge` (repeatable). The
force/release group explicitly grants force access to its two output signals
through `--force-signal`; it does not enable force globally. The inertial group
runs with `COCOTB_TRUST_INERTIAL_WRITES=0` and `=1` in separate configurations.
Ambient test filters, random-order settings and additional GPI libraries are
cleared before each run so the caller cannot silently change coverage.

Compilation and execution each have a timeout (`--timeout`, seconds).
`--celox` selects a different built executable. Logs, per-group xUnit XML,
and `summary.json` go under `work/cocotb-results/` by default; `--output`
changes the report directory. The runner rejects the wrong source revision,
modified upstream tests, Python/cocotb setup errors, compilation failures,
timeouts, simulator failures, and empty or skipped-only selections. Any failing
group makes the command fail after the remaining selected groups execute.

## Remaining coverage

[`coverage.json`](coverage.json) inventories all **68** upstream test-case
Makefiles at the pinned revision. It distinguishes the 22 enabled groups,
11 pending groups, 19 groups needing their specialized Makefile/pytest
contracts, and 16 VHDL, mixed-language or Verilator-specific groups outside
this Verilog VPI runner's scope. This is not complete cocotb compatibility.

Nine unresolved groups are already selectable with `--case`; `--all` executes
all **registered** configurations, including these failures. Failures remain
failures, not exclusions or expected-failure overrides:

| Group | Remaining issue |
| --- | --- |
| `test_array`, `test_array_simple`, `test_struct` | Unpacked aggregate lowering/discovery |
| `test_discovery` | Primitive/generate handles, typed/scoped parameters, integer and vector metadata |
| `test_package` | Package and compilation-unit object discovery |
| `test_packed_array` | Nested packed indexing, ranges and indexed callbacks |
| `test_similar_scope_name` | Generated-scope arrays |
| `test_deadlock` | Preserve upstream expected-SimFailure results while checking process termination |
| `test_trigger_memory_leak` | NextTimeStep RSS growth exceeds upstream's 2 MiB limit; stress group needs a longer timeout |

The other two pending groups require multiple top-level roots and attached-image
plusargs. The specialized harness groups require checks such as exact expected
failure/skip counts, startup diagnostics, log contents, output files, or repeated
invocations. Merely executing their Python modules is not sufficient evidence.

Runner contract checks:

```sh
python3 -m unittest discover -s conformance/cocotb -p 'test_*.py' -v
```
