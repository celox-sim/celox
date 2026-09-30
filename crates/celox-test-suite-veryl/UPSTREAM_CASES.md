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
| `inside_outside_range_endpoints` | `32_inside_outside.veryl` | All 256 input values; single value, half-open and inclusive ranges |
| `parameter_expression_type_cast_widths` | `94_cast_by_expression.veryl` | Truncation boundaries and subtraction wrap; named logic types replace expression-cast syntax unavailable in Veryl 0.21 |
| `packed_union_members_alias` | `41_union.veryl` | All 256 values through an 8-bit overlay and two packed structure members |

The checked SystemVerilog requirements are IEEE 1800-2023 clause 11.4.10
(shift operators, unsigned shift count), 7.2.1 (packed structures, first member
most significant), 11.4.13 (set membership), and 6.24.1 (cast operator).
Veryl's half-open ranges are adapted separately from SV's inclusive ranges.
The expected values are calculated from those operations and the designs;
simulator agreement is additional evidence rather than the definition of
correctness.

## Celox failures exposed by the new cases

The first run used Celox 0.8.2 with Veryl 0.21.0. These are existing implementation
failures uncovered by adding the cases; this test-suite change does not alter
compiler or runtime implementation. The shared assertions remain strict.
`crates/celox/tests/veryl_upstream.rs` marks only the failing backend variants
as ignored. Run the matching test with `--ignored` to reproduce a failure:

```sh
cargo test -p celox --test veryl_upstream wide_shift_amount_out_of_range -- --ignored
cargo test -p celox --test veryl_upstream nested_array_index_const_array -- --ignored
cargo test -p celox --test veryl_upstream wide_struct_bit_field_rhs_no_spill -- --ignored
```

| Case | Affected Celox backends | First observed disagreement |
| --- | --- | --- |
| `wide_shift_amount_out_of_range` | native, Cranelift, Wasm, interpreter, SV frontend | `0xa5 >> (2^64+1)` returns `0x52`; expected `0` |
| `nested_array_index_const_array` | native, Cranelift, Wasm, interpreter | `A[1]` returns `0`; expected `3` (and the outer read must return `33`) |
| `wide_struct_bit_field_rhs_no_spill` | Wasm | Bits 100..103 of the first member are cleared: actual `0xffffffffffffffffffffffff07ffffffffffffffffffffffff`, expected `0xfffffffffffffffffffffffff7ffffffffffffffffffffffff` |

The SV frontend has eight failing variants: set-membership assignment
expressions (`inside_outside_range_endpoints`), cast expressions
(`parameter_expression_type_cast_widths`,
`inlined_function_per_callsite_scratch_in_continuous_assign`), packed struct/union
types (`packed_union_members_alias`, `struct_bit_field_rhs_no_spill`,
`wide_struct_bit_field_rhs_no_spill`), and a constant-array assignment expression
(`nested_array_index_const_array`) are unsupported under tracking issue #64;
`wide_shift_amount_out_of_range` reaches the same runtime disagreement as the
other Celox backends. These SV variants are also explicitly ignored. The unary
shift, ternary, and default-port cases continue to run through the SV frontend.

The Veryl reference backend passes these three cases. External verification
results are retained in `verification/icarus.json` and `verification/verilator.json`.
These Celox exclusions are not exclusions in the shared corpus or external
runners, and must be removed when their implementations are corrected.

## External verification of the reconstructed cases

On 2026-09-30, Verilator 5.052 passed all 11 new cases. Icarus 13.0 passed
nine; the following two are retained as `compile_error`, with the actual
diagnostics in its JSON report:

- `veryl_language::inside_outside_range_endpoints`: `"inside" expressions not supported yet`.
- `veryl_regressions::nested_array_index_const_array`: `unpacked array parameters are not supported yet`.

These are compilation blockers rather than assertion disagreements. The Icarus
runner continues to return a failure for these cases; no new external-runner
exclusion was added. Emitted SV and expected values were not changed to accommodate
the tool. The retained reports were incrementally extended with actual results;
the earlier corpus was not rerun.

## Attribution

Upstream Veryl is licensed under MIT or Apache-2.0. These adaptations use the
MIT license; its copyright notice and full permission text are retained in
[LICENSE-VERYL-MIT](LICENSE-VERYL-MIT).

Copyright (c) 2022 Naoya Hatta <dalance@gmail.com>.
