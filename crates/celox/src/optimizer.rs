pub use celox_sir_opt::{OptLevel, OptimizeOptions, SirDiagnostics, SirPass};

pub mod sir;

pub fn optimize(
    program: &mut crate::ir::UnoptimizedSir,
    four_state: bool,
    optimize_options: &OptimizeOptions,
) {
    optimize_impl(program, four_state, optimize_options, false);
}

pub(crate) fn optimize_preserving_element_storage(
    program: &mut crate::ir::UnoptimizedSir,
    four_state: bool,
    optimize_options: &OptimizeOptions,
) {
    optimize_impl(program, four_state, optimize_options, true);
}

fn optimize_impl(
    program: &mut crate::ir::UnoptimizedSir,
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
    program: &mut crate::ir::UnoptimizedSir,
    operation: impl FnOnce(&mut celox_sir_opt::OptimizationContext<'_>) -> R,
) -> R {
    let crate::ir::UnoptimizedSir {
        sir,
        layout_requirements,
        runtime,
    } = program;
    let mut unit = celox_sir_opt::OptimizationContext {
        sir,
        design: &runtime.design,
        runtime_schema: &mut runtime.runtime_schema,
        layout_requirements,
    };
    operation(&mut unit)
}

pub(crate) fn with_optimized_program<R>(
    program: &mut crate::ir::OptimizedSir,
    operation: impl FnOnce(&mut celox_sir_opt::OptimizationContext<'_>) -> R,
) -> R {
    let crate::ir::OptimizedSir {
        sir,
        layout_requirements,
        runtime,
    } = program;
    let mut unit = celox_sir_opt::OptimizationContext {
        sir,
        design: &runtime.design,
        runtime_schema: &mut runtime.runtime_schema,
        layout_requirements,
    };
    operation(&mut unit)
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn with_laid_out_program<R>(
    program: &mut crate::ir::LaidOutProgram,
    operation: impl FnOnce(&mut celox_sir_opt::OptimizationContext<'_>) -> R,
) -> R {
    let crate::ir::LaidOutProgram { sir, runtime, .. } = program;
    let mut layout_requirements = Default::default();
    let mut unit = celox_sir_opt::OptimizationContext {
        sir,
        design: &runtime.design,
        runtime_schema: &mut runtime.runtime_schema,
        layout_requirements: &mut layout_requirements,
    };
    let result = operation(&mut unit);
    debug_assert!(layout_requirements.is_empty());
    result
}
