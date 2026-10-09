use veryl_analyzer::{ir::Declaration, symbol::Affiliation};

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
    parallel: &celox_frontend_core::ParallelScheduleOptions,
    trace_options: &FrontendTraceOptions,
    trace: Option<&mut FrontendTrace>,
) -> Result<VerylScheduledRtlOutput, ParserError> {
    let VerylSymbolicRtl {
        symbolic,
        module_ir,
        source_id_maps,
        process_storage,
    } = source;
    let root_id = symbolic.root_id;
    let root = module_ir.get(&root_id).copied();
    let initial_block_lengths = root
        .map(|module| {
            module
                .declarations
                .iter()
                .filter_map(|declaration| match declaration {
                    Declaration::Initial(initial) => Some(initial.statements.len()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
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
    let mut output = assembly::schedule_symbolic_rtl(
        symbolic,
        Some(&fused_ff_factory),
        ignored_loops,
        true_loops,
        four_state,
        parallel,
        trace_options,
        trace,
    )?;
    let lookup = &output.scheduled.frontend_lookup;
    let mut child_sources = Vec::new();
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
        if !path.0.is_empty() {
            let blocks: Vec<_> = module
                .declarations
                .iter()
                .filter_map(|declaration| match declaration {
                    Declaration::Initial(initial) => Some(initial.statements.clone()),
                    _ => None,
                })
                .collect();
            if blocks.iter().any(|block| !block.is_empty()) {
                child_sources.push(VerylTestbenchSource {
                    id_map: VerylIdMap {
                        module_variables: source_id_maps
                            .get(module_id)
                            .map(|variables| (*module_id, variables.clone()))
                            .into_iter()
                            .collect(),
                    },
                    initial_block_lengths: blocks.iter().map(Vec::len).collect(),
                    initial_statements: Some(blocks.into_iter().flatten().collect()),
                    instance: Some(instance_id),
                    function_locals: module
                        .variables
                        .iter()
                        .filter_map(|(&id, variable)| {
                            (variable.affiliation == Affiliation::Function).then_some(id)
                        })
                        .collect(),
                    functions: module.functions.clone(),
                    clock_periods: configured_clock_periods(module)?,
                    ..Default::default()
                });
            }
        }
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
    let mut testbench_source = VerylTestbenchSource {
        id_map: VerylIdMap {
            module_variables: source_id_maps,
        },
        initial_statements,
        initial_block_lengths,
        instance: None,
        child_sources,
        function_locals: root
            .map(|module| {
                module
                    .variables
                    .iter()
                    .filter_map(|(&id, variable)| {
                        (variable.affiliation == Affiliation::Function).then_some(id)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        functions,
        clock_periods: root
            .map(configured_clock_periods)
            .transpose()?
            .unwrap_or_default(),
        components,
        component_bindings,
        component_libraries: Vec::new(),
        component_file_base: None,
        kernels: false,
    };
    if config.testbench_kernels && !testbench_source.is_empty() {
        super::lowering::process::lower_testbench_kernels(
            &mut output.scheduled,
            &testbench_source,
            &module_ir,
            &process_storage,
            config,
        )?;
        testbench_source.kernels = true;
    }
    // Use the exact elaborated module, including parameter specialization, for
    // each process owner. Resolving by root/module name loses child scopes.
    let mut dynamic_for_diagnostics = Vec::new();
    for source in testbench_source.sources() {
        let lookup = &output.scheduled.frontend_lookup;
        let instance = source.base_instance(lookup);
        let module = module_ir[&lookup.instance_module[&instance]];
        dynamic_for_diagnostics.extend(super::check_elaborated_dynamic_for_bounds(
            &output.scheduled,
            source,
            module,
            &output.fused_optimization_hints,
        ));
    }
    Ok(VerylScheduledRtlOutput {
        scheduled: output.scheduled,
        fused_optimization_hints: output.fused_optimization_hints,
        testbench_source,
        dynamic_for_diagnostics,
    })
}

/// Veryl 0.22 keeps clock configuration on instance symbols, but only copies it
/// into ClockNext IR. Recover it while the analyzer symbols are still available,
/// using the specialization's overrides rather than a parameter's default.
fn configured_clock_periods(
    module: &veryl_analyzer::ir::Module,
) -> Result<crate::HashMap<veryl_parser::resource_table::StrId, u64>, ParserError> {
    use veryl_analyzer::{
        Context,
        ir::{Comptime, Expression, ValueVariant, VarPath},
        symbol::SymbolKind,
        symbol_table,
    };
    let mut context = None;
    let mut periods = crate::HashMap::default();
    for variable in module.variables.values() {
        if !variable.r#type.is_clock() {
            continue;
        }
        let Ok(symbol) = symbol_table::resolve(&variable.token.beg) else {
            continue;
        };
        let SymbolKind::Instance(instance) = &symbol.found.kind else {
            continue;
        };
        let Some((_, target)) = instance
            .parameter_connects
            .iter()
            .find(|(name, _)| name.text.to_string() == "period")
        else {
            continue;
        };
        let context = context.get_or_insert_with(|| {
            let mut context = Context::default();
            context.variables = module.variables.clone();
            context.push_generic_map(module.signature.to_generic_map());
            let overrides = module
                .signature
                .parameters
                .iter()
                .filter_map(|(name, value)| {
                    let ValueVariant::Numeric(value) = value else {
                        return None;
                    };
                    Some((
                        VarPath::new(*name),
                        (
                            Comptime::create_value(value.clone(), module.token),
                            Expression::create_value(value.clone(), module.token),
                        ),
                    ))
                })
                .collect();
            context.push_override(module.signature.namespace(), overrides);
            context
        });
        let value =
            veryl_analyzer::conv::utils::eval_expr(context, None, &target.expression, false)
                .ok()
                .and_then(|(comptime, _)| comptime.get_value().ok().cloned());
        let Some(value) = value else {
            return Err(ParserError::illegal_context(
                "testbench clock period",
                "cannot evaluate configured clock period",
                Some(&variable.token),
            ));
        };
        periods.insert(symbol.found.token.text, value.payload_u64().max(2));
    }
    Ok(periods)
}
