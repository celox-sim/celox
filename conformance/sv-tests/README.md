# sv-tests conformance

CI runs the unmodified [sv-tests `tools/runner`](https://github.com/chipsalliance/sv-tests/blob/db159ad8e9cbbd62036569d240153f753258f640/tools/runner)
at the commit in [`revision`](revision), using the Celox
[`BaseRunner` adapter](runners/celox.py). [`run.py`](run.py) launches that official
runner in a subprocess for every `.sv` file, as the upstream Makefile does.
It compares the official verdicts with [`expected.tsv`](expected.tsv).
Each test has separate `2state` and `4state` rows (`test`, `state_mode`, `result`).

The corpus matches the public dashboard's **main tests** at
[build 40150201340](https://github.com/chipsalliance/sv-tests/actions/runs/40150201340)
(2026-10-25, revision `db159ad8e9cbbd62036569d240153f753258f640`): **1,650 cases
per state mode**, comprising 1,028 checked-in cases and 622 generated cases.
[`prepare.py`](prepare.py) runs upstream's unmodified `template_generator`,
`easyUVM`, and `uvm-examples` generators. It verifies the pinned checkout and
library submodules, generates into a fresh temporary tree, checks the reviewed
counts in [`corpus.json`](corpus.json), and rejects stale or foreign generated
cases. CI prepares this corpus before collecting results. The dashboard's
`cores` and `imported` groups are separate corpora and are not included here.

The TSV report has a companion `.summary.json` containing the tested revision,
per-group/per-state pass totals, per-tag results, fully passing tag totals, and execution-mode
counts. As in the upstream dashboard, skipped cases are excluded from the
executed-case denominator, and a tag passes only when all its executed cases
pass. Aggregate tags (including `uvm-req`) are expanded using upstream
`conf/meta-tags.conf`, in the same order as `tools/sv-report`. Each case counts once in the overall total even when it has multiple tags.
States and groups are never combined into one percentage. A tool that only
parses a case is not demonstrating the same behavior as one that simulates it;
compare the corpus revision, selected cases, and execution modes alongside
percentages. This measures test outcomes, not a percentage of the language spec.

The official runner owns metadata parsing, compatible-runner filtering, mode
priority, library injection, timeouts, expected failures and simulation
`:assert:` evaluation. The adapter declares only the driver’s supported
`preprocessing`, `parsing`, `elaboration` and `simulation` modes. It forwards
source files, include directories, defines, an optional explicit top and the
selected state mode to
the Rust driver. Preprocessing mode expands macros and includes, emits the
resulting text, and stops before syntax parsing or root selection. Parsing mode
stops after preprocessing and syntax parsing; it does not build semantic module
metadata, select a root, elaborate or simulate.
The official mode priority still chooses elaboration for default cases and for
cases that permit both parsing and elaboration. In elaboration and simulation
modes, sources without a module still undergo preprocessing and parsing; root
detection then reports the actual no-root diagnostic. The current
compiler and simulator require a module root, so the driver reports a failure for
rootless unit, package and interface inputs. The official runner still decides
the case verdict, including expected failures. There is no `no_top` verdict
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

All 90 preprocessing cases at the pinned revision now execute in both state
modes, leaving no skipped expectation rows. Each mode reports 90 passes,
including `chapter-22/22.11--pragma-invalid.sv`: a bare `` `pragma `` is rejected
because its name is required by IEEE 1800-2023 22.11. Upstream preprocessing
verdicts check process success and expected failure; they do not compare the
emitted text against a golden file.

Built-in process handles update eight expectations: both state modes of
`9.7--process_cls_await`, `9.7--process_cls_kill`, `9.7--process_cls_self`,
and `9.7--process_cls_suspend_resume` now pass. CI reports only these four
fail-to-pass transitions per mode; the same sources also pass the local
simulation driver in both modes. No failure expectation is broadened.

Static task locals that keep their value update two expectations: both state
modes of `13.3.1--task-static` now pass. Its `int a = 0;` in a static task is
initialized once and shared by the four calls, which print 1, 2, 3 and 4 in
the local simulation driver; the full run reports no other change.

The official runner’s process exit code is separate from the case verdict.
The wrapper reads its `PASS`/`FAIL` log messages; it does not reconstruct their
rules from tool exit codes or `should_fail` metadata. A negative test may pass
because of an unsupported language construct rather than its intended error.
Crashes and timeouts retain the official runner’s verdict and tool return code.

A runner infrastructure error, missing driver or unreviewed missing
source/library/include directory aborts collection with exit code 2 and cannot
update expectations. One upstream input defect is retained explicitly in
[`input-issues.json`](input-issues.json): `uvm_alu_config_analysis` has a string
instead of a source-file list and a combined include-directory entry. For that
case, exact missing include path, and pinned revision only, the adapter forwards
the original input to the driver, which reports the missing include and fails.
It remains in the dashboard-compatible denominator, with an `input_issue` report
column and `upstream_input_issues` summary count; this is not a language-support
failure. No generated source or metadata is repaired, and negative tests cannot
use this exception to gain a false pass.
The UVM submodules named by upstream `conf/runners/libs.json` must be available
before any test runs. For simulation cases, each `simvariants` entry is forwarded
as one plusarg
to a fresh simulation, in upstream order. A failed run stops the driver; logs
from successful runs still reach the official assertion evaluator. The official
runner timeout covers the complete invocation, including all variants.
Elaboration/parsing cases do not execute variants.

Parameterized classes update fourteen more rows: both state modes for
`8.5--parameters`, `8.8--typed_constructor_param`,
`8.25--parametrized_class_extend`, `8.25.1--parametrized_class_scope_resolution`
and `8.26.3--type_access_implements` change from fail to pass. Each was
simulated and its displayed values checked, including the parameter that
`par_cls#()::b` reads and the base constructor argument passed by name in
`8.8--typed_constructor_param`. The negative
`8.26.6.2--parameter_type_conflict_unresolved` and
`8.26.6.3--diamond_relationship_parametrized` probes now fail for the reason
they test: an interface class inherits `T` from two different interface
classes or specializations without declaring its own. Full official runs at the
pinned revision report 1080 passing cases in 2state and 1091 in 4state, with no
passing case regressing; `8.25.1--parametrized_class_invalid_scope_resolution`
stays a passing negative case because `par_cls::b` outside the class is
rejected. `8.5--properties_enum` keeps failing because enumeration constants
declared in a class are not supported.

Shallow copies update two more rows: both state modes for `8.12--shallow_copy`
change from fail to pass. The case was simulated and its displayed values
checked: the property of the copy keeps the value it was copied with after the
method call changes the original. Full official runs at the pinned revision
report 1081 passing cases in 2state and 1092 in 4state, with no passing case
regressing.

Classes of packages and of the compilation unit update 62 more rows: both
state modes of 31 chapter 18 cases that declare classes outside any module
change from fail to pass, as such sources now elaborate their compilation unit.
Seven declare no constraints: `18.4.1--rand-modifier`,
`18.12--randomization-of-scope-variables_0`, `18.13.1--urandom_0`,
`18.13.1--urandom_2`, `18.13.2--urandom_range_0` and
`18.15--manually-seeding-randomize_0` were each simulated with a module that
constructs the class and calls its methods or `randomize()`, and printed the
expected results; `18.4.2--randc-modifier` elaborates while `randomize()` of
its `randc` property is rejected. The other 24 pass by elaboration alone:
their constraint blocks are parsed but not checked, and `randomize()` of a
class with constraints is rejected. `18.5.8.1--foreach-iterative-constraints_0`,
`18.5.8.2--array-reduction-iterative-constraints_0` and
`18.6.3--behavior-of-randomization-methods_0` keep failing on their array and
static random properties. Full official runs at the pinned revision report 1112
passing cases in 2state and 1123 in 4state, with no passing case regressing.

Dynamic container support updates 22 rows: both state modes of eleven chapter 7
cases change from fail to pass. Eight cases execute simulations and their
`:assert:` output passes: associative assignment, `exists`, `first`, `last`,
`next`, `prev`, traversal with a byte key, and dynamic-array `new[]`. The traversal
case declares both its key and output as `byte`; the stored key is already
truncated before traversal, so returning 1 is correct. The integral-key and
string-key associative declaration cases and `queues/basic` pass elaboration
only; they do not establish runtime behavior. Queue mutations and other container
behavior are exercised by the shared container simulation cases.
The [reviewed CI run](https://github.com/celox-sim/celox/actions/runs/40323383639)
contains 3,300 case/mode verdicts at the pinned revision. These are its only
changed rows, with no passing case regressing: 1,201 cases pass in 2state and
1,212 in 4state. Only these reviewed expectations are updated.

Zero-argument dynamic-container methods accept omitted parentheses, including
`size`, associative `num`, `delete`, and queue `pop_front` / `pop_back`
(7.5.2–7.5.3, 7.9, 7.10.2). Sixteen unmodified upstream simulation cases pass
the official runner in both state modes at the pinned revision: dynamic
`op-delete` / `op-size`, associative `alloc`, `nonexistent`, `delete`, `num`,
and `size`, and queue
`bounded`, `delete`, `insert`, `max-size`, `pop_back`, `pop_front`, `push_back`,
`push_front` and `size`. Only these 32 expectations change from `fail` to `pass`.
The focused runner checked 36 pinned case files containing these bare
method names (72 case/mode verdicts), with no passing candidate regressing.
This does not establish full dynamic-container conformance.
Shared regressions additionally cover discarded pop results, string/class
handles, four-state values and empty-pop defaults, and unevaluated type queries
that preserve packed result dimensions and authored bounds. The packed-result
query regression failed on all five Celox backend configurations even after
the initial 28 upstream checks passed. Arity and void-result rejection remain checked.
Queue slices, queue operands in concatenations, element-reference persistence,
warning diagnostics, and queries on nonintegral pop results remain outside
this change's support contract.
Logical equivalence (`<->`) now passes six generated cases in both state modes.
The binary-operator case passes elaboration; the five logical-operator cases
execute simulations and pass their assertions. A full run before the container
method changes above changed only these twelve verdicts, with no passing case
regressing: 1,208 cases pass in 2state and 1,219 in 4state. Shared tests cover unknown operands and
evaluate both operands exactly once (IEEE 1800-2023 11.4.7).

Logical implication (`->`) now passes six generated cases in both state modes.
The binary-operator case passes elaboration; the five logical-operator cases
execute simulations and pass their assertions. A full run before the container
method changes above changed only these twelve verdicts, with no passing case
regressing: 1,208 cases pass in 2state and 1,219 in 4state. Shared tests also cover short-circuiting and
unknown operands (IEEE 1800-2023 11.4.7).

Integral array reductions (7.12.3) support `sum`, `product`, `and`, `or` and
`xor`, with or without empty parentheses. The five unmodified
`chapter-7/arrays/unpacked/reduction-methods` simulation cases pass the
official runner in both state modes at the pinned revision. Only those ten
reviewed expectations change from `fail` to `pass`; this is a focused run,
not a new full-corpus measurement. Shared regressions also exercise fixed
arrays, queues, integral- and string-indexed associative arrays, element-width
overflow, signed results, packed type queries and four-state propagation.
Empty reductions use the operation identity (`product`: one, `and`: all ones,
`sum` / `or` / `xor`: zero). An explicit `legacy_empty_reductions` build option
selects zero for all empty reductions. These empty-input cases are covered by
shared and simulator API regressions; the five upstream cases use nonempty
arrays and do not distinguish these modes.
`with` expressions, iterator arguments and
multidimensional unpacked receivers remain unsupported.

Dynamic arrays and queues support `reverse`, `sort` and `rsort` (7.12.2),
including the bare method form. The three unmodified upstream
`chapter-7/arrays/unpacked/ordering-methods` cases pass the official simulation
runner in both state modes. Exactly these six expectations change from `fail`
to `pass`; the neighboring `shuffle` case remains a failure in both modes.
This is a focused eight-verdict run, not a full-corpus measurement.
Shared cases additionally check negative and unsigned integral values through
64 bits, string ordering, bounded queues, empty/singleton arrays, local
shadowing, value copies, class identity, X/Z preservation and scalar element
change events. Native-image/checkpoint tests cover the new runtime operations.
Associative ordering is prohibited by 7.12.2; fixed-array ordering, `shuffle`,
`with` expressions and iterator arguments remain unsupported.

Array locator methods (7.12.1) update 22 more rows: both state modes of the
ten `chapter-7/arrays/associative/locator-methods` cases and of
`chapter-7/arrays/unpacked/index.sv` (`item.index`, 7.12.4) change from fail
to pass. Each was simulated in both modes and its displayed values checked
against 7.12.1, including the first occurrence that `unique_index` keeps.
Full official runs at the pinned revision report 1277 passing cases in 2state
and 1288 in 4state, and change only these 22 verdicts.

Increment and decrement operators in expressions (11.4.2) update 4 more rows:
both state modes of `chapter-11/11.3.6--assign_in_expression.sv` and
`11.3.6--assign_in_expression-sim.sv`, which assign `(++a)`, change from fail
to pass. Each was simulated in both modes and leaves `a` and `b` at 1. Full
official runs at the pinned revision report 1283 passing cases in 2state and
1294 in 4state, and change only these 4 verdicts.

Streaming concatenations of fixed-size data (11.4.14) update 22 more rows:
both state modes of the `chapter-11/11.4.14.1` to `11.4.14.3` pack, re-order,
unpack and padding cases and of `simple/11.4.14.3--simple_unpack_stream-sim.sv`
change from fail to pass. Each was simulated in both modes: the `-sim` cases
assert their values, and the bit-reversed `{<<{a, b, c}}` of the other
`unpack_stream` cases was checked against 11.4.14.2 in 96 and, left-aligned,
128 bits. `11.4.14.3--unpack_stream_inv.sv` still fails to compile, as it must:
its 96-bit stream is wider than the 32-bit target. The `11.4.14.4` cases stream
dynamic arrays and queues, which remain unsupported. Full official runs at the
pinned revision report 1294 passing cases in 2state and 1305 in 4state, and
change only these 22 verdicts.

Streaming concatenations of dynamically sized data (11.4.14.4) update 6 more
rows: both state modes of `11.4.14.4--dynamic_array_stream.sv`,
`dynamic_array_stream-sim.sv` and `dynamic_array_stream_with.sv` change from
fail to pass. Each was simulated in both modes and unpacks the packet's header,
length, payload and checksum as they were packed, greedily and with
`with [0 +: o_len]`. Full official runs at the pinned revision report 1297
passing cases in 2state and 1308 in 4state, and change only these 6 verdicts.

## Running locally

Use the locked development shell and install the pinned [`requirements.txt`](requirements.txt) in a Python environment.

```bash
python3 -m venv work/sv-tests-venv
work/sv-tests-venv/bin/pip install -r conformance/sv-tests/requirements.txt
git clone https://github.com/chipsalliance/sv-tests.git ../sv-tests
git -C ../sv-tests checkout "$(cat conformance/sv-tests/revision)"
git -C ../sv-tests submodule update --init --depth 1 \
    third_party/tests/uvm third_party/tests/uvm-1.2 \
    third_party/tests/easyUVM third_party/tests/uvm-examples
work/sv-tests-venv/bin/python conformance/sv-tests/prepare.py ../sv-tests
cargo build --locked --release -p celox --features systemverilog --example sv_tests
PYTHONDONTWRITEBYTECODE=1 work/sv-tests-venv/bin/python conformance/sv-tests/run.py ../sv-tests \
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

`--shard INDEX/COUNT` runs and compares only one of COUNT disjoint shards
(1-based). Tests are dealt round-robin from their sorted names, so that
neighbouring tests, such as the UVM testbenches, spread over the shards; a test
that `expected.tsv` lists but the corpus lost belongs to one shard, which
reports it missing. With `--update`, a shard replaces only its own rows. CI
builds the driver once and runs three shards (`sv-tests conformance (N/3)`),
each with 4 jobs, and retains one report per shard.

Both state modes run by default. Use `--state-mode 2state` or
`--state-mode 4state` to run and compare only one; `--state-mode both` explicitly
selects the default. After reviewing changed verdicts, add `--update` to rewrite
expectations. A single-mode update preserves the other mode’s rows. Missing
cases and changed results are checked independently for each selected mode.
Update `revision`, the reviewed counts/provenance in `corpus.json`, and the
measured expectations together when upgrading upstream.

## Historical baseline reviews

The notes below describe earlier changes to the 1,027-case, checked-in-only
corpus at `c4229f3bd5220e6d3ba8f390e5d09c87e462e9c7`. Counts and support
boundaries are snapshots from those changes, not the current generated-corpus
totals. Use `expected.tsv` and the current run summary for present results.

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

Tagged packed union support changes `chapter-7/unions/tagged/packed.sv` from
fail to pass in both state modes. The official runner selects simulation: the
probe switches the active member with two tagged constructors and checks the
packed value with its `:assert:` output. Both executions return zero and pass
the assertion. The [reviewed full CI run](https://github.com/celox-sim/celox/actions/runs/40084353934)
contains 2054 case/mode verdicts; these are the only two changed rows, and all
verdicts match the updated expectations.

Accepting undriven nets updates both rows for
`5.6.4--compiler-directives-pragma` from fail to pass. Its `protected_wire`
has no driver and now elaborates with the standard net default (IEEE 1800-2023
6.6 and 6.7.1). The official runner selects elaboration for this probe; runtime
high-impedance values are checked by the shared undriven-net simulation cases.
The [reviewed CI run](https://github.com/celox-sim/celox/actions/runs/40112270471)
changes only these two verdicts across 2,054 mode runs.

## Reports and contract tests

The report contains each case’s state mode, verdict, selected execution mode,
tags, tool return code and path to the official log. The output directory retains
both the official log and runner diagnostics for each case and state mode.

Run the adapter contract fixtures against the same upstream checkout:

```bash
SV_TESTS_ROOT=../sv-tests PYTHONDONTWRITEBYTECODE=1 \
    work/sv-tests-venv/bin/python -m unittest discover -s conformance/sv-tests -p 'test_*.py' -v
```

These fixtures exercise the actual upstream runner with a controlled driver,
including mode priority, skips, negative tests, assertions, timeouts, library
injection and missing-environment failures. The Rust example is a single-case
compiler/simulator driver; it does not parse sv-tests metadata or judge verdicts.

## Failure triage

`run.py` now writes four companion artifacts:

- `report.tsv`: official verdicts plus `failure_category`, `diagnostic`, a bounded
  diagnostic excerpt (`evidence`), and `negative_test` (`1`, `0`, or empty when unknown).
- `report.summary.json`: schema version 2, collection completeness, requested and
  collected row counts, per-group/per-state totals, failure counts and frequent
  diagnostics. `classification_version` identifies the diagnostic rules.
- `report.summary.md`: a readable overview, also attached to the CI job summary.
- `report.failures.json`: every failing case with its classification and log path,
  plus collection errors. No case is removed from the TSV or denominator.

Categories describe **observed diagnostics**, not proven compiler root causes:

| Category | Evidence |
| --- | --- |
| `unsupported` | explicit unsupported-construct or single-module-root diagnostic |
| `compile_error` | parse-error or illegal-context diagnostic, even if the selected mode was simulation |
| `runtime_error` | `$fatal` or lost-runtime-event diagnostic |
| `assertion_failure` | official simulation output validation failed after a successful positive invocation |
| `unexpected_success` | a negative test's driver succeeded |
| `timeout` | upstream timeout return code **and** timeout diagnostic |
| `crash` | Rust panic diagnostic or abnormal/signal return code |
| `input_issue` | the reviewed upstream input defect described above |
| `environment` | missing include directory without the reviewed input exception |
| `unclassified` | unknown diagnostic, missing log, or TSV/log metadata disagreement |

For example, a parser error on a valid but unimplemented grammar construct is
still `compile_error`; the classifier does not guess whether the source or the
parser is wrong. A nonzero return code in simulation mode alone does not establish
a runtime bug because compilation may have failed first. Frequent diagnostic
counts normalize source paths and line numbers; `.failures.json` retains the
individual evidence and log locations. Tag counts still overlap as described above.

Negative-test passes accompanied by unsupported, root-requirement, timeout or
crash diagnostics appear in `negative_pass_diagnostics` for review. They remain
**official passes**, not new failures. No assertion expression is evaluated by
this classification code.

Missing drivers, missing libraries, runner exceptions and other collection
errors still return exit code 2 and prohibit `--update`. A failed collection now
retains already completed rows and separate `collection_errors`, with
`complete: false`; cancelled/unrun cases do not become invented failure rows.
An incomplete report must not be used as a full conformance score. Successful
collection has `complete: true` even when comparison with expectations returns 1.

Reclassify a downloaded or older **run report**, without rerunning simulations or
changing `expected.tsv`:

```bash
python conformance/sv-tests/reporting.py work/sv-tests-report.tsv \
  --log-root work/sv-tests-results \
  --meta-tags ../sv-tests/conf/meta-tags.conf \
  --output work/triage/report.tsv
```

`--log-root` relocates logs using `STATE/TEST/official.log`. Otherwise the paths
in the TSV are used (relative paths resolve against the TSV directory).
`--meta-tags` should reference the matching pinned checkout; omit it to count
only literal tags, reported as `meta_tags_expanded: false`. The existing summary
supplies the corpus revision and incomplete-collection provenance. Without a
completeness marker in that summary, offline reports use `complete: null`
(unknown), not a claim that every case ran. Missing logs stay unclassified.
