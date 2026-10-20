# sv-tests conformance

CI runs the unmodified [sv-tests `tools/runner`](https://github.com/chipsalliance/sv-tests/blob/c4229f3bd5220e6d3ba8f390e5d09c87e462e9c7/tools/runner)
at the commit in [`revision`](revision), using the Celox
[`BaseRunner` adapter](runners/celox.py). [`run.py`](run.py) launches that official
runner in a subprocess for every `.sv` file, as the upstream Makefile does.
It compares the official verdicts with [`expected.tsv`](expected.tsv).
Each test has separate `2state` and `4state` rows (`test`, `state_mode`, `result`).

The official runner owns metadata parsing, compatible-runner filtering, mode
priority, library injection, timeouts, expected failures and simulation
`:assert:` evaluation. The adapter declares only the driver’s supported
`parsing`, `elaboration` and `simulation` modes. It forwards source files, include
directories, defines, an optional explicit top and the selected state mode to
the Rust driver. Parsing mode stops after preprocessing and syntax parsing; it
does not build semantic module metadata, select a root, elaborate or simulate.
The official mode priority still chooses elaboration for default cases and for
cases that permit both parsing and elaboration. In elaboration and simulation
modes, sources without a module still undergo preprocessing and parsing; root
detection then reports the actual no-root diagnostic. The current
compiler and simulator require a module root, so the driver reports a failure for
rootless unit, package and interface inputs. The official runner still decides
the case verdict, including expected failures. There is no `no_top` classification
or preemptive skip. Without an explicit top, the driver infers a single root from
parsed declarations and instantiations; multiple roots receive a diagnostic
because Celox’s simulator requires one root. Each input file is a separate
compilation unit, so source-defined macros do not leak between files;
command-line defines apply to each file.

| Result | Meaning |
| --- | --- |
| `pass` | the official runner emitted `PASS` |
| `fail` | the official runner emitted `FAIL` |
| `skip` | the official runner skipped an incompatible test or unsupported mode |

The official runner’s process exit code is separate from the case verdict.
The wrapper reads its `PASS`/`FAIL` log messages; it does not reconstruct their
rules from tool exit codes or `should_fail` metadata. A negative test may pass
because of an unsupported language construct rather than its intended error.
Crashes and timeouts retain the official runner’s verdict and tool return code.

A runner infrastructure error, missing driver or missing source/library/include
directory aborts collection with exit code 2 and cannot update expectations.
The UVM submodules named by upstream `conf/runners/libs.json` must be available
before any test runs. Nonempty `simvariants` currently cause an explicit adapter
preparation error because the Celox driver has no runtime plusarg support;
variants are never silently ignored.

## Running locally

Use the locked development shell, which provides Python with `psutil`, or install
the pinned [`requirements.txt`](requirements.txt) in a Python environment.

```bash
git clone https://github.com/chipsalliance/sv-tests.git ../sv-tests
git -C ../sv-tests checkout "$(cat conformance/sv-tests/revision)"
git -C ../sv-tests submodule update --init --depth 1 \
    third_party/tests/uvm third_party/tests/uvm-1.2
cargo build --locked --release -p celox --features systemverilog --example sv_tests
PYTHONDONTWRITEBYTECODE=1 python3 conformance/sv-tests/run.py ../sv-tests \
    --driver target/release/examples/sv_tests --out-dir work/sv-tests --jobs 4 \
    --expected conformance/sv-tests/expected.tsv --report work/sv-tests-report.tsv
```

Use the release driver to reproduce CI: upstream time limits also cover compiler
execution, so slower build profiles can change timeout outcomes.

`--jobs` is a positive integer controlling independent official runner processes
(default: 1). CI uses 4 jobs to match the public Ubuntu runner’s
[four CPUs](https://docs.github.com/en/actions/reference/runners/github-hosted-runners). Each case
and state mode has separate logs, and upstream `BaseRunner` creates unique
work directories. Reports and expectations are sorted regardless of completion
order. A runner infrastructure error cancels pending work and cannot update
expectations.

Both state modes run by default. Use `--state-mode 2state` or
`--state-mode 4state` to run and compare only one; `--state-mode both` explicitly
selects the default. After reviewing changed verdicts, add `--update` to rewrite
expectations. A single-mode update preserves the other mode’s rows. Missing
cases and changed results are checked independently for each selected mode.
Update `revision` and the measured expectations together when upgrading upstream.

The native-parser transition reviews twenty expectation rows separately from grammar
repairs. The six module-level deferred `assert`, `assume`, and `cover` probes
(`16.2--*0` and `16.2--*-final`) now fail elaboration explicitly: they are legal
syntax (IEEE 1800-2023 16.4), but their module execution is unsupported. The old
analyzer dropped these items, producing misleading passes. Parsing these forms
still succeeds, with a separate analyzer regression checking the support boundary.
The three anonymous-enum declaration probes now pass because the analyzer creates
the declared storage. The negative `6.20.5--specparam_inv` probe now passes because
assigning a specparam to a value parameter is rejected, as required by 6.20.5;
an unreferenced specparam declaration remains accepted. No other expectation rows
are changed for these repairs.

The scalar floating-point implementation updates twenty reviewed expectation
rows: both state modes for `5.7.2-real-constants`, `5.7.2-real-token`,
`6.12--real`, `6.12--realtime`, `6.12--shortreal`, `6.24.1--cast_op`,
`20.5--itor`, `20.5--rtoi`, `20.5--real-bits-conv`, and
`20.5--shortreal-bits-conv` change from fail to pass. Full official runs at the
pinned revision report 472 passing cases in 2state and 474 in 4state, ten more
in each mode. No passing case regresses. The negative real-select and real-edge
cases retain their verdicts. The pinned `real_bit_select_idx` input is rejected
earlier for its undriven net, so a separate analyzer regression uses a driven
input to check that real-valued indices themselves are rejected.

The report contains each case’s state mode, verdict, selected execution mode,
tags, tool return code and path to the official log. The output directory retains
both the official log and runner diagnostics for each case and state mode.

Run the adapter contract fixtures against the same upstream checkout:

```bash
SV_TESTS_ROOT=../sv-tests PYTHONDONTWRITEBYTECODE=1 \
    python3 -m unittest discover -s conformance/sv-tests -p 'test_*.py' -v
```

These fixtures exercise the actual upstream runner with a controlled driver,
including mode priority, skips, negative tests, assertions, timeouts, library
injection and missing-environment failures. The Rust example is a single-case
compiler/simulator driver; it does not parse sv-tests metadata or judge verdicts.
