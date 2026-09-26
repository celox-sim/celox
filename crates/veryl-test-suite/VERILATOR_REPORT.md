# Verilator verification report

Historical report for the original 646-case corpus. The original machine-readable
results are retained in [verification/baseline/verilator.json](verification/baseline/verilator.json).
See [the current report](VERIFICATION_REPORT.md) for corrected expectations and
compilation-rejection cases.


This is the initial investigation. See the [combined verification report](VERIFICATION_REPORT.md)
for the retained Verilator/Icarus comparison and the current result taxonomy.

Verified on 2026-09-26 with Veryl 0.21.0 and Verilator 5.052 (2026-09-05,
rev v5.052), on x86_64 Linux. The corpus contains 646 extracted cases. Their
Veryl source, stimulus sequences, and assertions were preserved from Celox.
Celox-specific diagnostic text tests remain in Celox.

| Result | Cases |
| --- | ---: |
| Passed | 456 |
| Original assertion disagrees with Verilator | 9 |
| Veryl emission / SystemVerilog compilation failed | 74 |
| Unsupported four-state execution | 107 |
| Total | 646 |

This is not a claim that every expectation is correct. The nine disagreements
are reproducible observations, not an automatic verdict on which implementation
is wrong. The 74 compilation failures did not reach the assertions. No expected
values or runner exclusions were changed to make this verification pass.

## Reproduce

```sh
cargo run -p veryl-test-suite --features verilator --bin verify-verilator -- \
  --jobs 8 --output target/veryl-verilator
```

This command intentionally exits nonzero while discrepancies remain. Use
`--filter group::test_name` to reproduce a particular case. `results.json`
records each case, category, phase, result, and diagnostic. Each case directory
retains its Veryl input, emitted SystemVerilog, compiler log, protocol transcript,
and individual result. Each test asserts directly on values returned by
Verilator; no expected outputs are recorded from Celox.

