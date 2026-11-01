//! The shared Veryl corpus on the baseline native pipeline.
//!
//! Tiered execution runs most of a short workload on its baseline image,
//! which uses lighter MIR optimization and the linear register allocator.
//! Ordinary native tests compile the optimizing image, so these tests run
//! whole corpus categories that stress allocation (wide values, shifts,
//! division, dynamic indexing, loops, multi-clock state) through the baseline
//! pipeline.

#![cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros, unused_imports, dead_code)]
mod test_utils;

/// Cases the category's own test file ignores on every backend.
const IGNORED: &[&str] =
    &["synth_dynamic_loop::test_constant_signed_bounds_in_unrolled_synth_loops"];

fn run_category(category: &str) {
    let prefix = format!("{category}::");
    let names = celox_test_suite::veryl::cases()
        .filter(|case| case.name.starts_with(&prefix) && !IGNORED.contains(&case.name))
        .map(|case| case.name)
        .collect::<Vec<_>>();
    assert!(!names.is_empty(), "no corpus cases in {category}");
    for name in names {
        test_utils::suite::run_case(name, "native-baseline");
    }
}

macro_rules! categories {
    ($($category:ident),* $(,)?) => {
        $(
            #[test]
            fn $category() {
                run_category(stringify!($category));
            }
        )*
    };
}

categories! {
    operators,
    wide_operators,
    wide_shift_mem,
    synth_dynamic_loop,
    signed_divrem,
    nba_dynamic_array,
    std_fifo,
    std_ram,
    multi_clock,
    case_switch,
    linear_sorter_pull,
    compare_matrix,
    four_state,
}
