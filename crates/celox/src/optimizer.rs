pub use celox_sir_opt::{OptLevel, OptimizeOptions, SirPass};

pub mod coalescing;

pub fn optimize(
    program: &mut crate::ir::Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
) {
    optimize_impl(program, four_state, optimize_options, false);
}

pub(crate) fn optimize_preserving_element_storage(
    program: &mut crate::ir::Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
) {
    optimize_impl(program, four_state, optimize_options, true);
}

fn optimize_impl(
    program: &mut crate::ir::Program,
    four_state: bool,
    optimize_options: &OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    with_optimization_program(program, |unit| {
        celox_sir_opt::optimize(
            unit,
            four_state,
            optimize_options,
            preserve_element_storage_layout,
        );
    });
}

pub(crate) fn with_optimization_program<R>(
    program: &mut crate::ir::Program,
    operation: impl FnOnce(&mut celox_sir_opt::ir::Program<'_>) -> R,
) -> R {
    let crate::ir::Program {
        sir,
        design,
        runtime_schema,
        layout_requirements,
        ..
    } = program;
    let mut unit = celox_sir_opt::ir::Program {
        sir,
        design,
        runtime_schema,
        layout_requirements,
    };
    operation(&mut unit)
}