The adapter stages all input writes before evaluating, including the first
batch; looks up clock/reset polarity from the analyzed top-level port types;
and restores the driven event level after a tick. Arrays are flattened with
element zero in the least-significant bits. X/Z cases are reported explicitly
as unsupported because of [Verilator's two-state execution](https://verilator.org/guide/latest/languages.html).

## Assertion differences

Values below are the first failing comparison: the suite may contain additional
checks that are not reached after that assertion.

| Case | Expected | Verilator |
| --- | ---: | ---: |
| `basic::test_always_comb_read_before_write_uses_previous_value` | 0 | 1 |
| `comb_observer::test_named_function_outputs_apply_in_source_order` | 1 | 2 |
| `expression_semantics::system_function_results_obey_ternary_width_contexts` | 255 | 4294967295 |
| `flip_flop::test_ff_function_call_nonvariable_argument_preserves_self_sized_overflow_before_coercion` | 0 | 1 |
| `flip_flop::test_ff_runtime_for_signed_inclusive_range_preserves_negative_bounds` | 1 | 3735928559 |
| `function_arguments::test_comb_statement_output_copyout_obeys_named_argument_order` | 1 | 2 |
| `hierarchy::test_instance_output_concat_advances_each_destination` | 2 | 3 |
| `signed_divrem::signed_divrem_i64` | 9223372036854775808 | 0 |
| `synth_dynamic_loop::test_runtime_bounds_signed_inclusive_range_preserves_negative_bounds` | 3 | 0 |

Investigation notes:

- `basic::test_always_comb_read_before_write_uses_previous_value` emits
  `c = b; b = a;`. Verilator reports `ALWCOMBORDER` and its generated C++ assigns
  `b` before `c`. IEEE 1800-2023, 9.2.2.2.1, *Implicit always_comb sensitivities*,
  excludes variables also written by the block from its inferred sensitivity.
  Preserve this case for evaluating scheduling differences; matching Verilator
  here would not by itself establish conformance.
- Both named-output argument cases pass the same destination to two output
  arguments in reversed name order. The corpus expects the source-order
  copy-out result; Verilator produces the formal-order result. This report does
  not resolve the Veryl language's intended aliasing rule.
- The `$bits` ternary case produces `0xffffffff` instead of `0xff` when the
  other arm is a signed eight-bit value. The emitted SV retains `$bits`.
  IEEE 1800-2023, 20.6.2, *Expression size system function*, specifies an
  `integer` result for `$bits`; the boundary between Veryl's expression typing
  and the emitted SV needs review before changing the expectation.
- The signed inclusive-loop cases emit a signed `int` counter compared with an
  unsigned port bound; negative starts do not iterate as the corpus expects.
- `signed_divrem_i64` first disagrees for `i64::MIN / -1`: Verilator returns
  zero, while the corpus expects the wrapped minimum bit pattern.
- The nonvariable function-argument case and instance-output concatenation
  case require further review of expression sizing and side-effect scheduling.

The IEEE clauses above were checked against the locally supplied 2023 edition.
They describe SystemVerilog; they do not independently define Veryl semantics.

## Cases blocked before execution

| Diagnostic family | Cases |
| --- | ---: |
| Veryl `FunctionOutputInAlwaysFf` | 33 |
| Veryl `SideEffectFunctionCallInAlwaysFf` | 14 |
| Veryl `UnevaluableValue` | 4 |
| Veryl `InvalidForRange` | 2 |
| Veryl emitter `unreachable` panic | 2 |
| Verilator rejects emitted SystemVerilog | 19 |

These are recorded as failures, not passing/skipped tests. In particular,
Celox's existing support for function effects in `always_ff` does not imply that
Veryl 0.21.0's emitter accepts those sources. The verifier does not suppress its
analyzer errors.

<details>
<summary>All 74 cases and their first compiler diagnostic</summary>

| Case | Diagnostic |
| --- | --- |
| `advanced_interface::test_transitive_generics` | syntax error, unexpected "'", expecting "'{" |
| `array_literal::test_array_literal_default_comb_assignment` | syntax error, unexpected default, expecting IDENTIFIER-for-type |
| `array_literal::test_array_literal_nested_default_multidim_assignment` | syntax error, unexpected default, expecting IDENTIFIER-for-type |
| `array_literal::test_array_literal_repeat_plus_default` | syntax error, unexpected default, expecting IDENTIFIER-for-type |
| `array_literal::test_array_literal_two_explicit_plus_default` | syntax error, unexpected default, expecting IDENTIFIER-for-type |
| `comb_observer::test_comb_function_array_literal_array_item_preserves_element_type` | EXTENDS unexpected in assignment to unpacked array |
| `comb_observer::test_comb_function_direct_array_argument_converts_each_element` | EXTENDS unexpected in assignment to unpacked array |
| `comb_observer::test_comb_function_packed_array_literal_preserves_source_order` | syntax error, unexpected '}', expecting ':' |
| `comb_observer::test_comb_statement_function_direct_array_argument_converts_each_element` | EXTENDS unexpected in assignment to unpacked array |
| `enum_type::test_enum_assign_and_compare` | Implicit conversion to enum 'enum{}Top.Color' from 'logic[1:0]' (IEEE 1800-2023 6.19.3) |
| `expression_semantics::wildcard_predicates_remain_one_bit_in_ternaries_and_concats` | Unsupported: RHS of ==? or !=? is fourstate but not a constant |
| `flip_flop::test_ff_assert_message_output_argument_is_eager` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_bits_and_size_array_dependencies_do_not_alias_later_write` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_bits_and_size_operands_do_not_alias_earlier_array_argument` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_constant_signed_bounds_in_unrolled_loops` | Veryl InvalidForRange |
| `flip_flop::test_ff_dynamic_nonlocal_store_follows_pending_whole_write` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_effectful_array_item_output_is_not_a_read_alias` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_branch_view_is_reused_after_merge` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_effect_is_eager_in_short_circuit_rhs` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_effect_is_eager_in_ternary_arm` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_snapshots_scalar_before_callee_write` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_snapshots_scalar_before_later_write` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_supports_dynamic_multidim_indexing` | syntax error, unexpected default, expecting IDENTIFIER-for-type |
| `flip_flop::test_ff_function_call_array_literal_view_preserves_expression_order` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_array_literal_view_preserves_source_order` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_bit_select_on_nonvariable_one_bit_formal` | Illegal bit or array select; type does not have a bit range, or bad dimension: data type is 'logic' |
| `flip_flop::test_ff_function_call_bits_and_size_evaluate_effectful_array_argument` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_carries_branch_local_static_array_item_cache` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_converts_array_literal_view_for_wider_nested_formal` | EXTEND unexpected in assignment to unpacked array |
| `flip_flop::test_ff_function_call_converts_forwarded_static_array_element` | EXTEND unexpected in assignment to unpacked array |
| `flip_flop::test_ff_function_call_dynamic_multidim_indexing_accepts_array_valued_items` | EXTEND unexpected in assignment to unpacked array |
| `flip_flop::test_ff_function_call_effectful_array_items_are_eager_before_conditional_access` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_expression_with_output_argument_and_return_value` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_keeps_array_view_active_for_output_index` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_merges_directly_forwarded_array_cache` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_merges_nested_array_state_at_cache_completion` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_merges_nested_array_state_at_static_cache_completion` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_merges_outer_array_view_across_nested_short_circuit` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_nested_bits_evaluates_effectful_array_argument` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_nested_output_statement_in_function_body` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_snapshots_pure_array_items_before_later_effect` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_statement_with_output_argument` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_statement_with_output_argument_and_return_value` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_tracks_array_reads_in_output_indices` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_tracks_array_view_hidden_in_bound_literal` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_tracks_nested_array_reads_in_output_indices` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_call_tracks_nested_static_array_read_through_branch` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_function_output_index_uses_final_nonlocal_state` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_nonlocal_write_precedes_aliased_formal_output_copyout` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_outputless_wrapper_direct_dynamic_nonlocal_assignment_is_observable` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_outputless_wrapper_dynamic_indexed_copyout_to_nonlocal_is_observable` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_outputless_wrapper_expression_copyout_to_nonlocal_is_observable` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_outputless_wrapper_indexed_copyout_to_nonlocal_is_observable` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_outputless_wrapper_nested_copyout_to_nonlocal_is_observable` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_pure_input_is_snapshotted_before_later_effectful_input` | Veryl FunctionOutputInAlwaysFf |
| `flip_flop::test_ff_runtime_function_snapshots_helper_input_before_callee_nonlocal_write` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_runtime_function_snapshots_input_before_callee_nonlocal_write` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_short_circuit_runtime_write_preserves_later_state_source` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_skipped_conditional_nonlocal_write_preserves_prior_ff_assignment` | Veryl SideEffectFunctionCallInAlwaysFf |
| `flip_flop::test_ff_statement_function_direct_nonlocal_assignment_is_observable` | Veryl SideEffectFunctionCallInAlwaysFf |
| `function_arguments::test_comb_inout_statement_copies_input_before_mutating_formal` | internal error: entered unreachable code |
| `function_arguments::test_ff_inout_expression_copyout_commits_with_nonblocking_assignments` | internal error: entered unreachable code |
| `function_arguments::test_ff_nested_output_copyout_is_visible_before_outer_copyout` | Veryl FunctionOutputInAlwaysFf |
| `function_arguments::test_ff_output_copyout_to_dynamic_slice_preserves_other_bits` | Veryl FunctionOutputInAlwaysFf |
| `function_arguments::test_ff_statement_output_copyout_freezes_all_inputs` | Veryl FunctionOutputInAlwaysFf |
| `hierarchy::test_dynamic_prefix_colon_output_port_allows_zero_lsb` | Illegal assignment: types are not assignment compatible (IEEE 1800-2023 7.6) |
| `operators::test_pow_operator_runtime_exponent_comb_and_ff` | Veryl FunctionOutputInAlwaysFf |
| `std_mux::test_demux_build_smoke` | Illegal output port connection 'o_data', mismatch between port which is not an array, and expression which is an array. (IEEE 1800-2023 7.6) |
| `std_mux::test_mux_build_smoke` | Illegal input port connection 'i_data', mismatch between port which is not an array, and expression which is an array. (IEEE 1800-2023 7.6) |
| `synth_dynamic_loop::test_constant_signed_bounds_in_unrolled_synth_loops` | Veryl InvalidForRange |
| `system_function::test_direct_comb_clog2_system_function` | Veryl UnevaluableValue |
| `system_function::test_direct_comb_onehot_system_function` | Veryl UnevaluableValue |
| `system_function::test_direct_ff_clog2_system_function` | Veryl UnevaluableValue |
| `system_function::test_direct_ff_onehot_system_function` | Veryl UnevaluableValue |

</details>

## Extraction validation

The SystemVerilog-enabled Celox test run passed 5,735 tests, with its 781
pre-existing backend/test exclusions retained. The crate's independent adapter
and emission tests, doctest, formatting, Clippy, and package verification also
passed. The default crate dependency graph contains no Celox, Veryl analyzer,
parser, or simulator. Compiler/emitter dependencies are opt-in features.
