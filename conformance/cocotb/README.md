# cocotb upstream tests

This gate executes unmodified tests and HDL from cocotb 2.1.0, pinned by
[`revision`](revision), using Celox's attached VPI executable. It currently
covers five upstream groups: the main `test_cocotb` regression, coincident
triggers, packed vector indexing, handle comparisons, and multidimensional arrays. This is a focused subset,
not the complete cocotb test suite. VHDL-only tests retain upstream's skips.
No test filter, expected-failure override, or HDL rewrite is applied.

From the repository root, with the development shell active:

```sh
git clone --branch v2.1.0 --depth 1 https://github.com/cocotb/cocotb.git work/cocotb
python3 -m venv work/cocotb-python
work/cocotb-python/bin/python -m pip install -r conformance/cocotb/requirements.txt
cargo build --locked -p celox-vpi --bin celox
python3 conformance/cocotb/run.py work/cocotb \
  --python "$PWD/work/cocotb-python/bin/python"
```

The Python environment must use the same platform libraries as the development
shell. Keep the virtual environment's interpreter path when passing `--python`;
resolving its symlink loses the virtual environment.

Select a group with `--case test_first_on_coincident_triggers` (repeatable).
Compilation and execution each have a timeout (`--timeout`, seconds).
`--celox` selects a different built executable. Logs, per-group xUnit XML,
and `summary.json` go under `work/cocotb-results/` by default; `--output`
changes the report directory. The runner rejects the wrong source revision,
modified upstream tests, Python/cocotb setup errors, compilation failures,
timeouts, simulator failures, and empty or skipped-only selections. Any failing
group makes the command fail after the remaining selected groups execute.

The main `test_cocotb` selection runs every module listed in the upstream
Makefile. Its HDL includes scalar logic gates, real and string ports, and
escaped identifiers. Select it alone with `--case test_cocotb`. Other upstream
make-based groups and VHDL simulation are outside this runner's current scope.

Runner contract checks:

```sh
python3 -m unittest discover -s conformance/cocotb -p 'test_*.py' -v
```
