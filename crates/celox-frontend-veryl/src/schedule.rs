use veryl_analyzer::ir::Declaration;

use super::{
    VerylIdMap, VerylScheduledRtlOutput, VerylTestbenchSource, artifact::VerylSymbolicRtl,
};
use crate::{
    BuildConfig, FrontendTrace, FrontendTraceOptions, HashSet, ParserError, symbolic::assembly,
};

/// Project a Veryl-owned lowering result into the source-neutral scheduler,
/// then attach the source AST sidecar consumed by the Veryl testbench compiler.
pub fn schedule_symbolic_rtl(
    source: VerylSymbolicRtl<'_>,
    config: &BuildConfig,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
) -> Result<VerylScheduledRtlOutput, ParserError> {
    let VerylSymbolicRtl {
        symbolic,
        module_ir,
        source_id_maps,
    } = source;
    let root_id = symbolic.root_id;
    let root = module_ir.get(&root_id).copied();
    let initial_statements = root.and_then(|module| {
        let statements = module
            .declarations
            .iter()
            .filter_map(|declaration| match declaration {
                Declaration::Initial(initial) => Some(initial.statements.iter().cloned()),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>();
        (!statements.is_empty()).then_some(statements)
    });
    let functions = root
        .map(|module| module.functions.clone())
        .unwrap_or_default();
    let fused_ff_factory =
        super::lowering::global_ff::VerylFusedFfFactory::new(&module_ir, &source_id_maps, *config);
    let output = assembly::schedule_symbolic_rtl(
        symbolic,
        Some(&fused_ff_factory),
        ignored_loops,
        true_loops,
        four_state,
        trace_options,
        trace,
    )?;
    let lookup = &output.scheduled.frontend_lookup;
    let mut components = Vec::new();
    let mut component_bindings = Vec::new();
    let mut component_names = HashSet::default();
    let mut instances = lookup.instance_ids.iter().collect::<Vec<_>>();
    instances.sort_by_key(|(path, _)| path.0.clone());
    for (path, &instance_id) in instances {
        let Some(module_id) = lookup.instance_module.get(&instance_id) else {
            continue;
        };
        let Some(module) = module_ir.get(module_id) else {
            continue;
        };
        let (mut instance_components, mut instance_bindings) = super::component::collect(
            module,
            instance_id,
            path,
            &lookup.instance_ids,
            &lookup.indexed_instances,
            &mut component_names,
        )?;
        components.append(&mut instance_components);
        component_bindings.append(&mut instance_bindings);
    }
    let testbench_source = VerylTestbenchSource {
        id_map: VerylIdMap {
            module_variables: source_id_maps,
        },
        initial_statements,
        functions,
        components,
        component_bindings,
        component_libraries: Vec::new(),
        component_file_base: None,
    };
    Ok(VerylScheduledRtlOutput {
        scheduled: output.scheduled,
        fused_optimization_hints: output.fused_optimization_hints,
        testbench_source,
    })
}
