// Every group is a script file in the language of `crate::script`.

pub(super) struct Group {
    /// The file name, for diagnostics.
    pub file: &'static str,
    pub text: &'static str,
}

pub(super) const GROUPS: &[Group] = &[
    Group {
        file: "src/veryl/cases/partial_word_shift.vtest",
        text: include_str!("partial_word_shift.vtest"),
    },
    Group {
        file: "src/veryl/cases/concurrent_initial.vtest",
        text: include_str!("concurrent_initial.vtest"),
    },
    Group {
        file: "src/veryl/cases/hierarchical_assignment.vtest",
        text: include_str!("hierarchical_assignment.vtest"),
    },
    Group {
        file: "src/veryl/cases/advanced_interface.vtest",
        text: include_str!("advanced_interface.vtest"),
    },
    Group {
        file: "src/veryl/cases/array_literal.vtest",
        text: include_str!("array_literal.vtest"),
    },
    Group {
        file: "src/veryl/cases/basic.vtest",
        text: include_str!("basic.vtest"),
    },
    Group {
        file: "src/veryl/cases/case_switch.vtest",
        text: include_str!("case_switch.vtest"),
    },
    Group {
        file: "src/veryl/cases/comb_observer.vtest",
        text: include_str!("comb_observer.vtest"),
    },
    Group {
        file: "src/veryl/cases/compare_matrix.vtest",
        text: include_str!("compare_matrix.vtest"),
    },
    Group {
        file: "src/veryl/cases/concat_operators.vtest",
        text: include_str!("concat_operators.vtest"),
    },
    Group {
        file: "src/veryl/cases/concatenation.vtest",
        text: include_str!("concatenation.vtest"),
    },
    Group {
        file: "src/veryl/cases/context_width.vtest",
        text: include_str!("context_width.vtest"),
    },
    Group {
        file: "src/veryl/cases/counter.vtest",
        text: include_str!("counter.vtest"),
    },
    Group {
        file: "src/veryl/cases/data_access.vtest",
        text: include_str!("data_access.vtest"),
    },
    Group {
        file: "src/veryl/cases/duplicate_varpath.vtest",
        text: include_str!("duplicate_varpath.vtest"),
    },
    Group {
        file: "src/veryl/cases/enum_type.vtest",
        text: include_str!("enum_type.vtest"),
    },
    Group {
        file: "src/veryl/cases/expression_semantics.vtest",
        text: include_str!("expression_semantics.vtest"),
    },
    Group {
        file: "src/veryl/cases/false_loop.vtest",
        text: include_str!("false_loop.vtest"),
    },
    Group {
        file: "src/veryl/cases/ff_event_snapshot.vtest",
        text: include_str!("ff_event_snapshot.vtest"),
    },
    Group {
        file: "src/veryl/cases/ff_narrow_arrays.vtest",
        text: include_str!("ff_narrow_arrays.vtest"),
    },
    Group {
        file: "src/veryl/cases/fifo_issue5.vtest",
        text: include_str!("fifo_issue5.vtest"),
    },
    Group {
        file: "src/veryl/cases/flip_flop.vtest",
        text: include_str!("flip_flop.vtest"),
    },
    Group {
        file: "src/veryl/cases/for_loop_unroll.vtest",
        text: include_str!("for_loop_unroll.vtest"),
    },
    Group {
        file: "src/veryl/cases/four_state.vtest",
        text: include_str!("four_state.vtest"),
    },
    Group {
        file: "src/veryl/cases/four_state_expression_semantics.vtest",
        text: include_str!("four_state_expression_semantics.vtest"),
    },
    Group {
        file: "src/veryl/cases/function_arguments.vtest",
        text: include_str!("function_arguments.vtest"),
    },
    Group {
        file: "src/veryl/cases/function_bodies.vtest",
        text: include_str!("function_bodies.vtest"),
    },
    Group {
        file: "src/veryl/cases/generic_identity.vtest",
        text: include_str!("generic_identity.vtest"),
    },
    Group {
        file: "src/veryl/cases/hierarchy.vtest",
        text: include_str!("hierarchy.vtest"),
    },
    Group {
        file: "src/veryl/cases/interface.vtest",
        text: include_str!("interface.vtest"),
    },
    Group {
        file: "src/veryl/cases/issue3_repro.vtest",
        text: include_str!("issue3_repro.vtest"),
    },
    Group {
        file: "src/veryl/cases/linear_sorter_pull.vtest",
        text: include_str!("linear_sorter_pull.vtest"),
    },
    Group {
        file: "src/veryl/cases/loop_idiom.vtest",
        text: include_str!("loop_idiom.vtest"),
    },
    Group {
        file: "src/veryl/cases/multi_clock.vtest",
        text: include_str!("multi_clock.vtest"),
    },
    Group {
        file: "src/veryl/cases/nba_cross_block.vtest",
        text: include_str!("nba_cross_block.vtest"),
    },
    Group {
        file: "src/veryl/cases/nba_cross_block_empty.vtest",
        text: include_str!("nba_cross_block_empty.vtest"),
    },
    Group {
        file: "src/veryl/cases/nba_dynamic_array.vtest",
        text: include_str!("nba_dynamic_array.vtest"),
    },
    Group {
        file: "src/veryl/cases/operators.vtest",
        text: include_str!("operators.vtest"),
    },
    Group {
        file: "src/veryl/cases/packed_scatter_store.vtest",
        text: include_str!("packed_scatter_store.vtest"),
    },
    Group {
        file: "src/veryl/cases/param_override.vtest",
        text: include_str!("param_override.vtest"),
    },
    Group {
        file: "src/veryl/cases/proto_package.vtest",
        text: include_str!("proto_package.vtest"),
    },
    Group {
        file: "src/veryl/cases/recovered_unrolled_fold.vtest",
        text: include_str!("recovered_unrolled_fold.vtest"),
    },
    Group {
        file: "src/veryl/cases/reset_edge_cases.vtest",
        text: include_str!("reset_edge_cases.vtest"),
    },
    Group {
        file: "src/veryl/cases/self_determination.vtest",
        text: include_str!("self_determination.vtest"),
    },
    Group {
        file: "src/veryl/cases/shift_bug_test.vtest",
        text: include_str!("shift_bug_test.vtest"),
    },
    Group {
        file: "src/veryl/cases/shift_signedness.vtest",
        text: include_str!("shift_signedness.vtest"),
    },
    Group {
        file: "src/veryl/cases/signed_divrem.vtest",
        text: include_str!("signed_divrem.vtest"),
    },
    Group {
        file: "src/veryl/cases/state_cast_semantics.vtest",
        text: include_str!("state_cast_semantics.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_binary_codec.vtest",
        text: include_str!("std_binary_codec.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_delay.vtest",
        text: include_str!("std_delay.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_edge_detector.vtest",
        text: include_str!("std_edge_detector.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_fifo.vtest",
        text: include_str!("std_fifo.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_gray_codec.vtest",
        text: include_str!("std_gray_codec.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_lfsr.vtest",
        text: include_str!("std_lfsr.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_mux.vtest",
        text: include_str!("std_mux.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_onehot.vtest",
        text: include_str!("std_onehot.vtest"),
    },
    Group {
        file: "src/veryl/cases/std_ram.vtest",
        text: include_str!("std_ram.vtest"),
    },
    Group {
        file: "src/veryl/cases/struct_constructor.vtest",
        text: include_str!("struct_constructor.vtest"),
    },
    Group {
        file: "src/veryl/cases/synth_dynamic_loop.vtest",
        text: include_str!("synth_dynamic_loop.vtest"),
    },
    Group {
        file: "src/veryl/cases/system_function.vtest",
        text: include_str!("system_function.vtest"),
    },
    Group {
        file: "src/veryl/cases/test_unimplemented_paths.vtest",
        text: include_str!("test_unimplemented_paths.vtest"),
    },
    Group {
        file: "src/veryl/cases/veryl_context_regressions.vtest",
        text: include_str!("veryl_context_regressions.vtest"),
    },
    Group {
        file: "src/veryl/cases/veryl_language.vtest",
        text: include_str!("veryl_language.vtest"),
    },
    Group {
        file: "src/veryl/cases/veryl_regressions.vtest",
        text: include_str!("veryl_regressions.vtest"),
    },
    Group {
        file: "src/veryl/cases/wide_context_width.vtest",
        text: include_str!("wide_context_width.vtest"),
    },
    Group {
        file: "src/veryl/cases/wide_data.vtest",
        text: include_str!("wide_data.vtest"),
    },
    Group {
        file: "src/veryl/cases/wide_operators.vtest",
        text: include_str!("wide_operators.vtest"),
    },
    Group {
        file: "src/veryl/cases/wide_shift_mem.vtest",
        text: include_str!("wide_shift_mem.vtest"),
    },
];
