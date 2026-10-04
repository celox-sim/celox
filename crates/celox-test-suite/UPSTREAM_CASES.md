# Cases reconstructed from Veryl

These cases were adapted from `veryl-lang/veryl` commit
[`ba7daead7d69f95988f25807795956bf89827b32`](https://github.com/veryl-lang/veryl/tree/ba7daead7d69f95988f25807795956bf89827b32),
using the local checkout at `/home/tsukasa/develop/veryl` on 2026-09-30.
The upstream checkout was read without changes. The adaptations remove
Veryl-specific IR, configuration, synthetic-event, and `Value` APIs, expose
results through ports, and add input transitions and boundary checks. They do
not depend on upstream files at test time.

The original compiler fixtures mainly check parsing and emission. The new
`veryl_language` cases add runtime inputs and independent numerical assertions.
The simulator regressions already have behavioral expectations; the new
`veryl_regressions` cases retain those and add further stimulus.

Source files at the pinned revision:

- [Simulator regressions](https://github.com/veryl-lang/veryl/blob/ba7daead7d69f95988f25807795956bf89827b32/crates/simulator/src/tests/simulation.rs)
- [Compiler fixtures](https://github.com/veryl-lang/veryl/tree/ba7daead7d69f95988f25807795956bf89827b32/testcases/veryl)

| Shared case (group omitted) | Upstream source | Added checks |
| --- | --- | --- |
| `wide_shift_amount_out_of_range` | simulator function of the same name | Counts 0, 1, 7, 8, 256, and 2^64+1; multiple payloads |
| `unary_minus_as_shift_amount` | simulator function of the same name | Zero and nonzero exponents; 10-bit modular negation |
| `struct_bit_field_rhs_no_spill` | simulator function of the same name | Equal/unequal operands and neighboring fields |
| `wide_struct_bit_field_rhs_no_spill` | `interp_wide_struct_bit_field_rhs_no_spill` | Equal/unequal operands; exact 200-bit packed value |
| `wide_ternary_narrow_branch_no_spill` | simulator function of the same name | Both branch transitions with zero, one, patterned, and all-one data |
| `nested_array_index_const_array` | simulator function of the same name | Repeated index transitions; both inner and outer reads |
| `inst_port_default_value_connected_not_folded` | simulator function of the same name | Connected and omitted input in distinct instances |
| `inlined_function_per_callsite_scratch_in_continuous_assign` | simulator function of the same name | Swapped inputs, zero, MSB, and all-one inputs; Rust `leading_zeros` oracle |
| `inside_outside_range_endpoints` | `32_inside_outside.veryl` | All 256 input values; constant half-open/inclusive ranges and runtime inclusive bounds in inside/outside and case, including empty and singleton ranges |
| `parameter_expression_type_cast_widths` | `94_cast_by_expression.veryl` | Truncation boundaries and subtraction wrap; Veryl 0.22 parenthesized expression casts checked against equivalent named logic types |
| `packed_union_members_alias` | `41_union.veryl` | All 256 values through an 8-bit overlay and two packed structure members |

The checked SystemVerilog requirements are IEEE 1800-2023 clause 11.4.10
(shift operators, unsigned shift count), 7.2.1 (packed structures, first member
most significant), 11.4.13 (set membership), and 6.24.1 (cast operator).
Veryl's half-open ranges are adapted separately from SV's inclusive ranges.
The expected values are calculated from those operations and the designs;
simulator agreement is additional evidence rather than the definition of
correctness.

## Celox failures exposed at introduction

The first run used Celox 0.8.2 with Veryl 0.21.0. These are existing implementation
failures uncovered by adding the cases. The stacked implementation fix now
initializes constant-array storage, saturates complete wide shift counts before
backend lowering, and preserves the final partial byte of Wasm stores (payload
and mask). The original shared assertions remain unchanged and the affected
correctness tests are enabled again. Run them normally to verify the repair:

```sh
cargo test -p celox --test veryl_upstream wide_shift_amount_out_of_range
cargo test -p celox --test veryl_upstream nested_array_index_const_array
cargo test -p celox --test veryl_upstream wide_struct_bit_field_rhs_no_spill
```

| Case | Affected Celox backends | First observed disagreement |
| --- | --- | --- |
| `wide_shift_amount_out_of_range` | native, Cranelift, Wasm, interpreter, SV frontend | `0xa5 >> (2^64+1)` returns `0x52`; expected `0` |
| `nested_array_index_const_array` | native, Cranelift, Wasm, interpreter | `A[1]` returns `0`; expected `3` (and the outer read must return `33`) |
| `wide_struct_bit_field_rhs_no_spill` | Wasm | Bits 100..103 of the first member are cleared: actual `0xffffffffffffffffffffffff07ffffffffffffffffffffffff`, expected `0xfffffffffffffffffffffffff7ffffffffffffffffffffffff` |

The SV frontend retains seven unsupported variants: set-membership assignment
expressions (`inside_outside_range_endpoints`), cast expressions
(`parameter_expression_type_cast_widths`,
`inlined_function_per_callsite_scratch_in_continuous_assign`), packed struct/union
types (`packed_union_members_alias`, `struct_bit_field_rhs_no_spill`,
`wide_struct_bit_field_rhs_no_spill`), and a constant-array assignment expression
(`nested_array_index_const_array`) are unsupported under tracking issue #64.
These variants remain explicitly ignored. The wide-count, unary shift, ternary,
and default-port cases run through the SV frontend. The new four-state constant
array/FF case is also excluded there because that frontend does not support its
constant-array expressions or four-state FF event signals.

The Veryl reference backend passes these three cases. External verification
results are retained in `verification/icarus.json` and `verification/verilator.json`.
The remaining SV feature exclusions do not change expectations in the shared
corpus or external runners and should be removed as those features are implemented.

The stacked fix adds two further portable regressions:

- `wide_shift_count_preserves_unknowns_and_sign_fill`: 70-bit counts applied to
  130-bit logical shifts and an 8-bit arithmetic shift, including X/Z in low and
  high count words. IEEE 1800-2023 11.4.10 requires an unknown shift result for
  any unknown bit in the count.
- `constant_arrays_initialize_comb_and_ff_reads`: dynamic reads from explicit,
  default-filled, multidimensional, and two-state constant arrays, with exact
  X/Z payload/mask transport in combinational and clocked outputs.

Both pass the four Celox execution backends and the Veryl reference backend.
The shift case also passes the SV frontend. The expanded Wasm backend test
checks partial stores of 65, 98, and 127 bits, payload/mask preservation, and
both two-state and four-state source registers.

## External verification of the reconstructed cases

On 2026-09-30, Verilator 5.052 passed all 11 new cases. Icarus 13.0 passed
nine; the following two are retained as `compile_error`, with the actual
diagnostics in its JSON report:

- `veryl_language::inside_outside_range_endpoints`: `"inside" expressions not supported yet`.
- `veryl_regressions::nested_array_index_const_array`: `unpacked array parameters are not supported yet`.

These are compilation blockers rather than assertion disagreements.
The new `constant_arrays_initialize_comb_and_ff_reads` case hits the same
unpacked-array parameter compilation blocker in Icarus; the new four-state
shift-count case passes Icarus. Verilator reports both additional four-state
cases unsupported. Thus three retained Icarus cases currently have compilation
failures. The Icarus runner continues to return a failure for these cases; no new external-runner
exclusion was added. Emitted SV and expected values were not changed to accommodate
the tool. The retained reports were incrementally extended with actual results;
the earlier corpus was not rerun.

## Attribution

Upstream Veryl is licensed under MIT or Apache-2.0. These adaptations use the
MIT license; its copyright notice and full permission text are retained in
[LICENSE-VERYL-MIT](LICENSE-VERYL-MIT).

Copyright (c) 2022 Naoya Hatta <dalance@gmail.com>.

## Additional width and signedness cases

The 14 `veryl_context_regressions` cases with upstream names are adapted from
`veryl` commit `d17ce955ef2990af3201ca9529159eca6248aa76`,
`crates/simulator/src/tests/simulation.rs`,
an inspected upstream revision.
They retain the Veryl sources, input vectors, and bit-pattern expectations;
`signed_struct_member_sign_extends` additionally checks that an explicit
full-width part-select remains unsigned. The two `runtime_*_context` cases
isolate the Celox corrections from upstream constant-folding failures and
exercise input transitions. The Celox harness marks unrepaired failures with
explicit reasons; the shared assertions remain executable by other adapters.
