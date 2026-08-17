# cocotb with a native Celox image

Celox can compile a Veryl design into a native image appended to a prebuilt VPI
runtime. The resulting executable loads cocotb's ordinary Icarus VPI adapter;
cocotb itself does not need a Celox-specific Python package.

Build the reusable tools once:

```sh
cargo build --release -p celox-vpi \
  --bin celox-vpi-runtime --bin celox-vpi-compile
```

Attach a design to the runtime:

```sh
mkdir -p build
target/release/celox-vpi-compile design.veryl \
  --top Top \
  --runtime target/release/celox-vpi-runtime \
  --output build/design-sim
```

Set the normal cocotb environment and run the generated executable. The exact
VPI filename is obtained through Python because cocotb 2.0 uses `.vpl` for its
Icarus adapter on Linux:

```sh
export PYGPI_PYTHON_BIN="$(command -v python3)"
export LIBPYTHON_LOC="$(python3 -m cocotb_tools.config --libpython)"
export CELOX_COCOTB_VPI="$(python3 -c \
  "from pathlib import Path; import cocotb; print(Path(cocotb.__file__).parent / 'libs' / 'libcocotbvpi_icarus.vpl')")"
export COCOTB_TOPLEVEL=Top
export COCOTB_TEST_MODULES=test_design
export TOPLEVEL_LANG=verilog

build/design-sim
python3 -m cocotb_tools.check_results results.xml
```

`--vpi PATH` can be passed to the generated executable instead of setting
`CELOX_COCOTB_VPI`.

The current compatibility layer implements the cocotb 2.0.1 paths for module
and signal discovery, immediate/deposit/force/release writes, scalar and vector
values, simulation time, and the Start, ReadWrite, ReadOnly, NextTimeStep,
Timer, ValueChange, and End callback regions. Packed bit handles, delayed VPI
writes, unpacked-array indexing, and derived/cascaded clock scheduling are not
yet supported.
