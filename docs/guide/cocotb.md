# cocotb and the Celox CLI

Celox can compile a Veryl or SystemVerilog design into a native executable that runs ordinary
[cocotb](https://www.cocotb.org/) tests through VPI. The generated executable
contains the compiled design and the Celox runtime; it does not need the HDL
sources when it runs.

## Build the CLI

The CLI is currently built from the Celox source tree:

```sh
cargo build --release -p celox-vpi --features systemverilog --bin celox
```

The `run` command below needs no Python installation. For VPI tests, install
cocotb into the Python environment you want to use:

```sh
python3 -m pip install cocotb
```

The repository dev container already includes cocotb 2.0.1 and Verilator.

## Run an HDL testbench directly

Use the same command for a SystemVerilog testbench and a Veryl native
`#[test]` module:

```sh
target/release/celox run src/types.sv src/Dut.sv test/Top.sv --top Top
target/release/celox run test/Top.veryl --top Top
```

For SystemVerilog, pass `.sv` or `.v` sources in compilation order. The command
uses four-state simulation and runs until `$finish` or no scheduled events
remain. For Veryl, pass one project entry file: Celox discovers `Veryl.toml`
and collects the project's sources and dependencies. A standalone
Veryl file also works. Select the native testbench module with `--top`.
Veryl uses the existing native testbench executor; a test may also complete by
falling through its initial blocks.

The command uses the existing frontends' supported language subsets; unsupported
constructs produce compilation diagnostics. `-O O0`, `-O O1`, and `-O O2` select
the existing compiler optimization levels. Both languages default to O2, including
dead store elimination; signals read by the HDL testbench are retained
automatically. An explicit `-O` overrides the default.

Successful completion returns status 0. Compilation errors, runtime errors,
assertion failures, `$error`, `$fatal`, lost runtime events, and exhausted
execution limits return status 1. SV `$stop` also returns status 1 because this
command has no interactive debugger. Invalid command arguments return status 2.
SV `$display`/`$write` output goes to stdout; the existing Veryl native
testbench executor sends its HDL output to stderr.

`--max-steps N` sets a positive execution limit (default: 1000000). It counts
scheduler steps for SV and testbench ticks for Veryl. Reaching the Veryl tick
limit before completion fails the command. This limit does not bound compilation
or work within a single step/tick; use an external process timeout when a wall
clock limit is needed. Source languages cannot be mixed in one invocation.

## Compile a design

Pass one source in the Veryl project and name the top-level module:

```sh
target/release/celox vpi build src/Top.veryl --top Top -o build/top-sim
```

Celox finds `Veryl.toml` from the source path and includes the project's source
files and dependencies. For a standalone source outside a Veryl project, it
compiles that file alone. The output defaults to `celox.out` when `-o` is
omitted.

For SystemVerilog, pass one or more `.sv` files in compilation order:

```sh
target/release/celox vpi build src/types.sv src/Child.sv src/Top.sv --top Top -o build/top-sim
```

This compiles the specified files using Celox's supported SystemVerilog subset.
List package definitions before their consumers. It does not discover a
`Veryl.toml` project or additional SystemVerilog source files. Multiple inputs
must all be `.sv` files; for Veryl, pass one project entry file. The
`celox-vpi-compile` binary accepts the same source list.

## Run cocotb

Suppose `test/test_top.py` contains a cocotb test. Run the generated executable
from a directory where that module is importable:

```sh
PYTHONPATH=test build/top-sim --test-module test_top
```

The executable automatically:

- uses `python3` and locates its cocotb VPI adapter and libpython;
- supplies the compiled top-level name to cocotb;
- writes `results.xml` and returns a failing exit status when a test fails.

Use another Python environment or result path explicitly when needed:

```sh
build/top-sim \
  --test-module test_top \
  --python .venv/bin/python \
  --results-file build/results.xml
```

`--test-filter REGEX` selects matching tests; `--testcase NAME` is also
available for cocotb's legacy test selection. `--vpi PATH` overrides automatic
VPI adapter discovery. The corresponding cocotb environment variables remain
supported for existing automation: `PYGPI_PYTHON_BIN`, `LIBPYTHON_LOC`,
`CELOX_COCOTB_VPI`, `COCOTB_TEST_MODULES`, `COCOTB_TEST_FILTER`, and
`COCOTB_RESULTS_FILE`.

## Current compatibility

The compatibility layer covers cocotb 2.0 paths for module and signal
discovery, immediate/deposit/force/release writes, scalar and vector values,
simulation time, and the Start, ReadWrite, ReadOnly, NextTimeStep, Timer,
ValueChange, and End callback regions.

Packed bit handles, delayed VPI writes, unpacked-array indexing, and
derived/cascaded clock scheduling are not yet supported.
