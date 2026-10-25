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
uses two-state simulation by default and runs until `$finish` or no scheduled
events remain. Use `--four-state` or `four_state = true` in `celox.toml` to retain
X/Z values. For Veryl, pass one project entry file: Celox discovers `Veryl.toml`
and collects the project's sources and dependencies. A standalone
Veryl file also works. Both languages default to two-state simulation. Select the native testbench module with `--top`.
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
clock limit is needed. `--unlimited` removes the step/tick limit. Source languages cannot be mixed in one invocation.

For a SystemVerilog testbench, a positional argument starting with `+` (or
`--plusarg +ARG`, repeatable) is a plusarg, not a source: `$test$plusargs` and
`$value$plusargs` see them in command-line order, on every backend.

```sh
celox run tb.v --top tb +iterations=9 --plusarg +trace
```

## Select execution and collect measurements

`celox run` accepts `--threads N` (1–64, default 1) and `--four-state` for both
languages. Both languages support `--backend native|tiered|interpreter|cranelift|wasm`
(default: tiered; native when `--compile-only`, a native image, `--build-cache-dir` or an
IR dump is requested). Tiered starts interpreted and adopts compiled code at a scheduler
safe point while preserving pending events. For example:

```sh
celox run test/Top.sv --top Top --backend tiered --threads 4
```

### Save state files during a SystemVerilog run

To compare two runs or two simulators, `celox run` can save
[state files](checkpoints.md#state-files) for `celox state dump` and `celox state diff`:

| Options | Purpose |
| --- | --- |
| `--save-state-on SIGNAL` | Count the cycles of `SIGNAL`, a hierarchical path such as `tb.clk` (the top module's name may be left out). A cycle is a time step at whose end the signal is 1 while it was not at the end of the previous time step. |
| `--save-state-at N[,M...]` | Save the state after cycles N, M, ... of `--save-state-on`, counted from 1, as `<SIGNAL>-<N>.state`. The file is written right after that time step, with every region of the time done, so its nonblocking updates are included. |
| `--save-state-at-time T[,U...]` | Save the state after everything due at times up to T, before the run advances past T, as `time-<T>.state`. T is in units of the design's global time precision. |
| `--save-state-dir DIR` | Write the files to `DIR` (default: the current directory). |

A cycle that the run never reaches is an error. These options are available for
SystemVerilog testbenches on every backend.

```sh
celox run c906.v --top tb --save-state-on tb.clk --save-state-at 1000,2000 --save-state-dir states
celox state diff states/tb.clk-1000.state other/tb.clk-1000.state
```

For Veryl native testbenches it also provides:

| Options | Purpose |
| --- | --- |
| `--report` | Emit machine-readable `CELOX_*` configuration, timing, tiered statistics and result records on stdout. |
| `--require-compiled` | With `--report --backend tiered`, require generated code to be exercised in a completed run. Normal tiered runs can finish on the interpreter. |
| `--compile-only` | Compile native, Cranelift or WASM code without executing the testbench. |
| `--build-cache-dir DIR` | Cache native compilation for a Veryl.toml project in a trusted local directory. |
| `--native-image-output FILE` | Export a native image with `--compile-only`. |
| `--native-image-input FILE` | Execute an image without reading HDL sources. |
| `--dump-ir-dir DIR` | Write native compiler diagnostics; add `--dump-ir-and-run` to execute as well. |

```sh
celox run --project /path/to/veryl-project --top smoke --backend tiered --threads 4
celox run test/Top.veryl --top Top --four-state
celox run --project /path/to/veryl-project --top smoke --report --unlimited
```

`--project` accepts a Veryl project directory or `Veryl.toml` as well as
`celox.toml`. `--source-file PATH` selects a Veryl source relative to the project;
repeat it to select multiple files. Heliodor benchmarks invoke this same CLI
with `--report --unlimited`; no separate Celox benchmark executable is used.
The report preserves compile/execute/generated-code timing intervals, and tiered
execution time includes background compilation and promotion. Short tiered runs
need not promote unless `--require-compiled` is requested.

## Manage a multi-file project

Save common inputs and named targets in `celox.toml`:

```toml
sources = ["rtl/pkg.sv"]
filelists = ["rtl/design.f"]
include_dirs = ["include"]
default_target = "smoke"

[defines]
SIMULATION = "1"

[diagnostics]
mixed_timescale = "warn"

[targets.smoke]
sources = ["test/Tb.sv"]
top = "Tb"
opt_level = "O1"
max_steps = 1000000

[targets.dut]
top = "Dut"
```

Both commands resolve the same sources, include directories, macros and top:

```sh
celox run --target smoke
celox vpi build --target dut -o build/dut-sim
celox run --project /path/to/celox.toml --target smoke
```

With no explicit sources or `-f` arguments, or when `--target` is present, Celox
searches the current directory and its parents for `celox.toml`. Explicit
`--project FILE` selects a manifest directly. Relative manifest paths resolve
from the manifest's directory, including paths in its targets. Positional
sources and `-f` alone run independently of any nearby manifest.

Targets inherit the root settings. Sources are collected in this order: root
`sources`, root `filelists`, target `sources`, target `filelists`, then CLI
sources and `-f` expansions in command-line order. Each list retains its source order. Include
directories accumulate in the same layer order. Macro definitions are global
to the invocation; later definitions win, with CLI `-D` definitions applied last.
TOML macro values are strings. Configured macros apply to each source; macros
defined in HDL files retain the frontend's per-file compilation-unit scope. `--top` overrides the selected top; `run`'s `-O`
and `--max-steps` override the target/root run settings. VPI builds retain the
existing force-compatible compilation settings.

Without `--target`, Celox uses `default_target`, otherwise the root `top`,
otherwise the sole target. Multiple targets without a default require
`--target`. A project may also name a single `.veryl` entry file; its Veryl
project discovery still uses `Veryl.toml`.

Compilation diagnostic levels can be set in `[diagnostics]`, overridden in
`[targets.NAME.diagnostics]`, and overridden again with repeatable
`--diagnostic CODE=allow|warn|error` options on either command. See
[diagnostic configuration](./celox-toml.md#diagnostics) for supported recovery
behavior and examples.

### Use a file list directly

```sh
celox run -f test/all.f --top Tb
celox vpi build -f rtl/design.f --top Dut -o build/dut-sim
```

A `.f` file contains whitespace-separated source paths and can contain nested
lists and preprocessing options:

```text
# Paths resolve from this file's directory.
../rtl/pkg.sv
-f ../rtl/design.f
+incdir+../include
+define+SIMULATION=1
"../test/with spaces/Tb.sv"
```

Supported options are `-f FILE`, `-I DIR`/`-IDIR`, `-D NAME[=VALUE]`/`-DNAME[=VALUE]`,
`+incdir+DIR[+DIR...]` and `+define+NAME[=VALUE][+NAME[=VALUE]...]`.
Bare macro names expand to `1`. Paths may be quoted or use backslash escapes;
`#` and `//` start comments at a token boundary, and `/* ... */` comments are
also accepted. Every nested list resolves paths from its own directory.
The file-list reader performs no environment, wildcard or shell expansion.
Unknown options, unknown TOML fields and cyclic file lists are errors.
The CLI accepts `-I`/`--include-dir` and `-D`/`--define` directly as well.
Include directories and macros require SystemVerilog inputs and a build with
`--features systemverilog`.

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

cocotb drives the design, so the compiled executable runs no HDL process.
Compilation fails for a design with an `initial` process, an ordinary `always`
process or another procedure Celox runs as a process, and names the first one;
constant `initial` blocks and declaration initializers still set initial
values.

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
