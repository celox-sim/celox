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
        inferred_latch_diagnostics,
    } = source;
    let reset_wires = reset_wires(&symbolic);
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
    let fused_ff_factory = super::lowering::global_ff::VerylFusedFfFactory::new(
        &module_ir,
        &source_id_maps,
        config.clone(),
    );
    let output = assembly::schedule_symbolic_rtl(
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
    let mut constant_strings = crate::HashMap::default();
    for (&instance, module_id) in &lookup.instance_module {
        let Some(module) = module_ir.get(module_id) else {
            continue;
        };
        let Some(variables) = source_id_maps.get(module_id) else {
            continue;
        };
        for declaration in &module.declarations {
            let Declaration::Comb(comb) = declaration else {
                continue;
            };
            for statement in &comb.statements {
                let veryl_analyzer::ir::Statement::Assign(assign) = statement else {
                    continue;
                };
                if !assign.expr.comptime().r#type.is_string()
                    || assign.hier_dst.is_some()
                    || assign.dst.len() != 1
                {
                    continue;
                }
                let dst = &assign.dst[0];
                let Some(variable) = module.variables.get(&dst.id) else {
                    continue;
                };
                if !matches!(variable.kind, veryl_analyzer::ir::VarKind::Let) {
                    continue;
                }
                if !assign.expr.comptime().is_const
                    || assign.expr.comptime().get_value().is_err()
                    || variable.r#type.is_array()
                {
                    return Err(ParserError::illegal_context(
                        "string let initializer",
                        "Celox currently supports only constant scalar string let initializers",
                        Some(&variable.token),
                    ));
                }
                if let Some(id) = variables.get(&dst.id)
                    && let Some((address, _)) = lookup.instance_variable(instance, *id)
                {
                    constant_strings.insert(address, assign.expr.clone());
                }
            }
        }
    }
    // Preserve declared polarity before the source sidecar is discarded.
    // PortTypeKind has already resolved abstract `reset` using project defaults,
    // so it cannot distinguish a declaration from that fallback by itself.
    let mut declared_reset_active_low = crate::HashMap::default();
    for (&instance, module_id) in &lookup.instance_module {
        let (Some(module), Some(ids)) = (module_ir.get(module_id), source_id_maps.get(module_id))
        else {
            continue;
        };
        for (id, variable) in &module.variables {
            use veryl_analyzer::ir::TypeKind;
            let active_low = match variable.r#type.kind {
                TypeKind::ResetAsyncHigh | TypeKind::ResetSyncHigh => false,
                TypeKind::ResetAsyncLow | TypeKind::ResetSyncLow => true,
                _ => continue,
            };
            if let Some(id) = ids.get(id)
                && let Some((address, _)) = lookup.instance_variable(instance, *id)
            {
                declared_reset_active_low
                    .entry(address)
                    .and_modify(|previous| {
                        if *previous != Some(active_low) {
                            *previous = None;
                        }
                    })
                    .or_insert(Some(active_low));
            }
        }
    }
    // Input ports have separate state objects in Celox. Follow only exact
    // one-bit copies, not event aliases (opposite async edges have separate
    // event domains even when they share a physical reset level).
    let mut neighbors: crate::HashMap<celox_design::StateAddr, Vec<celox_design::StateAddr>> =
        crate::HashMap::default();
    for (path, &instance) in &lookup.instance_ids {
        let module = lookup.instance_module[&instance];
        for wire in reset_wires.get(&module).into_iter().flatten() {
            let child = if let Some((name, index)) = &wire.child {
                let mut child_path = path.0.clone();
                child_path.push((name.clone(), *index));
                let Some(child) = lookup.instance_ids.get(&crate::InstancePath(child_path)) else {
                    continue;
                };
                *child
            } else {
                instance
            };
            let endpoint = |addr: celox_slt::GlueAddrBase<crate::SourceVarId>| match addr {
                celox_slt::GlueAddrBase::Parent(id) => lookup.instance_variable(instance, id),
                celox_slt::GlueAddrBase::Child(id) => lookup.instance_variable(child, id),
            };
            if let (Some((a, _)), Some((b, _))) = (endpoint(wire.a), endpoint(wire.b)) {
                neighbors.entry(a).or_default().push(b);
                neighbors.entry(b).or_default().push(a);
            }
        }
    }
    let mut visited = HashSet::default();
    for &start in neighbors.keys() {
        if visited.contains(&start) {
            continue;
        }
        let mut pending = vec![start];
        let mut component = Vec::new();
        let mut declared = None;
        let mut conflict = false;
        while let Some(address) = pending.pop() {
            if !visited.insert(address) {
                continue;
            }
            component.push(address);
            if let Some(polarity) = declared_reset_active_low.get(&address) {
                if polarity.is_none()
                    || declared.is_some_and(|previous| Some(previous) != *polarity)
                {
                    conflict = true;
                } else {
                    declared = *polarity;
                }
            }
            pending.extend(neighbors.get(&address).into_iter().flatten().copied());
        }
        if declared.is_some() || conflict {
            for address in component {
                declared_reset_active_low.insert(address, if conflict { None } else { declared });
            }
        }
    }
    let declared_reset_active_low = std::sync::Arc::new(declared_reset_active_low);
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
                    reset_type: config.reset_type,
                    file_dependencies: config.file_dependencies.clone(),
                    constant_strings: constant_strings.clone(),
                    declared_reset_active_low: declared_reset_active_low.clone(),
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
    let testbench_source = VerylTestbenchSource {
        reset_type: config.reset_type,
        file_dependencies: config.file_dependencies.clone(),
        constant_strings,
        declared_reset_active_low,
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
    };
    // Use the exact elaborated module, including parameter specialization, for
    // each process owner. Resolving by root/module name loses child scopes.
    let mut dynamic_for_diagnostics = inferred_latch_diagnostics;
    for source in testbench_source.sources() {
        let instance = source.base_instance(lookup);
        let Some(&module) = module_ir.get(&lookup.instance_module[&instance]) else {
            continue;
        };
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

/// The period of every `$tb::clock_gen` instance of `module`.
///
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
        // A clock variable declared by an instance is a `$tb::clock_gen`;
        // without a `period` it has the default period.
        let Some((_, target)) = instance
            .parameter_connects
            .iter()
            .find(|(name, _)| name.text.to_string() == "period")
        else {
            periods.insert(symbol.found.token.text, 2);
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

struct ResetWire {
    child: Option<(String, usize)>,
    a: celox_slt::GlueAddrBase<crate::SourceVarId>,
    b: celox_slt::GlueAddrBase<crate::SourceVarId>,
}

fn reset_wires(
    source: &crate::symbolic::artifact::SymbolicRtl,
) -> crate::HashMap<celox_design::ModuleId, Vec<ResetWire>> {
    use celox_slt::{GlueAddrBase as Addr, SLTNode};
    let mut result = crate::HashMap::default();
    for (&id, module) in &source.modules {
        let wires = result.entry(id).or_insert_with(Vec::new);
        let one_bit = |id| {
            module
                .variables
                .get(&id)
                .is_some_and(|v| v.metadata.width == 1)
        };
        for path in &module.comb_blocks {
            let Some(target) = path.target.var() else {
                continue;
            };
            if let SLTNode::Input {
                variable,
                index,
                access,
                ..
            } = module.arena.get(path.expr)
                && one_bit(target.id)
                && one_bit(*variable)
                && target.access.lsb == 0
                && target.access.msb == 0
                && access.lsb == 0
                && access.msb == 0
                && index.is_empty()
            {
                wires.push(ResetWire {
                    child: None,
                    a: Addr::Parent(target.id),
                    b: Addr::Parent(*variable),
                });
            }
        }
        for (name, blocks) in &module.glue_blocks {
            for (index, block) in blocks.iter().enumerate() {
                let width = |addr| match addr {
                    Addr::Parent(id) => module.variables[&id].metadata.width,
                    Addr::Child(id) => {
                        source.modules[&block.module_id].variables[&id]
                            .metadata
                            .width
                    }
                };
                for (_, path) in block.input_ports.iter().chain(&block.output_ports) {
                    let Some(target) = path.target.var() else {
                        continue;
                    };
                    if let SLTNode::Input {
                        variable,
                        index: indices,
                        access,
                        ..
                    } = block.arena.get(path.expr)
                        && width(target.id) == 1
                        && width(*variable) == 1
                        && target.access.lsb == 0
                        && target.access.msb == 0
                        && access.lsb == 0
                        && access.msb == 0
                        && indices.is_empty()
                    {
                        wires.push(ResetWire {
                            child: Some((name.clone(), module.instance_index(name, index))),
                            a: target.id,
                            b: *variable,
                        });
                    }
                }
            }
        }
    }
    result
}
