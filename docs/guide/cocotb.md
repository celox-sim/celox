# cocotb and the Celox CLI

Celox can compile a Veryl design into a native executable that runs ordinary
[cocotb](https://www.cocotb.org/) tests through VPI. The generated executable
contains the compiled design and the Celox runtime; it does not need the Veryl
sources when it runs.

## Build the CLI

The CLI is currently built from the Celox source tree:

```sh
cargo build --release -p celox-vpi --bin celox
```

Install cocotb into the Python environment you want to use:

```sh
python3 -m pip install cocotb
```

The repository dev container already includes cocotb 2.0.1 and Verilator.

## Compile a design

Pass one source in the Veryl project and name the top-level module:

```sh
target/release/celox vpi build src/Top.veryl --top Top -o build/top-sim
```

Celox finds `Veryl.toml` from the source path and includes the project's source
files and dependencies. For a standalone source outside a Veryl project, it
compiles that file alone. The output defaults to `celox.out` when `-o` is
omitted.

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
