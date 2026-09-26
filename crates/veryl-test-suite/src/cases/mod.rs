// The cases use ordinary Rust assertions and a small setup/build DSL so the
// source and stimulus remain together. This macro is private to the corpus.
macro_rules! cases {
    (@expectation) => { crate::Expectation::Simulation };
    (@expectation reject) => { crate::Expectation::CompilationError };
    ($category:ident, $group:literal; $(
        fn $name:ident($sim:ident) {
            $(@setup { $($setup:tt)* })?
            $(@expect $expectation:ident;)?
            @build $design:expr;
            $($body:tt)*
        }
    )*) => {
        pub(super) const CASES: &[crate::TestCase] = &[$(
            crate::TestCase {
                name: concat!($group, "::", stringify!($name)),
                category: crate::Category::$category,
                expectation: cases!(@expectation $($expectation)?),
                run: |factory| {
                    $($($setup)*)?
                    let design = $design;
                    let compiled = factory(&design);
                    if cases!(@expectation $($expectation)?) == crate::Expectation::CompilationError {
                        assert!(compiled.is_err(), "invalid design was accepted: {}::{}", $group, stringify!($name));
                        return;
                    }
                    let backend = compiled.unwrap_or_else(|error| {
                        panic!("compile {}::{}: {error}", $group, stringify!($name))
                    });
                    #[allow(unused_mut, unused_variables)]
                    let mut $sim = crate::Simulator::new(backend);
                    $($body)*
                },
            },
        )*];
    };
}

use std::path::{Path, PathBuf};

fn std_source(parts: &[&str]) -> String {
    veryl_std::expand().expect("failed to expand veryl-std sources");
    let rel = parts.iter().collect::<PathBuf>();
    let paths = veryl_std::paths(Path::new("")).expect("failed to resolve veryl-std sources");
    let src = paths
        .iter()
        .find(|path| path.src.ends_with(&rel))
        .unwrap_or_else(|| panic!("veryl-std source not found: {}", rel.display()));
    std::fs::read_to_string(&src.src)
        .unwrap_or_else(|err| panic!("failed to read {}: {}", src.src.display(), err))
}

mod advanced_interface;
mod array_literal;
mod basic;
mod case_switch;
mod comb_observer;
mod compare_matrix;
mod concat_operators;
mod concatenation;
mod context_width;
mod counter;
mod data_access;
mod duplicate_varpath;
mod enum_type;
mod expression_semantics;
mod false_loop;
mod ff_event_snapshot;
mod fifo_issue5;
mod flip_flop;
mod for_loop_unroll;
mod four_state;
mod four_state_expression_semantics;
mod function_arguments;
mod generic_identity;
mod hierarchy;
mod interface;
mod issue3_repro;
mod linear_sorter_pull;
mod loop_idiom;
mod multi_clock;
mod nba_cross_block;
mod nba_cross_block_empty;
mod nba_dynamic_array;
mod operators;
mod param_override;
mod proto_package;
mod recovered_unrolled_fold;
mod reset_edge_cases;
mod self_determination;
mod shift_bug_test;
mod shift_signedness;
mod signed_divrem;
mod state_cast_semantics;
mod std_binary_codec;
mod std_delay;
mod std_edge_detector;
mod std_fifo;
mod std_gray_codec;
mod std_lfsr;
mod std_mux;
mod std_onehot;
mod std_ram;
mod struct_constructor;
mod synth_dynamic_loop;
mod system_function;
mod test_unimplemented_paths;
mod wide_context_width;
mod wide_data;
mod wide_operators;
mod wide_shift_mem;

pub(super) const GROUPS: &[&[crate::TestCase]] = &[
    advanced_interface::CASES,
    array_literal::CASES,
    basic::CASES,
    case_switch::CASES,
    comb_observer::CASES,
    compare_matrix::CASES,
    concat_operators::CASES,
    concatenation::CASES,
    context_width::CASES,
    counter::CASES,
    data_access::CASES,
    duplicate_varpath::CASES,
    enum_type::CASES,
    expression_semantics::CASES,
    false_loop::CASES,
    ff_event_snapshot::CASES,
    fifo_issue5::CASES,
    flip_flop::CASES,
    for_loop_unroll::CASES,
    four_state::CASES,
    four_state_expression_semantics::CASES,
    function_arguments::CASES,
    generic_identity::CASES,
    hierarchy::CASES,
    interface::CASES,
    issue3_repro::CASES,
    linear_sorter_pull::CASES,
    loop_idiom::CASES,
    multi_clock::CASES,
    nba_cross_block::CASES,
    nba_cross_block_empty::CASES,
    nba_dynamic_array::CASES,
    operators::CASES,
    param_override::CASES,
    proto_package::CASES,
    recovered_unrolled_fold::CASES,
    reset_edge_cases::CASES,
    self_determination::CASES,
    shift_bug_test::CASES,
    shift_signedness::CASES,
    signed_divrem::CASES,
    state_cast_semantics::CASES,
    std_binary_codec::CASES,
    std_delay::CASES,
    std_edge_detector::CASES,
    std_fifo::CASES,
    std_gray_codec::CASES,
    std_lfsr::CASES,
    std_mux::CASES,
    std_onehot::CASES,
    std_ram::CASES,
    struct_constructor::CASES,
    synth_dynamic_loop::CASES,
    system_function::CASES,
    test_unimplemented_paths::CASES,
    wide_context_width::CASES,
    wide_data::CASES,
    wide_operators::CASES,
    wide_shift_mem::CASES,
];
