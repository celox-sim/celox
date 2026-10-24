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

Static function storage support updates the two rows for
`13.4.2--function-static` from fail to pass in both state modes. The official
runner selects elaboration for this probe: its local accumulator now has
persistent storage rather than being rejected. Runtime retention is exercised
by the shared static-function simulation cases. The full pinned run matches
the reviewed expectations in both state modes.

Blocking latch support changes ten expectation rows: `12.4--if`,
`12.4.1--if_else_if`, and the three `12.4.2` priority/unique conditional probes
now pass elaboration in both state modes. These sources use ordinary
`always @*`, whose retained assignments are accepted by default. The unmodified
runner and default driver require no inference option; `always_comb` remains
strict by default. The `9.2.2.3--always_latch` probe still fails because it uses
an unsupported nonblocking latch assignment (`<=`). The full pinned run reviewed
all 2054 case/mode rows; no other expectation rows changed.

The integral file-input extension updates six expectation rows: `21.3--feof`,
`21.3--fflush` and `21.3--fpos` now pass simulation in both state modes. The
remaining file-input probes retain their measured verdicts; several still use
unsupported `final` constructs.

The six Annex N distribution probes (`20.15--dist_normal`, `dist_exponential`,
`dist_poisson`, `dist_chi_square`, `dist_t` and `dist_erlang`) now pass simulation
in both state modes, updating twelve more expectation rows. The scanner probes
retain failures caused by unsupported `final` constructs or internal data types;
no scanner expectation is changed.

The `string` implementation updates forty-two reviewed expectation rows: both
state modes for `5.13-builtin-methods-strings`, `6.16--string`, the sixteen
`6.16.*` method cases, `6.20.4--localparam_string`,
`11.4.12.2--string_concat_op` and `11.4.12.2--string_repl_op` change from fail
to pass. Each was simulated and its values checked, not only elaborated. Full
official runs at the pinned revision report 507 passing cases in 2state and 509
in 4state. No passing case regresses. The `12.7.3--foreach`, `12.7.4--while` and
`12.7.5--dowhile` cases keep failing: they use an unpacked array of strings,
which is rejected because its element and character selects are not yet told
apart.

The real system-function extension updates twenty expectation rows: `$realtime`
passes elaboration and the nine `20.8` probes for `atan2`, `ceil`, `exp`, `floor`,
`hypot`, `ln`, `log10`, `pow` and `sqrt` pass simulation in both state modes.
The full pinned run reviewed all 2,054 rows, with 496 passing cases in 2state
and 498 in 4state; no passing case regresses. The existing scanf probes retain
their verdicts because their other unsupported constructs remain unchanged.

Classes update fourteen reviewed expectation rows: both state modes for
`8.4--instantiation`, `8.5--properties`, `8.6--methods`, `8.12--assignment`,
`8.24--out_of_block_methods`, `8.26.5--implemented_class_handle` and
`6.20.6--const` change from fail to pass. The simulated cases were run and their
values checked, including the shared object behind two handles in
`8.12--assignment`. Full official runs at the pinned revision report 524 passing
cases in 2state and 526 in 4state. No passing case regresses: classes that are
declared but never constructed, interface classes and pure virtual methods are
not lowered. `8.7--constructor` keeps failing because it initializes an
implicitly static procedural variable without the `static` keyword, which IEEE
1800-2023 6.21 requires; `8.7--constructor_param` fails for the same reason.

Base constructors, `super`, static members and `C::` scope resolution update
eighteen more rows: both state modes for `8.7--constructor_super`,
`8.8--typed_constructor`, `8.9--static_properties`, `8.10--static_methods`,
`8.13--inheritance`, `8.14--override_member`, `8.15--super`,
`8.17--constructor_const_arg` and `8.23--scope_resolution` change from fail to
pass. Each was simulated and its displayed values checked, including the base
method that a base-class handle calls in `8.14--override_member`. Full official
runs at the pinned revision report 534 passing cases in 2state and 536 in
4state, with no passing case regressing. Virtual dispatch, abstract and
interface classes, classes in packages, parameterized classes and shallow copies
stay unsupported.

Virtual methods, abstract classes and classes implementing interface classes
update eighteen more rows: both state modes for `8.15--super-default-new`,
`8.20--virtual_method`, `8.21--abstract_class`, `8.22--dynamic_method_lookup`,
`8.26.2--implements`, `8.26.2--implements_extends`,
`8.26.2--implements_multiple`, `8.26.6.1--name_conflict_resolved` and
`8.26.7--partial_implementation` change from fail to pass. Each was simulated
and its displayed values checked: the override of the object's class runs
through base-class handles and an array of them, and the shadowed property of
`8.20--virtual_method` reads the derived class's value. Full official runs at
the pinned revision report 543 passing cases in 2state and 545 in 4state, with
no passing case regressing. `8.26.6.1--name_conflict_unresolved` stays a
passing negative case: its return types conflict, which is rejected. Calls
through interface class handles, `$cast`, parameterized classes and shallow
copies stay unsupported.

Interface class handles and `$cast` of class handles update two more rows: both
state modes for `8.26.5--cast_between_interface_classes` change from fail to
pass. The case was simulated with calls added after the cast, which reached the
object through both interface class handles. Full official runs at the pinned
revision report 544 passing cases in 2state and 546 in 4state, with no passing
case regressing: an interface class whose methods use its own typedefs or type
parameters, as in `8.26.3--type_access_extends` and
`8.26.6.2--parameter_type_conflict`, is still elaborated without being lowered.
`6.24.2--cast_fn`, `6.24.2--cast_task` and `8.16--cast_func` keep failing
because they cast to integral and enum variables.

Accepting undriven nets updates both rows for
`5.6.4--compiler-directives-pragma` from fail to pass. Its `protected_wire`
has no driver and now elaborates with the standard net default (IEEE 1800-2023
6.6 and 6.7.1). The official runner selects elaboration for this probe; runtime
high-impedance values are checked by the shared undriven-net simulation cases.
The full pinned CI run changes only these two verdicts across 2,054 mode runs.

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
