# Expectation review and permitted behavior

Reviewed on 2026-09-26 against Veryl 0.21.0, Verilator 5.052, Icarus 13.0, and
IEEE 1800-2023. The initial 9 Verilator and 6 Icarus disagreements overlap in
four cases, leaving **11 distinct cases**. The original result files are retained
in [verification/baseline](verification/baseline/README.md). The current reports
include the corrections described below; the baseline observations in the case
mapping remain historical evidence.

| Finding | Cases | Treatment |
| --- | ---: | --- |
| Simulator behavior differs from the checked SV requirements | 3 | Retain the expectations; ignore only on the affected external tool, with upstream evidence |
| Veryl constant evaluation disagrees with its emitted SV | 2 | Require the SV result; retain failures and ignore affected Veryl/Celox variants until fixed |
| Test declaration or expectation conflicts with its stated intent | 3 | Fixed signed-bound fixtures, shift expectations, and Celox X/Z transport |
| Output copy-out order between formals is unspecified | 2 | Accept either order; verify named binding separately with distinct destinations |
| Emitted output connection is not legal SV | 1 | Reject at compilation; removed Celox dynamic-output extension |

The two signed-bound declarations and the X/Z expectation are now fixed. Celox
preserves Z in shifts and in a narrowed result, including 8/64/65/128/256-bit
regressions. Eight dynamic output connection cases now have
`Expectation::CompilationError`, retaining their stable IDs. Celox's Veryl
frontend rejects dynamic destinations; the former lowering extension has been
removed. The SV frontend rejects the supported forms, while earlier analyzer
limitations remain excluded. In particular, three indexed part-select cases
cannot validate the intended rejection; the [review regression checks](VERIFICATION_REPORT.md#review-regression-checks)
record their diagnostics and exclusions.

The external runners report ten reviewed conformance discrepancies as `ignored` by default:
nine for Verilator (including seven accepted invalid dynamic output connections)
and one for Icarus. Each exclusion records the specification basis, observed
version, and upstream evidence or retained local observations described below.
This is a runner policy, not permission for alternative SV results. The reusable
excluded cases still execute their original assertions, and `--include-ignored` makes an
external runner execute them too, reporting actual passes or failures. Original
observations remain in the baseline and standalone probe reports.
Additional compilation/execution limitations are reviewed separately in
[LIMITATIONS.md](LIMITATIONS.md); they do not change the conformance verdicts here.

## What may vary, and what may not

The distinction below is based on the checked 2023 clauses, not on a majority vote
between simulators. These are **SV** requirements; the Veryl inconsistencies
remain separate questions.

| Issue | Required result or remaining latitude | Suite treatment |
| --- | --- | --- |
| Read before write in `always_comb` | `0,0,1` for this stimulus; re-reading the freshly written `b` is not an allowed alternative (9.3.1, 9.2.2.2.1) | Retain the expectation; ignore on Verilator |
| Addition into a 9-bit input port | `0x100`; zero is not an allowed alternative (10.8, 11.8.2) | Retain the expectation; ignore on Icarus |
| Signed 64-bit `MIN / -1` | The 64-bit representation of the quotient is `MIN` (6.9.1, 11.4.3, 11.6.1) | Retain the expectation; ignore on Verilator |
| `$bits` with a signed ternary arm | `0xffffffff` (20.6.2, 11.8.1); Veryl constant evaluation gives `0xff` | Correct the shared expectation; expose the Veryl/Celox error |
| Addition passed to a 4-bit formal | Bit 2 = `1` (10.8, 11.8.2); Veryl constant evaluation gives `0` | Correct the shared expectation; expose the Veryl/Celox error |
| Two output formals aliasing one actual | Copy-out is blocking (4.9.7); 13.5/13.5.1/13.5.4 do not prescribe source order or formal order | Accept either complete output value; also check distinct destinations exactly |
| Negative bounds | With explicitly signed bounds: three iterations, sum zero, last value one | Corrected declarations |
| X/Z shifts | Retained Z stays Z; any X/Z in the count makes all result bits X (11.4.10) | Corrected expectations and implementation |
| Dynamic output destinations | Invalid implicit continuous-assignment destination (10.2, 23.3.3.2) | Require compilation rejection, never a numeric result |

For output aliases, reversing two whole-variable blocking copy-outs yields
**1 or 2** in the constant probe, and **`x+1` or `x+2` modulo 256** in the
parameterized probe. Neither relative copy-out order is prescribed, so both
orders satisfy the checked requirements. The shared tests now allow those
values. This is unspecified ordering, not permission to produce arbitrary bits:
each whole-variable blocking assignment copies a complete output value.
Input-expression evaluation order is explicitly undefined in 13.5, but that
sentence is not the basis for this copy-out interpretation. Both shared tests
also verify named binding with distinct destinations and exact expected values.

Clause 6.9.1 explicitly requires vector arithmetic modulo 2^n. Its exemption
from *detecting* integer overflow does not license arbitrary result bits.
Together with division (11.4.3) and the 64-bit expression width (11.6.1), this
requires the minimum bit pattern for `MIN / -1`. No C/C++ signed-overflow rule
is imported into the SV verdict; tool observations are corroborating evidence.

## Complete case mapping

“V” and “I” below mean Verilator and Icarus. Values are the first failing
comparison in the original case. Full independent observations are retained in
[results.json](verification/repros/mismatches/results.json).

| Case | Original expectation | Observation | Finding |
| --- | --- | --- | --- |
| `basic::test_always_comb_read_before_write_uses_previous_value` | `0` after the first rising input transition | V `1`, I `0` | Verilator scheduling |
| `comb_observer::test_named_function_outputs_apply_in_source_order` | `1` | V `2`; I rejects output function arguments | Either copy-out order now accepted |
| `expression_semantics::system_function_results_obey_ternary_width_contexts` | `0x000000ff` | V/I `0xffffffff` | Veryl IR/SV signedness inconsistency |
| `flip_flop::test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion` | `0` | V/I `1` | Veryl IR/SV argument-width inconsistency |
| `flip_flop::test_ff_runtime_for_signed_inclusive_range_preserves_negative_bounds` | last value `1` | V/I `0xdeadbeef` | Unsigned fixture used as a negative bound |
| `four_state::test_four_state_wide_shifts` | masked bits all become X | I preserves alternating X/Z; V is unsupported | Wrong Z-to-X expectation |
| `function_arguments::test_comb_statement_output_copyout_obeys_named_argument_order` | named result `1` | V `2`; I rejects output function arguments | Either copy-out order now accepted |
| `hierarchy::test_instance_input_port_assignment_width_context` | `0x100` | V `0x100`, I `0` | Icarus port-expression sizing |
| `hierarchy::test_instance_output_concat_advances_each_destination` | memory `2` | V `3`; I rejects nonconstant output bit-select | Illegal SV connection |
| `signed_divrem::signed_divrem_i64` | `0x8000000000000000` | V `0`, I expected value | Verilator signed-division overflow guard |
| `synth_dynamic_loop::test_runtime_bounds_signed_inclusive_range_preserves_negative_bounds` | 3 iterations | V/I 0 iterations | Unsigned fixture used as a negative bound |

## 1. Read before write: retain the expectation

For `c = b; b = a;`, `b` is excluded from the inferred sensitivity because the
same block writes it. Starting with zero storage, `a = 0, 1, 0` should expose
`c = 0, 0, 1`, rather than the newly assigned `b` in each activation. See IEEE
1800-2023, 9.3.1 (*Sequential blocks*) and 9.2.2.2.1
(*Implicit always_comb sensitivities*).

The [standalone probe](verification/repros/mismatches/read_before_write.sv)
returns `0,0,1` in Icarus and `0,1,0` in Verilator. Verilator behaves the same
with and without `--public-flat-rw`. This rules out our VPI input staging and
public-signal setting as the cause. The probe uses `bit b` solely to match the
suite's zero-initialized two-state storage.

Verilator's own [ALWCOMBORDER documentation](https://verilator.org/guide/latest/warnings.html#alwcomborder)
also describes the previous-value behavior of a read before a write.

In [#4052 (2023), Wilson Snyder](https://github.com/verilator/verilator/issues/4052#issuecomment-1476997115)
explained that Verilator prioritizes speed on well-designed combinational logic
over exact IEEE behavior; that issue was closed as not planned. The later
[#7350 report](https://github.com/verilator/verilator/issues/7350) matches this
read-before-write pattern. Its merged [#7608 change (2026)](https://github.com/verilator/verilator/pull/7608)
improves diagnostics and documentation, without changing execution to preserve
the previous value. The suite therefore ignores this case on Verilator 5.052's
runner by default while retaining the required result.

## 2. `$bits` and function-argument sizing: require the SV results

**Celox follow-up (Veryl 0.21.0):** The runtime `$bits`/`$size` ternary fixture
now passes native, Cranelift, Wasm, and interpreter. Celox derives their signedness
from their signed integer results and variable signedness from its intrinsic type,
rather than the analyzer's propagated sibling context. Those four ignores are
removed. Forced checks still find an unresolved-system-function internal error
in the Veryl 0.21.0 reference simulator and an unsupported
`always_comb` assignment expression in the SV frontend, so those two variants
remain ignored.

This fixes expressions whose operators survive in AIR. It does **not** repair
the all-constant probe below: Veryl folds those expressions before Celox receives
them. The constant function-argument case likewise still reaches Celox as the
incorrect literal `0`, so its exclusions remain. The original observations below
are historical and describe the state before this Celox follow-up.

The shared fixture covers `$bits` and `$size` with both a signed 8-bit arm
(required sign extension to 32 bits) and an unsigned 40-bit arm, in combinational
and clocked assignments. Both branch selections are checked.

Related upstream work, checked on 2026-09-28:

- [PR #3410](https://github.com/veryl-lang/veryl/pull/3410) fixes element widths,
  `$bits` on unpacked arrays, and `$size` returning the leading dimension.
- [Issue #2555](https://github.com/veryl-lang/veryl/issues/2555), resolved by
  [PR #3449](https://github.com/veryl-lang/veryl/pull/3449), concerns the optional
  dimension argument of `$size`.
- [PR #2804](https://github.com/veryl-lang/veryl/pull/2804) fixes extension of
  ternaries whose arms are both signed.

These are merged but do not resolve this query-result signedness discrepancy.
On upstream master `1a6c00adb3589d4d6a25e4e75508d5ed1dc91e8d`, the constant
probe `if 1'b0 ? $size(logic<5>) : SIGNED_VALUE` still evaluates to `0xff`
instead of `0xffffffff`, just like the `$bits` probe. No issue or PR directly
addressing this return-signedness mismatch was found during this investigation.

Both disagreements can be demonstrated using constants in a single
[Veryl module](verification/repros/mismatches/constant_context.veryl), so neither
clock scheduling nor the Rust/VPI adapters are involved.

| Expression | Veryl analyzer's constant value | Emitted SV in V/I |
| --- | --- | --- |
| `if 1'b0 ? $bits(logic<5>) : signed_8bit_ff` | `0x000000ff` | `0xffffffff` |
| `f(2'b11 + 2'b01)`, with 4-bit formal and `return x[2]` | `0` | `1` |

[Analyzer observations](verification/repros/mismatches/analyzer-values.txt)
and the [emitted SV with a testbench](verification/repros/mismatches/constant_context.sv)
are retained. The analyzer source was compared with its cached crates.io archive;
the relevant files matched, and the archive checksum matches `Cargo.lock`:
`27642985fd83ff90aeea356cbc70f63d2ac83988e1e1bae9e7b0bd63fe61fcf8`.
Its source revision is `2e32d53f056eef323225a91079af92574d18f726`.

For `$bits`, `veryl-analyzer`'s `src/ir/system_function.rs`,
`SystemFunctionCall::eval_value`, constructs a 32-bit **unsigned** `Value`.
The emitter preserves the SV `$bits` call, whose result is a signed `integer`
(IEEE 1800-2023, 20.6.2). Combining that with a signed eight-bit arm sign-extends
the selected `0xff`. The [control probe](verification/repros/mismatches/expression_context.sv)
shows that explicitly applying `$unsigned($bits(...))` restores `0x000000ff`
in both simulators. This locates the discrepancy at the signedness boundary.

For function arguments, an SV subroutine input is an assignment-like context
(10.8). Its width propagates to context-determined operands before evaluation
(11.8.2). The addition is therefore `4'b0011 + 4'b0001`, and bit 2 is one.
Putting the addition inside a concatenation creates a self-determined expression
(11.6.1): `f({2'b11 + 2'b01})` returns zero in both tools. The Veryl analyzer
constant result is already zero, so this is not merely a Celox backend failure.

The [Veryl function-call reference](https://doc.veryl-lang.org/book/05_language_reference/04_expression/02_function_call.html)
permits SV system functions, but does not establish a differing argument-width
or `$bits` signedness rule. The [type-inference reference](https://doc.veryl-lang.org/book/05_language_reference/19_type_inference.html)
explicitly discusses dependence on SV expression-width rules. An upstream
implementation fix is still needed. The shared tests now require the checked
SV results: `0xffffffff` for the signed ternary arm and `1` for bit 2 of the
widened addition. This is a specification-based oracle, not agreement with a
particular implementation. Celox and the Veryl reference simulator execute
these same assertions when explicitly rerun. Their 11 failing backend variants
are ignored by default while the implementation fixes are deferred; the
failure observations and reasons remain recorded. The passing Celox SV
function-width variant and both external simulators remain enabled. The Veryl
reference tests remain discoverable as ignored tests, rather than being omitted.
The misleading legacy function-width case ID is retained only for compatibility.

The [retained focused results](verification/repros/veryl_celox_failures.json)
separate incorrect results from unsupported execution:

| Implementation | `$bits` signed ternary (required `0xffffffff`) | Function argument (required `1`) |
| --- | --- | --- |
| Celox Veryl frontend: native, Cranelift, Wasm, interpreter | Mismatch: `0xff` | Mismatch: `0` |
| Veryl 0.21.0 reference simulator | Internal error: unresolved system function | Mismatch: `0` |
| Celox SV frontend | Unsupported `always_comb` assignment expression | Pass |
| Verilator / Icarus | Pass | Pass |

Both source designs are legal simulation fixtures. The shared expectations
remain unchanged when a backend is ignored; a forced run still reports an
incorrect result, runtime panic, or unsupported construct as a failure.
The retained report records execution before the ignores were added.
Reproduce those observations explicitly with:

```sh
cargo test --locked -p celox --features systemverilog \
  --test expression_semantics system_function_results_obey_ternary_width_contexts \
  -- --include-ignored
cargo test --locked -p celox --features systemverilog \
  --test flip_flop test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion \
  -- --include-ignored
```

## 3. Signed inclusive bounds: fix the declarations for the intended test

Both tests originally declared `start` and `count` as **unsigned** `logic<32>`, then supply
`0xffffffff` intending it to mean `-1`. The emitter produces an `int` loop
counter but leaves `i <= count` mixed-signedness. The comparison is unsigned
(IEEE 1800-2023, 11.8.1), so the loop does not execute: the FF output keeps its
sentinel and the combinational iteration count remains zero.

The [control probe](verification/repros/mismatches/signed_bounds.sv) changes the
bounds to signed 32-bit signals while keeping the same input bit patterns. Both
tools then produce **3 iterations, sum 0, last value 1**, matching the original
intent. The fixtures now declare `start/count: input signed logic<32>`. Their expected
values and input bit patterns are unchanged.

This follows the documented [Veryl `signed` type modifier](https://doc.veryl-lang.org/book/05_language_reference/03_data_type/01_builtin_type.html);
we found no documented blanket signed conversion for every `for` range.

## 4. Wide shift: the expected Z-to-X conversion is wrong

The stimulus uses `(payload, mask) = (0xaa, 0xff)` in bits 71:64. With the suite's
encoding, that is **`xzxzxzxz`**, not eight X bits. The original test then ORed the mask into
the expected payload, demanding `xxxxxxxx`, even for a shift by zero.

The [probe](verification/repros/mismatches/shift_xz.sv) shows Icarus preserving
`xzxzxzxz` both at shift zero and after shifting it down by 64. Veryl's own
constant evaluator also preserves `8'bxzxzxzxz >> 0` (the `SHIFT_CONTEXT` entry
in the analyzer observations). IEEE 1800-2023, 11.4.10 (*Shift operators*), moves
operand bits and defines fill bits; it does not replace a retained Z bit by X.

For the current mixed-X/Z stimulus, the corrected payloads are the original
`(0xaa << 64) | 0x55` for shift zero and `0xaa` for shift 64; the masks stay
unchanged. These expectations are now corrected. The added
`four_state::test_four_state_shifts_preserve_xz` exercises logical left/right
shifts, constant and runtime counts, narrowing, word boundaries, and both X/Z
counts with a known-one bit. Celox's compiled backends and interpreter now
preserve the payload instead of ORing every mask bit into it. Four-state slices
use the mask-preserving SIR slice operation rather than bitwise AND.

Verilator's `constant_shift` field in the mixed constant probe is diagnostic
output from a two-state simulator and is not evidence about X/Z conformance.

## 5. Named output aliases: source order is not a portable oracle

Both tests connect two output formals to the same destination, then reverse
the named argument order. Celox's `ordered_function_outputs` deliberately walks
`FunctionCall::outputs` in source order, so its final value is one. Verilator
copies results in formal order and produces two for positional, forward-named,
and reverse-named calls, in both returning and void functions.

Icarus cannot execute these output functions. A separate
[task control](verification/repros/mismatches/task_output_alias.sv), using the
same copy-out binding pattern, is accepted by **both** tools and produces two
for all three call orders. Distinct destinations produce first=1, second=2,
confirming that the names bind correctly. Task agreement corroborates the
copy-order finding; it is not direct execution of the rejected functions.

IEEE 1800-2023, 4.9.7 specifies blocking copy-out. Clauses 13.5, 13.5.1, and
13.5.4 describe argument transfer and named binding without prescribing the
relative order of the output copies. The shared tests therefore accept either
complete output value for aliased actuals, independently for positional and
named calls. Separate destinations must still receive first=1, second=2 (or
`x+1`, `x+2` modulo 256). Legacy case IDs are retained for compatibility.

Celox's Veryl frontend emits `unspecified_output_copy_order` when different
output formals have statically overlapping destinations. It checks whole
variables, packed slices, concatenations, and constant array elements, including
calls nested in expressions and functions. The diagnostic points to the call
and recommends distinct output destinations followed by explicit assignments.
Disjoint destinations and input/output aliasing alone do not trigger it;
runtime indices are not assumed to alias. The warning does not reject the call
or change Celox's current copy-out order.

## 6. Instance input sizing: retain `0x100`

The [integer probe](verification/repros/integer_edges.sv) connects eight-bit
`0xff + 1` to a nine-bit input port. Verilator returns `0x100`. Icarus warns that
it is padding an eight-bit result and returns zero. Port connections are
assignment-like contexts (IEEE 1800-2023, 10.8 and 23.3.3), and 11.8.2 propagates
the nine-bit context before the addition. This identifies an Icarus sizing
limitation; the test's ninth carry bit should be retained.

Icarus's [input-port elaboration comment](https://github.com/steveicarus/iverilog/blob/3a28970ee1d9dc9717b52de58cdc19a30a5c5510/elaborate.cc#L1938-L1953)
says it evaluates the expression at its self-determined width so that it can
warn about port-width mismatches. This explains the observed implementation.
No direct upstream response or decision on this exact carry-loss case was
found. The Icarus exclusion records our observed limitation, not an upstream
admission or a claim that it is permitted by the standard. The runner already
uses `-gstrict-expr-width`; the separate documented treatment of unsized numbers
does not explain these explicitly sized operands.

## 7. Dynamic instance output selection: compilation error

The connection `.o({mem[tmp], tmp})` contains a nonconstant bit selection on the
sink side. A variable output port connection implies a continuous assignment
(23.3.3 and 23.3.3.2). Table 10-1 in 10.2 permits **constant** packed bit-selects
on the left side of a continuous assignment.

Icarus's rejection is therefore relevant, rather than just an implementation
missing a legal construct. Verilator accepts the
[standalone probe](verification/repros/mismatches/output_concat.sv) and reaches
`mem=3`; this value cannot validate a portable expectation for this connection.
The case now requires compilation rejection. The seven other dynamic-output
cases have the same contract. Celox's Veryl and SV dynamic-output lowering paths
were removed, with no opt-in extension left behind. Runtime observer tests for
these invalid connections also check diagnostics now. Constant destinations,
including valid concatenations and array slices, retain their simulation tests.

The external runner reports `rejected` separately from a numerical `passed`, and
`unexpected_accept` when a compiler accepts the invalid design. Infrastructure
failure, compiler timeout, and emitter panic do not count as successful rejection.
A rejection reports the actual compiler diagnostic; it does not prove that a tool
supports every other construct in the fixture.

Verilator 5.052 accepts seven of these eight negative cases. Its runner now
ignores those seven by default, retaining the compilation-error expectations.
The [pre-exclusion results](verification/repros/dynamic_output_acceptance.json)
record each `unexpected_accept`; no direct upstream decision on these cases
was found. `--include-ignored` rechecks them normally. The eighth case, which
Verilator already rejects, remains enabled. Icarus's eight rejection checks and
Celox's checks remain enabled too.

## 8. Signed 64-bit division: retain the wrapped minimum

For `0x8000000000000000 / -1`, Icarus and Celox preserve the minimum bit pattern;
Verilator returns zero. The [integer probe](verification/repros/integer_edges.sv)
uses plusarg inputs, preventing replacement by a constant-folded expression.

The installed Verilator 5.052 `include/verilated_funcs.h`, `VL_DIVS_QQQ`, has an
explicit early return of zero for exactly this operand pair to avoid a host
signed-division trap. This directly explains the observation and rules out the
VPI adapter or Veryl emission as its cause. Ordinary integer division and
expression width are specified in IEEE 1800-2023, 11.4.3 and 11.6.1; 6.9.1
requires vector arithmetic modulo 2^n.

In [#2460 (2020), Wilson Snyder](https://github.com/verilator/verilator/issues/2460#issuecomment-656970666)
explicitly says the overflow exception was fixed by returning zero. The
discussion supplies no SV rule permitting that result. This intentional host
exception workaround is the documented reason for the Verilator exclusion;
the expected 64-bit quotient is unchanged.

## Reproduce

From the repository root, with both simulators on `PATH`:

```sh
cargo run -p celox-test-suite --all-features --example inspect-mismatch-ir -- \
  crates/celox-test-suite/verification/repros/mismatches/constant_context.sv
python3 crates/celox-test-suite/scripts/reproduce-mismatches.py
```

The Rust example prints analyzer values and regenerates the SV constant probe.
The Python script records **observations**, not pass/fail conformance verdicts,
for 18 tool/probe combinations. It exits nonzero because Icarus rejects the two
function-output and nonconstant-port probes. Their compiler diagnostics remain
in the report. Sixteen probes execute; no execution error or timeout occurred.
The default artifact directory is `/tmp/veryl-mismatch-probes`; the portable JSON
report is retained beside the probe sources.

## Veryl width and signedness regressions

Checked on 2026-10-02 with Celox 0.8.2 and its pinned Veryl 0.21.0. The
14 upstream probes exposed seven failing cases on native, Cranelift, Wasm,
and the SIR interpreter. The first failing output in each case was:

| Case (`veryl_context_regressions::` omitted) | Output | Expected | Observed | Diagnosis |
| --- | --- | --- | --- | --- |
| `signed_struct_member_sign_extends` | `y0` | `0xfffe` | `0x00fe` | Celox treats a rebased member read as an unsigned select |
| `case_compares_each_label_as_an_if_does` | `y1` | `2` | `0` | Celox computes an 8-bit target before the 9-bit label comparison |
| `runtime_for_bound_keeps_its_type` | `y0` | `0` | `4` | Veryl's `ForBound::Const(usize)` erases the unsigned upper bound type |
| `constant_ternary_keeps_both_arm_types` | `t` | `4` | `12` | Veryl folds/clears an arm before preserving its selected type |
| `signed_cast_of_folded_constant_sign_extends` | `y1` | `0xfb` | `0x0b` | Folded AIR already contains the zero-extended result |
| `constant_case_on_signed_target` | `y0` | `1` | `2` | Veryl elaboration evaluates a negative case label incorrectly |
| `folded_const_select_keeps_its_sign` | `y4` | `0xfffb` | `0x00fb` | Veryl folds a signed constant array element into an unsigned value |

IEEE 1800-2023 11.8.1 (Rules for expression types) and 11.8.2 (Steps for
evaluating an expression) distinguish unsigned explicit part-selects from
signed operands and propagate a comparison's common width into its operands
before evaluating arithmetic. The oracles retain those distinctions.
Verilator passes all original cases except the constant-function range case,
which it cannot compile; Icarus passes ten and cannot compile four.

Celox now recovers whole packed-member signedness from the retained member
path, while leaving explicit bit/part-selects unsigned. Pure context-determined
case targets are evaluated using each comparison's width and signedness against
a frozen symbolic store; targets containing calls retain their single evaluation.
Dynamic loop-bound arithmetic is evaluated at no less than the emitted
32-bit `int` width, preserving wider bounds. The two added isolated cases
exercise runtime case targets in statements, expressions, and functions and
forward/reverse/inclusive loop bounds at the 8-bit carry boundary. The case
probe additionally checks that function copy-out occurs once. Both probes pass
Verilator; Icarus passes the loop probe but cannot compile the function output
argument in the case probe. The packed-member case passes both tools.

The dependency version is unchanged. Six original cases remain ignored in
Celox's harness because their residual failures require information already
lost in Veryl AIR. In particular, fixing the runtime target makes the combined
case probe advance to `y2` (expected `1`, observed `0`), where the constant
label was evaluated before the unsigned target context was known. The runtime
loop probe still stops at `y0`; the isolated bound-arithmetic probe passes.
Veryl-only failures are excluded separately, including a negative-bound loop
that failed to terminate during the reference run and was stopped after more
than 60 seconds. The reusable shared cases keep their expected values.

Reproduce the remaining failures with:

```sh
cargo test -p celox --test veryl_context_regressions -- --include-ignored \
    --skip runtime_for_with_negative_bound::veryl
```

The optional Celox SystemVerilog frontend was checked separately. Eight of the
new enabled cases encounter its existing unsupported constructs (casts,
procedural loops, member assignments, or dynamic partial-array writes).
`runtime_case_target_uses_comparison_context` also reproduces its own target
truncation (`y0 = 0`, expected `2`). These SV frontend variants are explicitly
excluded; this patch repairs the Veryl frontend, not the separate SV lowering.
The 128-bit folded-shift case passes through the SV frontend.

Rechecked on 2026-10-05 with the pinned Veryl 0.22.0. The two cases that still
failed on native, Cranelift, Wasm, and the SIR interpreter were Celox defects,
not information lost in Veryl AIR:

- `folded_const_select_keeps_its_sign` read `y5 = Q[i]` as `0`: a dynamic read
  of a parameter array loads its state, and only constant arrays had their
  elements placed in the initial state. Its `y9` (`LS.m[3:0] + a`) read `0`
  for the same reason: the analyzer leaves a select of a local constant
  struct's member unfolded. Constants and parameters now all keep their values
  in the initial state.
- `case_compares_each_label_as_an_if_does` stopped at `y7`
  (`case J + 8'h01 { 16'h0100: .. }`): the analyzer folds a constant case
  target in its own 8-bit width, before the 16-bit label comparison is known,
  and Celox extended that folded `0`. Case statements now recompute a target's
  context-determined operators in each comparison's width, as for a runtime
  target, in combinational, `always_ff`, and function bodies. Case expressions
  were already folded per comparison by the analyzer.

Both cases are enabled on all four Celox backends and the SV frontend. Three
added cases isolate the repaired paths:
`dynamic_param_array_read_uses_its_elements` (default and overridden parameter
arrays in comb and ff), `constant_case_target_uses_comparison_context`
(widening, narrowing, shifted, and range labels in comb, ff, and a function), and
`unfolded_constant_struct_member_reads_its_value`. All three pass Verilator.
Icarus cannot compile them: it rejects unpacked array parameters, the emitted
`case ... inside`, and the package-typed local parameter respectively. The SV
frontend runs the last two; it rejects the unpacked array parameter.
