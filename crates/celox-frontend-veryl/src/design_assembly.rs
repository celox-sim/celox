use std::collections::{BTreeMap, BTreeSet};

use celox_design::{
    BitAccess, DomainKind, ElaboratedDesign, EventTopology, InitialStateValue, InstanceId,
    ModuleId, PortTypeKind, RegionedStateAddr, RuntimeCombObserver, RuntimeErrorInfo,
    RuntimeEventKind, RuntimeEventSite, RuntimeSchema, STABLE_REGION, StateAddr, StateObjectId,
    VarAtomBase, VariableMetadata,
};
use celox_sir::{BasicBlock, ExecutionUnit, SIRInstruction, SIRTerminator, SirProgram};
use celox_slt::{
    CombObserver, FfAccessSummary, LogicPath, LogicPathId, LogicPathTarget, NodeId, SLTNodeArena,
    scheduler::{self, SchedulerError},
};
use celox_testbench::{
    ComponentConnection, ComponentParameterValue, SourceLocation as TestbenchSourceLocation,
    TestbenchComponent,
};
use veryl_analyzer::ir::ExternalParamValue;
use veryl_analyzer::ir::TypeKind;
use veryl_analyzer::ir::{Declaration, Module, VarId, VarPath};
use veryl_analyzer::symbol::Affiliation;
use veryl_analyzer::value::Value;
use veryl_metadata::{ClockType, ResetType};
use veryl_parser::resource_table::{self, StrId};

use crate::{
    AbsoluteAddr, BuildConfig, FfRuntimeRelocation, FrontendTrace, FrontendTraceOptions,
    FusedSirOptimizationHints, GlueAddr, HashMap, HashSet, InstancePath, ParserError,
    RegionedAbsoluteAddr, RegionedVarAddr, RelocationModule, ScheduledRtl, ScheduledRtlOutput,
    SharedClockLowering, SimModule, SourceLocation, SymbolicRtl, VariableInfo,
    VerylComponentBinding, VerylComponentConnectionBinding, VerylComponentEventBinding,
    VerylComponentInputBinding, VerylFrontendLookup, VerylTestbenchSource, bitaccess,
    build_ff_clock_recipes, flattening, resolve_total_width,
};

fn string_of(id: StrId) -> String {
    resource_table::get_str_value(id).unwrap_or_default()
}

fn testbench_source_location(
    token: &veryl_parser::token_range::TokenRange,
) -> Option<TestbenchSourceLocation> {
    let file = token
        .beg
        .source
        .get_path()
        .and_then(resource_table::get_path_value)?;
    Some(TestbenchSourceLocation {
        file: file.to_string_lossy().into_owned(),
        line: token.beg.line,
        column: token.beg.column,
    })
}

fn component_parameter(value: &ExternalParamValue) -> ComponentParameterValue {
    match value {
        ExternalParamValue::Str(value) => ComponentParameterValue::String(value.clone()),
        ExternalParamValue::Value(value) => {
            let mut words = match value {
                Value::U64(value) => vec![value.payload],
                Value::BigUint(value) => value.payload().iter_u64_digits().collect(),
            };
            words.resize(value.width().div_ceil(64).max(1), 0);
            ComponentParameterValue::Bits {
                words,
                width: value.width() as u32,
            }
        }
    }
}

fn component_input_target(
    expression: &veryl_analyzer::ir::Expression,
) -> Option<VerylComponentInputBinding> {
    use veryl_analyzer::ir::{Expression, Factor, Op};

    match expression {
        Expression::Term(term) => match term.as_ref() {
            Factor::Variable(id, index, select, _) => Some(VerylComponentInputBinding::Root {
                id: *id,
                index: index.clone(),
                select: select.clone(),
            }),
            Factor::HierVariable(reference) => {
                Some(VerylComponentInputBinding::Hierarchical(reference.clone()))
            }
            _ => None,
        },
        Expression::Unary(Op::BitNot, inner, _) => component_input_target(inner),
        _ => None,
    }
}

fn component_event_target(
    expression: &veryl_analyzer::ir::Expression,
) -> Option<VerylComponentEventBinding> {
    use veryl_analyzer::ir::{Expression, Factor};

    let Expression::Term(term) = expression else {
        return None;
    };
    match term.as_ref() {
        Factor::Variable(id, _, _, _) => Some(VerylComponentEventBinding::Root(*id)),
        Factor::HierVariable(reference) => {
            Some(VerylComponentEventBinding::Hierarchical(reference.clone()))
        }
        _ => None,
    }
}

fn collect_testbench_components(
    module: &Module,
    parent_instance: InstanceId,
    parent_path: &InstancePath,
    names: &mut HashSet<String>,
) -> Result<(Vec<TestbenchComponent>, Vec<VerylComponentBinding>), ParserError> {
    let mut components = Vec::new();
    let mut bindings = Vec::new();
    for declaration in &module.declarations {
        let Declaration::External(external) = declaration else {
            continue;
        };
        let local_name = string_of(external.name);
        let prefix = parent_path
            .0
            .iter()
            .map(|(name, index)| format!("{}[{index}]", string_of(*name)))
            .collect::<Vec<_>>()
            .join(".");
        let instance = if prefix.is_empty() {
            local_name
        } else {
            format!("{prefix}.{local_name}")
        };
        if !names.insert(instance.clone()) {
            return Err(ParserError::illegal_context(
                "testbench component elaboration",
                format!("duplicate component instance `{instance}`"),
                Some(&external.token),
            ));
        }
        let connections = external
            .connects
            .iter()
            .map(|connection| ComponentConnection {
                port: string_of(connection.port),
                group: connection.group.map(string_of),
                member: connection.member.map(string_of),
                input: connection.input,
                has_output: connection.output.is_some(),
                is_clock: connection.is_clock,
                is_reset: connection.is_reset,
                width: connection.width,
            })
            .collect();
        let connection_bindings = external
            .connects
            .iter()
            .map(|connection| {
                let output = connection.output.clone();
                let input_target = if connection.input {
                    component_input_target(&connection.expr)
                } else {
                    None
                };
                let sync_reset = connection.is_reset
                    && matches!(
                        connection.expr.comptime().r#type.kind,
                        TypeKind::ResetSyncHigh | TypeKind::ResetSyncLow
                    );
                let event = if (connection.is_clock || connection.is_reset) && !sync_reset {
                    output
                        .as_ref()
                        .map(|output| VerylComponentEventBinding::Root(output.id))
                        .or_else(|| component_event_target(&connection.expr))
                } else {
                    None
                };
                VerylComponentConnectionBinding {
                    port: string_of(connection.port),
                    input: connection.input.then(|| connection.expr.clone()),
                    input_target,
                    output,
                    event,
                }
            })
            .collect();
        components.push(TestbenchComponent {
            instance: instance.clone(),
            component: string_of(external.component),
            params: external
                .params
                .iter()
                .map(|(name, value)| (string_of(*name), component_parameter(value)))
                .collect(),
            connections,
            is_var_form: external.is_var_form,
            source: testbench_source_location(&external.token),
        });
        bindings.push(VerylComponentBinding {
            instance,
            parent_instance,
            functions: module.functions.clone(),
            connections: connection_bindings,
        });
    }
    Ok((components, bindings))
}

fn flatten_with_trace(
    module: &SimModule,
    path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
) -> Result<RelocationModule, celox_slt::SLTNodeFactsError> {
    let flattened = flattening::flatten_module(
        module,
        path,
        instance_ids,
        global_boundaries,
        unpacked_element_widths,
        arena,
    )?;

    if let Some(trace) = trace.as_deref_mut()
        && trace_opts.pre_atomized_comb_blocks
    {
        match &mut trace.pre_atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.pre_atomized_comb_blocks);
                *trace_arena = arena.clone();
            }
            slot @ None => *slot = Some((flattened.pre_atomized_comb_blocks, arena.clone())),
        }
    }

    if let Some(trace) = trace
        && trace_opts.atomized_comb_blocks
    {
        match &mut trace.atomized_comb_blocks {
            Some((blocks, trace_arena)) => {
                blocks.extend(flattened.relocation.comb_blocks.iter().cloned());
                *trace_arena = arena.clone();
            }
            slot @ None => {
                *slot = Some((flattened.relocation.comb_blocks.clone(), arena.clone()));
            }
        }
    }

    Ok(flattened.relocation)
}

fn remap_for_fold_runtime_event_sites<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    start: usize,
    runtime_event_site_map: &HashMap<u32, u32>,
) -> Result<(), ParserError> {
    arena
        .remap_for_fold_effect_sites(start..arena.len(), |site_id, fatal_error_code| {
            Ok(runtime_event_site_map.get(&site_id).map(|&global_site| {
                (
                    global_site,
                    fatal_error_code.map(|_| i64::from(global_site)),
                )
            }))
        })
        .map_err(|error| {
            ParserError::illegal_context(
                "ForFold runtime-event relocation",
                error.to_string(),
                None,
            )
        })
}

fn create_absolute_addr(
    instance_path: &[(String, usize)],
    var_path: &[String],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> AbsoluteAddr {
    let instance_path = InstancePath(
        instance_path
            .iter()
            .map(|s| (resource_table::insert_str(&s.0), s.1))
            .collect(),
    );
    let instance_id = expanded[&instance_path];
    let module_id = instance_modules[&instance_id];
    let module = &modules[&module_id];
    let var_path = VarPath(
        var_path
            .iter()
            .map(|s| resource_table::insert_str(s))
            .collect(),
    );
    let var_id = *module
        .variables
        .iter()
        .find(|v| v.1.path == var_path)
        .unwrap()
        .0;
    AbsoluteAddr {
        instance_id,
        var_id,
    }
}
fn parse_ignored_loops(
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashSet<(AbsoluteAddr, AbsoluteAddr)> {
    let mut res = HashSet::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path)) in ignored_loops {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to));
    }
    res
}
fn parse_true_loops(
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashMap<(AbsoluteAddr, AbsoluteAddr), usize> {
    let mut res = HashMap::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path), max_iter) in
        true_loops
    {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to), *max_iter);
    }
    res
}

fn scheduler_source_locations(
    error: &SchedulerError<AbsoluteAddr>,
    module_ir: &HashMap<ModuleId, &Module>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
) -> Vec<SourceLocation> {
    let blocks = match error {
        SchedulerError::CombinationalLoop { blocks } => blocks,
        SchedulerError::MultipleDriver { blocks } => blocks,
        SchedulerError::InvalidDependencyGraph => return Vec::new(),
    };
    let mut seen = HashSet::default();
    blocks
        .iter()
        .filter_map(|block| {
            let addr = block.target.var()?.id;
            if !seen.insert(addr) {
                return None;
            }
            let module_id = instance_modules.get(&addr.instance_id)?;
            let module = module_ir.get(module_id)?;
            let var = module.variables.get(&addr.var_id)?;
            Some(SourceLocation::from_token(&var.token))
        })
        .collect()
}

pub fn schedule_symbolic_rtl(
    symbolic: SymbolicRtl<'_>,
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
    trace_opts: &FrontendTraceOptions,
    mut trace: Option<&mut FrontendTrace>,
) -> Result<ScheduledRtlOutput, ParserError> {
    let SymbolicRtl {
        modules,
        module_ir,
        module_names,
        root_id,
    } = symbolic;
    let flatten_timing = trace_opts.phase_timing;
    macro_rules! timed_sub {
        ($label:expr, $body:expr) => {{
            if flatten_timing {
                let start = std::time::Instant::now();
                let result = $body;
                tracing::debug!("[flatten] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.sim_modules
    {
        t.sim_modules = Some(modules.clone());
    }

    let (expanded, instance_modules) =
        timed_sub!("expand_hierarchy", expand_hierarchy(&root_id, &modules));
    let global_boundaries = timed_sub!(
        "propagate_boundaries",
        propagate_boundaries(&expanded, &instance_modules, &modules)
    );
    let unpacked_element_widths = instance_modules
        .iter()
        .flat_map(|(&instance_id, &module_id)| {
            module_ir[&module_id]
                .variables
                .iter()
                .filter_map(move |(&var_id, variable)| {
                    let element_count = variable.r#type.total_array()?;
                    let element_width = variable.total_width()?;
                    (element_count > 1 && element_width > 0).then_some((
                        AbsoluteAddr {
                            instance_id,
                            var_id,
                        },
                        element_width,
                    ))
                })
        })
        .collect::<HashMap<_, _>>();

    let clock_domains = timed_sub!(
        "unify_clock_domains",
        unify_clock_domains(&expanded, &instance_modules, &modules)
    );
    let (
        mut global_arena,
        mut eval_apply_ffs,
        mut eval_only_ffs,
        mut apply_ffs,
        _ff_access_summaries,
        ff_runtime_relocations,
        mut comb_blocks,
        mut comb_observers,
        mut runtime_errors,
        runtime_event_sites,
        next_runtime_error_code,
    ) = timed_sub!(
        "relocate_units",
        relocate_units(
            &expanded,
            &instance_modules,
            &modules,
            &global_boundaries,
            &unpacked_element_widths,
            &clock_domains,
            trace_opts,
            &mut trace,
        )
    )?;
    let ignored_loops = parse_ignored_loops(ignored_loops, &instance_modules, &modules, &expanded);
    let true_loops = parse_true_loops(true_loops, &instance_modules, &modules, &expanded);

    // Build reset -> clock mapping with AbsoluteAddr
    let mut reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (reset_var_id, clock_var_id) in &sim_module.reset_clock_map {
            let reset_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *reset_var_id,
            };
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *clock_var_id,
            };
            // Use canonical clock domain if available
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            let canonical_reset = clock_domains
                .get(&reset_addr)
                .copied()
                .unwrap_or(reset_addr);
            reset_clock_map.insert(canonical_reset, canonical_clock);
        }
    }

    let (topological_clocks, cascaded_clocks) = timed_sub!(
        "analyze_clock_dependencies",
        analyze_clock_dependencies(
            &mut eval_apply_ffs,
            &mut eval_only_ffs,
            &mut apply_ffs,
            &comb_blocks,
            &global_arena,
            &clock_domains,
            &expanded,
            &instance_modules,
            &modules,
            config,
        )
    );

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.flattened_comb_blocks
    {
        t.flattened_comb_blocks = Some((comb_blocks.clone(), global_arena.clone()));
    }

    // Constant variable inlining: detect variables whose every LogicPath
    // is a constant, then replace all Input references with Constant nodes.
    // This eliminates Store→Load roundtrips for compile-time constants
    // (e.g. genvar-expanded parity-check matrices).
    celox_slt::const_inline::inline_constant_variables(&mut comb_blocks, &mut global_arena)?;
    apply_always_comb_previous_source_ordering(&mut comb_blocks);

    let var_widths: HashMap<AbsoluteAddr, usize> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            let module = module_ir[&mod_id];
            module.variables.iter().filter_map(move |(var_id, var)| {
                resolve_total_width(module, var).ok().map(|w| {
                    (
                        AbsoluteAddr {
                            instance_id: inst_id,
                            var_id: *var_id,
                        },
                        w,
                    )
                })
            })
        })
        .collect();
    let var_signedness: HashMap<AbsoluteAddr, bool> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            module_ir[&mod_id]
                .variables
                .iter()
                .map(move |(var_id, var)| {
                    (
                        AbsoluteAddr {
                            instance_id: inst_id,
                            var_id: *var_id,
                        },
                        var.r#type.signed,
                    )
                })
        })
        .collect();

    build_comb_observer_capture_paths(
        &mut comb_blocks,
        &mut comb_observers,
        &runtime_event_sites,
        &mut global_arena,
    )?;
    for (site_id, site) in runtime_event_sites.iter().enumerate() {
        if !matches!(site.kind, RuntimeEventKind::AssertFatal) {
            continue;
        }
        runtime_errors
            .entry(site_id as i64)
            .or_insert_with(|| RuntimeErrorInfo {
                message: site
                    .template
                    .clone()
                    .unwrap_or_else(|| "assertion failed".to_string()),
                signals: Vec::new(),
            });
    }

    celox_slt::verify_symbolic_roots(
        &global_arena,
        &comb_blocks,
        &comb_observers,
        &var_widths,
        &var_signedness,
    )
    .map_err(|error| ParserError::SltVerify {
        phase: "after flattening symbolic logic",
        error,
    })?;

    let ff_clock_recipes = build_ff_clock_recipes(
        &module_ir,
        &modules,
        &instance_modules,
        &clock_domains,
        &ff_runtime_relocations,
        *config,
    );
    let mut clock_arena = SLTNodeArena::<RegionedAbsoluteAddr>::new();
    let mut clock_node_cache = HashMap::default();
    let clock_comb_blocks = comb_blocks
        .iter()
        .map(|path| {
            path.map_addr(
                &global_arena,
                &mut clock_arena,
                &mut clock_node_cache,
                &|addr| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, *addr),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let clock_var_widths = var_widths
        .iter()
        .map(|(&addr, &width)| {
            (
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                width,
            )
        })
        .collect::<HashMap<_, _>>();
    let clock_unpacked_element_widths = unpacked_element_widths
        .iter()
        .map(|(&addr, &element_width)| {
            (
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, addr),
                element_width,
            )
        })
        .collect::<HashMap<_, _>>();
    let clock_ignored_loops = ignored_loops
        .iter()
        .map(|&(from, to)| {
            (
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, from),
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, to),
            )
        })
        .collect::<HashSet<_>>();
    let clock_true_loops = true_loops
        .iter()
        .map(|(&(from, to), &limit)| {
            (
                (
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, from),
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, to),
                ),
                limit,
            )
        })
        .collect::<HashMap<_, _>>();

    let sched_start = flatten_timing.then(std::time::Instant::now);
    let schedule = match scheduler::sort_with_unpacked_element_widths(
        comb_blocks,
        &global_arena,
        &ignored_loops,
        &true_loops,
        four_state,
        &var_widths,
        &unpacked_element_widths,
        next_runtime_error_code,
    ) {
        Ok(schedule) => schedule,
        Err(error) => {
            let (err_vars, err_path_idx) = module_variables(&module_ir, config).unwrap_or_default();
            let frontend_lookup = VerylFrontendLookup {
                instance_ids: expanded.clone(),
                instance_module: instance_modules.clone(),
                module_variables: err_vars,
                module_var_path_index: err_path_idx,
                module_names: module_names.clone(),
                source_to_state: HashMap::default(),
                state_to_source: HashMap::default(),
                event_aliases: HashMap::default(),
            };
            let source_locations =
                scheduler_source_locations(&error, &module_ir, &instance_modules);
            let mut target_arena = SLTNodeArena::new();
            let error = error.map_addr(&global_arena, &mut target_arena, &|addr| {
                frontend_lookup.get_path(addr)
            })?;
            return Err(if source_locations.is_empty() {
                ParserError::Scheduler(error)
            } else {
                ParserError::SchedulerWithLocation {
                    error,
                    source_locations,
                }
            });
        }
    };
    if let Some(s) = sched_start {
        tracing::debug!("[flatten] scheduler::sort: {:?}", s.elapsed());
    }
    runtime_errors.extend(schedule.runtime_errors);
    let schduled: Vec<ExecutionUnit<RegionedAbsoluteAddr>> = schedule
        .execution_units
        .into_iter()
        .map(|eu| ExecutionUnit {
            entry_block_id: eu.entry_block_id,
            blocks: eu
                .blocks
                .into_iter()
                .map(|(id, bb)| {
                    (
                        id,
                        BasicBlock {
                            id: bb.id,
                            params: bb.params,
                            instructions: bb
                                .instructions
                                .into_iter()
                                .map(|inst| {
                                    inst.into_map_addr(|addr| RegionedAbsoluteAddr {
                                        region: STABLE_REGION,
                                        instance_id: addr.instance_id,
                                        var_id: addr.var_id,
                                    })
                                })
                                .collect(),
                            terminator: bb.terminator,
                        },
                    )
                })
                .collect(),
            register_map: eu.register_map,
        })
        .collect();
    let eval_comb = schduled.clone();
    let mut eval_comb_apply_ffs = HashMap::default();
    let mut fused_direct_ff_writes = HashMap::default();
    let mut fused_schedule_cache = HashMap::<
        Vec<usize>,
        (
            Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
            Vec<VarAtomBase<RegionedAbsoluteAddr>>,
        ),
    >::default();
    for (trigger, recipes) in ff_clock_recipes {
        let recipe_ids = recipes.iter().map(|recipe| recipe.id).collect::<Vec<_>>();
        if let Some((units, direct_ff_writes)) = fused_schedule_cache.get(&recipe_ids) {
            eval_comb_apply_ffs.insert(trigger, units.clone());
            fused_direct_ff_writes.insert(trigger, direct_ff_writes.clone());
            continue;
        }
        let mut ff_lowering = SharedClockLowering::new(recipes, *config);
        let fused_start = flatten_timing.then(std::time::Instant::now);
        let fused = match scheduler::sort_clock(
            clock_comb_blocks.clone(),
            &clock_arena,
            &clock_ignored_loops,
            &clock_true_loops,
            four_state,
            &clock_var_widths,
            &clock_unpacked_element_widths,
            next_runtime_error_code,
            &mut ff_lowering,
        ) {
            Ok(schedule) => schedule,
            Err(scheduler::ClockSortError::Lowering(error)) => return Err(error),
            Err(scheduler::ClockSortError::Scheduler(error)) => {
                let mut error_arena = SLTNodeArena::new();
                let error =
                    error.map_addr(&clock_arena, &mut error_arena, &|addr| addr.to_string())?;
                return Err(ParserError::Scheduler(error));
            }
        };
        if let Some(start) = fused_start {
            tracing::debug!("[flatten] scheduler::sort_clock: {:?}", start.elapsed());
        }
        let direct_ff_writes = fused.direct_ff_writes;
        let units = fused.execution_units;
        fused_schedule_cache.insert(recipe_ids, (units.clone(), direct_ff_writes.clone()));
        eval_comb_apply_ffs.insert(trigger, units);
        fused_direct_ff_writes.insert(trigger, direct_ff_writes);
    }

    if let Some(t) = trace
        && trace_opts.scheduled_units
    {
        t.scheduled_units = Some(schduled.clone());
    }

    // The unified function is the normal fast path.  Split evaluator/apply
    // functions are needed only when scheduling can evaluate several active
    // domains or must cascade through a derived clock.
    let active_ff_domains = eval_apply_ffs
        .values()
        .filter(|units| !units.is_empty())
        .count();
    let needs_split_path = active_ff_domains > 1 || !cascaded_clocks.is_empty();
    let (eval_only_ffs, apply_ffs) = if needs_split_path {
        (eval_only_ffs, apply_ffs)
    } else {
        (HashMap::default(), HashMap::default())
    };

    // Extract initial block statements from root module (for native testbenches)
    let initial_statements = module_ir.get(&root_id).and_then(|root_module| {
        let mut stmts = Vec::new();
        for decl in &root_module.declarations {
            if let Declaration::Initial(init_decl) = decl {
                stmts.extend(init_decl.statements.iter().cloned());
            }
        }
        if stmts.is_empty() { None } else { Some(stmts) }
    });

    let (mod_vars, mod_path_idx) = module_variables(&module_ir, config)?;
    let initial_memory_values: Vec<InitialStateValue<AbsoluteAddr>> = instance_modules
        .iter()
        .flat_map(|(&instance_id, module_id)| {
            modules[module_id]
                .initial_memory_values
                .iter()
                .map(move |init| InitialStateValue {
                    address: AbsoluteAddr {
                        instance_id,
                        var_id: init.address,
                    },
                    data: init.data.clone(),
                })
        })
        .collect();
    let state_objects: HashMap<AbsoluteAddr, VariableMetadata> = instance_modules
        .iter()
        .flat_map(|(&instance_id, module_id)| {
            mod_vars[module_id].values().map(move |info| {
                (
                    AbsoluteAddr {
                        instance_id,
                        var_id: info.id,
                    },
                    info.metadata.clone(),
                )
            })
        })
        .collect();
    let runtime_comb_observers: Vec<RuntimeCombObserver<AbsoluteAddr>> = comb_observers
        .iter()
        .map(|observer| RuntimeCombObserver {
            site_id: observer.site_id,
            activation_group: observer.activation_group,
            sensitivity: observer.sensitivity.clone(),
            written_inputs: observer.written_inputs.clone(),
        })
        .collect();
    let mut components = Vec::new();
    let mut component_bindings = Vec::new();
    let mut component_names = HashSet::default();
    let mut elaborated_instances = expanded.iter().collect::<Vec<_>>();
    elaborated_instances.sort_by_key(|(path, _)| path.0.clone());
    for (path, &instance_id) in elaborated_instances {
        let Some(module_id) = instance_modules.get(&instance_id) else {
            continue;
        };
        let Some(module) = module_ir.get(module_id) else {
            continue;
        };
        let (mut instance_components, mut instance_bindings) =
            collect_testbench_components(module, instance_id, path, &mut component_names)?;
        components.append(&mut instance_components);
        component_bindings.append(&mut instance_bindings);
    }
    let testbench_source = VerylTestbenchSource {
        initial_statements,
        functions: module_ir
            .get(&root_id)
            .map(|m| m.functions.clone())
            .unwrap_or_default(),
        components,
        component_bindings,
        component_libraries: Vec::new(),
        component_file_base: None,
    };
    let source_sir = SirProgram {
        eval_apply_ffs,
        eval_comb_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        eval_comb,
    };
    let mut source_addresses = state_objects.keys().copied().collect::<Vec<_>>();
    source_addresses.sort_unstable();
    let mut source_to_state = HashMap::default();
    let mut state_to_source = HashMap::default();
    for (index, source) in source_addresses.into_iter().enumerate() {
        let object = StateObjectId(u32::try_from(index).map_err(|_| {
            ParserError::illegal_context(
                "design state projection",
                "flattened state object count exceeds u32",
                None,
            )
        })?);
        let state = StateAddr {
            instance_id: source.instance_id,
            var_id: object,
        };
        source_to_state.insert(source, state);
        state_to_source.insert(state, source);
    }
    let project = |source: AbsoluteAddr| source_to_state[&source];
    let project_regioned = |source: RegionedAbsoluteAddr| RegionedStateAddr {
        region: source.region,
        instance_id: source.instance_id,
        var_id: source_to_state[&source.absolute_addr()].var_id,
    };

    let sir = source_sir.into_map_addr(project, project_regioned);
    let state_objects: HashMap<StateAddr, VariableMetadata> = state_objects
        .into_iter()
        .map(|(address, metadata)| (project(address), metadata))
        .collect();
    let initial_state = initial_memory_values
        .into_iter()
        .map(|initial| InitialStateValue {
            address: project(initial.address),
            data: initial.data,
        })
        .collect();
    let events = EventTopology {
        aliases: clock_domains
            .into_iter()
            .map(|(alias, canonical)| (project(alias), project(canonical)))
            .collect(),
        ordered_events: topological_clocks.into_iter().map(project).collect(),
        cascaded_events: cascaded_clocks.into_iter().map(project).collect(),
        reset_clocks: reset_clock_map
            .into_iter()
            .map(|(reset, clock)| (project(reset), project(clock)))
            .collect(),
    };
    let event_aliases = events.aliases.clone();
    let runtime_errors = runtime_errors
        .into_iter()
        .map(|(code, info)| {
            (
                code,
                RuntimeErrorInfo {
                    message: info.message,
                    signals: info.signals.into_iter().map(project).collect(),
                },
            )
        })
        .collect();
    let comb_observers = runtime_comb_observers
        .into_iter()
        .map(|observer| RuntimeCombObserver {
            site_id: observer.site_id,
            activation_group: observer.activation_group,
            sensitivity: observer
                .sensitivity
                .into_iter()
                .map(|atom| VarAtomBase {
                    id: project(atom.id),
                    access: atom.access,
                })
                .collect(),
            written_inputs: observer.written_inputs.into_iter().map(project).collect(),
        })
        .collect();
    let direct_ff_writes = fused_direct_ff_writes
        .into_iter()
        .map(|(event, writes)| {
            (
                project(event),
                writes
                    .into_iter()
                    .map(|atom| VarAtomBase {
                        id: project_regioned(atom.id),
                        access: atom.access,
                    })
                    .collect(),
            )
        })
        .collect();

    let mut rtl_writes = HashSet::default();
    for unit in sir
        .eval_comb
        .iter()
        .chain(sir.eval_apply_ffs.values().flatten())
        .chain(sir.eval_comb_apply_ffs.values().flatten())
        .chain(sir.eval_only_ffs.values().flatten())
        .chain(sir.apply_ffs.values().flatten())
    {
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                let (address, offset, width) = match instruction {
                    SIRInstruction::Store(address, offset, width, ..)
                    | SIRInstruction::Commit(_, address, offset, width, _) => {
                        (address.absolute_addr(), offset, *width)
                    }
                    _ => continue,
                };
                let access = offset
                    .constant_bit_offset()
                    .and_then(|lsb| {
                        width
                            .checked_sub(1)
                            .and_then(|tail| lsb.checked_add(tail))
                            .map(|msb| BitAccess::new(lsb, msb))
                    })
                    .or_else(|| {
                        state_objects
                            .get(&address)
                            .and_then(|object| object.width.checked_sub(1))
                            .map(|msb| BitAccess::new(0, msb))
                    });
                if let Some(access) = access {
                    rtl_writes.insert(VarAtomBase {
                        id: address,
                        access,
                    });
                }
            }
        }
    }

    let scheduled = ScheduledRtl {
        sir,
        design: ElaboratedDesign {
            state_objects,
            events,
            initial_state,
        },
        frontend_lookup: VerylFrontendLookup {
            instance_ids: expanded,
            instance_module: instance_modules,
            module_variables: mod_vars,
            module_var_path_index: mod_path_idx,
            module_names,
            source_to_state,
            state_to_source,
            event_aliases,
        },
        runtime_schema: RuntimeSchema {
            runtime_errors,
            runtime_event_sites,
            comb_observers,
            testbench_read_roots: Default::default(),
            rtl_writes,
        },
        testbench_source,
    };

    Ok(ScheduledRtlOutput {
        scheduled,
        fused_optimization_hints: FusedSirOptimizationHints { direct_ff_writes },
    })
}

fn module_variables(
    module_ir: &HashMap<ModuleId, &Module>,
    config: &BuildConfig,
) -> Result<
    (
        HashMap<ModuleId, HashMap<VarId, VariableInfo>>,
        HashMap<ModuleId, HashMap<VarPath, Option<VarId>>>,
    ),
    ParserError,
> {
    let mut res = HashMap::default();
    let mut path_index = HashMap::default();
    for (id, module) in module_ir {
        let mut variables = HashMap::default();
        let mut paths: HashMap<VarPath, Option<VarId>> = HashMap::default();
        for (id, varibale) in &module.variables {
            // Only module-scope variables are externally addressable. Locals
            // may share the same VarPath, but must not make a legal
            // hierarchical or public lookup appear ambiguous.
            if varibale.affiliation == Affiliation::Module {
                match paths.entry(varibale.path.clone()) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(Some(*id));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        // Duplicate visible VarPath — mark as ambiguous.
                        e.insert(None);
                    }
                }
            }
            let (dimensions, _, _) = bitaccess::get_dimensions_and_strides(module, *id)?;
            let packed_dims = dimensions
                .into_iter()
                .skip(varibale.r#type.array.iter().count())
                .collect();
            variables.insert(
                *id,
                VariableInfo {
                    id: *id,
                    path: varibale.path.clone(),
                    var_kind: varibale.kind,
                    packed_dims,
                    metadata: VariableMetadata {
                        width: resolve_total_width(module, varibale)?,
                        is_4state: is_4state_type(&varibale.r#type.kind),
                        kind: type_kind_to_domain_kind(&varibale.r#type.kind, config),
                        type_kind: type_kind_to_port_type_kind(&varibale.r#type.kind, config),
                        array_dims: varibale.r#type.array.iter().filter_map(|d| *d).collect(),
                    },
                },
            );
        }
        res.insert(*id, variables);
        path_index.insert(*id, paths);
    }
    Ok((res, path_index))
}

fn type_kind_to_port_type_kind(
    kind: &veryl_analyzer::ir::TypeKind,
    config: &BuildConfig,
) -> PortTypeKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ClockNegedge => PortTypeKind::Clock,
        TypeKind::Reset => match config.reset_type {
            ResetType::AsyncHigh => PortTypeKind::ResetAsyncHigh,
            ResetType::AsyncLow => PortTypeKind::ResetAsyncLow,
            ResetType::SyncHigh => PortTypeKind::ResetSyncHigh,
            ResetType::SyncLow => PortTypeKind::ResetSyncLow,
        },
        TypeKind::ResetAsyncHigh => PortTypeKind::ResetAsyncHigh,
        TypeKind::ResetAsyncLow => PortTypeKind::ResetAsyncLow,
        TypeKind::ResetSyncHigh => PortTypeKind::ResetSyncHigh,
        TypeKind::ResetSyncLow => PortTypeKind::ResetSyncLow,
        TypeKind::Logic => PortTypeKind::Logic,
        TypeKind::Bit => PortTypeKind::Bit,
        _ => PortTypeKind::Other,
    }
}

fn type_kind_to_domain_kind(
    kind: &veryl_analyzer::ir::TypeKind,
    config: &BuildConfig,
) -> DomainKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock => match config.clock_type {
            ClockType::PosEdge => DomainKind::ClockPosedge,
            ClockType::NegEdge => DomainKind::ClockNegedge,
        },
        TypeKind::ClockPosedge => DomainKind::ClockPosedge,
        TypeKind::ClockNegedge => DomainKind::ClockNegedge,
        TypeKind::Reset => match config.reset_type {
            ResetType::AsyncHigh => DomainKind::ResetAsyncHigh,
            ResetType::AsyncLow => DomainKind::ResetAsyncLow,
            ResetType::SyncHigh | ResetType::SyncLow => DomainKind::Other,
        },
        TypeKind::ResetAsyncHigh => DomainKind::ResetAsyncHigh,
        TypeKind::ResetAsyncLow => DomainKind::ResetAsyncLow,
        _ => DomainKind::Other,
    }
}

fn is_4state_type(kind: &veryl_analyzer::ir::TypeKind) -> bool {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock
        | TypeKind::ClockPosedge
        | TypeKind::ClockNegedge
        | TypeKind::Reset
        | TypeKind::ResetAsyncHigh
        | TypeKind::ResetAsyncLow
        | TypeKind::ResetSyncHigh
        | TypeKind::ResetSyncLow
        | TypeKind::Logic => true,
        TypeKind::Struct(x) => x.members.iter().any(|m| is_4state_type(&m.r#type.kind)),
        TypeKind::Union(x) => x.members.iter().any(|m| is_4state_type(&m.r#type.kind)),
        TypeKind::Enum(x) => is_4state_type(&x.r#type.kind),
        _ => false,
    }
}

fn expand_hierarchy(
    top: &ModuleId,
    modules: &HashMap<ModuleId, SimModule>,
) -> (
    HashMap<InstancePath, InstanceId>,
    HashMap<InstanceId, ModuleId>,
) {
    let mut expanded = HashMap::default();
    let mut instance_modules = HashMap::default();
    let mut instance_id = 0;
    let path = vec![];
    let id = InstanceId(instance_id);
    instance_modules.insert(id, *top);
    expanded.insert(InstancePath(path.clone()), id);
    instance_id += 1;
    expand(
        top,
        path,
        modules,
        &mut expanded,
        &mut instance_modules,
        &mut instance_id,
    );
    (expanded, instance_modules)
}

fn propagate_boundaries(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>> {
    let mut current_boundaries = HashMap::default();

    // Initialize with local boundaries
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, boundaries) in &sim_module.comb_boundaries {
            let addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *var_id,
            };
            current_boundaries.insert(addr, boundaries.clone());
        }
    }

    // Propagate boundaries
    let mut changed = true;
    while changed {
        changed = false;
        for (path, id) in expanded {
            let module_id = &instance_modules[id];
            let sim_module = &modules[module_id];

            for (inst_name, glue_blocks) in &sim_module.glue_blocks {
                for (idx, glue_block) in glue_blocks.iter().enumerate() {
                    let mut child_path = path.0.clone();
                    child_path.push((*inst_name, idx));
                    let child_id = expanded[&InstancePath(child_path)];

                    // Propagate from Parent to Child (Input Ports)
                    for (parent_vars, child_addr) in &glue_block.input_ports {
                        if let Some(target) = child_addr.target.var()
                            && let GlueAddr::Child(child_var_id) = target.id
                        {
                            let child_abs = AbsoluteAddr {
                                instance_id: child_id,
                                var_id: child_var_id,
                            };

                            // Collect boundaries from all parent variables connected to this port
                            let mut incoming_boundaries = std::collections::BTreeSet::new();
                            for parent_var in parent_vars {
                                let parent_abs = AbsoluteAddr {
                                    instance_id: *id,
                                    var_id: *parent_var,
                                };
                                if let Some(bounds) = current_boundaries.get(&parent_abs) {
                                    for b in bounds {
                                        incoming_boundaries.insert(*b);
                                    }
                                }
                            }

                            // Apply to child
                            if !incoming_boundaries.is_empty() {
                                let child_bounds = current_boundaries.entry(child_abs).or_default();
                                let old_len = child_bounds.len();
                                child_bounds.extend(incoming_boundaries);
                                if child_bounds.len() != old_len {
                                    changed = true;
                                }
                            }
                        }
                    }

                    // Propagate from Child to Parent (Output Ports)
                    for (parent_vars, logic_path) in &glue_block.output_ports {
                        // logic_path.target is Parent. logic_path.sources contains Child.
                        for source in &logic_path.sources {
                            if let GlueAddr::Child(child_var_id) = source.id {
                                let child_abs = AbsoluteAddr {
                                    instance_id: child_id,
                                    var_id: child_var_id,
                                };

                                // Child -> Parent
                                if let Some(child_bounds) =
                                    current_boundaries.get(&child_abs).cloned()
                                {
                                    for parent_var in parent_vars {
                                        let parent_abs = AbsoluteAddr {
                                            instance_id: *id,
                                            var_id: *parent_var,
                                        };
                                        let parent_bounds =
                                            current_boundaries.entry(parent_abs).or_default();
                                        let old_len = parent_bounds.len();
                                        parent_bounds.extend(child_bounds.clone());
                                        if parent_bounds.len() != old_len {
                                            changed = true;
                                        }
                                    }
                                }

                                // Parent -> Child (Sink -> Source propagation)
                                // If the parent wire connected to this output has boundaries (e.g. used in slices),
                                // those boundaries should propagate to the child output port so it drives them appropriately.
                                let mut incoming_boundaries = std::collections::BTreeSet::new();
                                for parent_var in parent_vars {
                                    let parent_abs = AbsoluteAddr {
                                        instance_id: *id,
                                        var_id: *parent_var,
                                    };
                                    if let Some(bounds) = current_boundaries.get(&parent_abs) {
                                        for b in bounds {
                                            incoming_boundaries.insert(*b);
                                        }
                                    }
                                }

                                if !incoming_boundaries.is_empty() {
                                    let child_bounds =
                                        current_boundaries.entry(child_abs).or_default();
                                    let old_len = child_bounds.len();
                                    child_bounds.extend(incoming_boundaries);
                                    if child_bounds.len() != old_len {
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    current_boundaries
}

fn expand(
    target: &ModuleId,
    path: Vec<(StrId, usize)>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &mut HashMap<InstancePath, InstanceId>,
    instance_modules: &mut HashMap<InstanceId, ModuleId>,
    instance_id: &mut usize,
) {
    let module = &modules[target];
    for (inst_name, gbs) in &module.glue_blocks {
        for (idx, gb) in gbs.iter().enumerate() {
            let mut path = path.clone();
            path.push((*inst_name, idx));
            let id = InstanceId(*instance_id);
            expanded.insert(InstancePath(path.clone()), id);
            instance_modules.insert(id, gb.module_id);
            *instance_id += 1;
            expand(
                &gb.module_id,
                path,
                modules,
                expanded,
                instance_modules,
                instance_id,
            );
        }
    }
}

fn relocate_executation_unit_with_errors<A, B>(
    eu: &ExecutionUnit<A>,
    f: &impl Fn(&A) -> B,
    runtime_error_codes: &HashMap<i64, i64>,
    runtime_event_sites: &HashMap<u32, u32>,
) -> ExecutionUnit<B> {
    ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .iter()
            .map(|(id, block)| {
                (
                    *id,
                    BasicBlock {
                        id: block.id,
                        instructions: block
                            .instructions
                            .iter()
                            .map(|inst| match inst {
                                SIRInstruction::RuntimeEvent { site_id, args } => {
                                    SIRInstruction::RuntimeEvent {
                                        site_id: runtime_event_sites
                                            .get(site_id)
                                            .copied()
                                            .unwrap_or(*site_id),
                                        args: args.clone(),
                                    }
                                }
                                SIRInstruction::CombCaptureEvent {
                                    site_id,
                                    args,
                                    fatal_error_code,
                                    consume_enabled,
                                } => SIRInstruction::CombCaptureEvent {
                                    site_id: runtime_event_sites
                                        .get(site_id)
                                        .copied()
                                        .unwrap_or(*site_id),
                                    args: args.clone(),
                                    fatal_error_code: *fatal_error_code,
                                    consume_enabled: *consume_enabled,
                                },
                                _ => inst.map_addr(f),
                            })
                            .collect(),
                        params: block.params.clone(),
                        terminator: match block.terminator {
                            SIRTerminator::Error(code) => SIRTerminator::Error(
                                runtime_error_codes.get(&code).copied().unwrap_or(code),
                            ),
                            ref terminator => terminator.clone(),
                        },
                    },
                )
            })
            .collect(),
        register_map: eu.register_map.clone(),
    }
}

fn unify_clock_domains(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];

        // Internal aliases (e.g. `assign clk_internal = clk_port;`)
        for logic_path in &sim_module.comb_blocks {
            // Only unify direct aliases, not complex logic like gated clocks
            if logic_path.sources.len() == 1 {
                let expr_node = sim_module.arena.get(logic_path.expr);
                let is_alias = matches!(
                    expr_node,
                    celox_slt::SLTNode::Input { .. } | celox_slt::SLTNode::Slice { .. }
                );
                if is_alias {
                    let Some(target) = logic_path.target.var() else {
                        continue;
                    };
                    let target_abs = AbsoluteAddr {
                        instance_id: *id,
                        var_id: target.id,
                    };
                    let source_abs = AbsoluteAddr {
                        instance_id: *id,
                        var_id: logic_path.sources.iter().next().unwrap().id,
                    };
                    drive_graph.entry(source_abs).or_default().push(target_abs);
                }
            }
        }
        for (inst_name, glue_blocks) in &sim_module.glue_blocks {
            for (idx, glue_block) in glue_blocks.iter().enumerate() {
                let mut child_path = path.0.clone();
                child_path.push((*inst_name, idx));
                let child_id = expanded[&InstancePath(child_path)];

                // Inputs: Parent -> Child (Parent drives Child)
                for (parent_vars, logic_path) in &glue_block.input_ports {
                    if let Some(target) = logic_path.target.var()
                        && let GlueAddr::Child(child_var_id) = target.id
                    {
                        let child_abs = AbsoluteAddr {
                            instance_id: child_id,
                            var_id: child_var_id,
                        };
                        for parent_var in parent_vars {
                            let parent_abs = AbsoluteAddr {
                                instance_id: *id,
                                var_id: *parent_var,
                            };
                            drive_graph.entry(parent_abs).or_default().push(child_abs);
                        }
                    }
                }
                // Outputs: Child -> Parent (Child drives Parent)
                for (parent_vars, logic_path) in &glue_block.output_ports {
                    for parent_var in parent_vars {
                        let parent_abs = AbsoluteAddr {
                            instance_id: *id,
                            var_id: *parent_var,
                        };
                        for source in &logic_path.sources {
                            if let GlueAddr::Child(child_var_id) = source.id {
                                let child_abs = AbsoluteAddr {
                                    instance_id: child_id,
                                    var_id: child_var_id,
                                };
                                drive_graph.entry(child_abs).or_default().push(parent_abs);
                            }
                        }
                    }
                }
            }
        }
    }

    // Resolve Canonical Clock Domains: Find the root driver for each connected component
    let mut clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();

    // Reverse the drive graph to find roots (Sink -> Sources)
    let mut reverse_drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();
    for (src, sinks) in &drive_graph {
        for sink in sinks {
            reverse_drive_graph.entry(*sink).or_default().push(*src);
        }
    }

    // Collect all unique addresses involved in any drive relationship
    let mut all_addrs = HashSet::default();
    for src in drive_graph.keys() {
        all_addrs.insert(*src);
    }
    for sinks in drive_graph.values() {
        for sink in sinks {
            all_addrs.insert(*sink);
        }
    }

    // Assign each address its canonical root driver
    for addr in all_addrs {
        let mut current = addr;
        let mut visited = HashSet::default();
        // Traverse upwards towards the root driver
        while let Some(sources) = reverse_drive_graph.get(&current) {
            if sources.is_empty() {
                break;
            }
            // In a valid hardware design, a clock net usually has 1 driver.
            // If multiple, we just pick the first for canonicalization.
            let next = sources[0];
            if visited.contains(&next) {
                break; // Prevent infinite loop in case of bad combinational loop
            }
            visited.insert(next);
            current = next;
        }
        clock_domains.insert(addr, current);
    }
    clock_domains
}

fn relocate_units(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    trace_opts: &crate::FrontendTraceOptions,
    trace: &mut Option<&mut crate::FrontendTrace>,
) -> Result<
    (
        SLTNodeArena<AbsoluteAddr>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<FfAccessSummary<RegionedAbsoluteAddr>>>,
        HashMap<InstanceId, FfRuntimeRelocation>,
        Vec<celox_slt::LogicPath<AbsoluteAddr>>,
        Vec<CombObserver<AbsoluteAddr>>,
        HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
        Vec<RuntimeEventSite>,
        i64,
    ),
    ParserError,
> {
    let mut global_arena = SLTNodeArena::<AbsoluteAddr>::new();
    let mut eval_apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut eval_only_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut apply_ffs: HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut ff_access_summaries: HashMap<AbsoluteAddr, Vec<FfAccessSummary<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut ff_runtime_relocations = HashMap::default();
    let mut comb_blocks = Vec::new();
    let mut comb_observers = Vec::new();
    let mut runtime_errors = HashMap::default();
    let mut runtime_event_sites = Vec::new();
    let mut next_runtime_error_code = 2000;

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        let runtime_event_site_base = u32::try_from(runtime_event_sites.len()).map_err(|_| {
            ParserError::illegal_context(
                "FF runtime-event relocation",
                "runtime event site count exceeds u32",
                None,
            )
        })?;
        let relocate_ff_summary = |summary: &FfAccessSummary<RegionedVarAddr>| {
            let relocate_addr = |addr: RegionedVarAddr| RegionedAbsoluteAddr {
                region: addr.region,
                instance_id: *id,
                var_id: addr.var_id,
            };
            FfAccessSummary {
                reads: summary
                    .reads
                    .iter()
                    .map(|read| VarAtomBase {
                        id: relocate_addr(read.id),
                        access: read.access,
                    })
                    .collect(),
                writes: summary
                    .writes
                    .iter()
                    .map(|write| VarAtomBase {
                        id: relocate_addr(write.id),
                        access: write.access,
                    })
                    .collect(),
                dynamic_writes: summary
                    .dynamic_writes
                    .iter()
                    .copied()
                    .map(relocate_addr)
                    .collect(),
            }
        };
        for (trigger_set, summary) in &sim_module.ff_access_summaries {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            ff_access_summaries
                .entry(canonical_clock)
                .or_default()
                .push(relocate_ff_summary(summary));
            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_reset = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                ff_access_summaries
                    .entry(canonical_reset)
                    .or_default()
                    .push(relocate_ff_summary(summary));
            }
        }
        let mut runtime_error_codes = HashMap::default();
        for (&local_code, info) in &sim_module.runtime_errors {
            let global_code = next_runtime_error_code;
            next_runtime_error_code += 1;
            runtime_error_codes.insert(local_code, global_code);
            runtime_errors.insert(
                global_code,
                RuntimeErrorInfo {
                    message: info.message.clone(),
                    signals: info
                        .signals
                        .iter()
                        .filter(|var_id| sim_module.variables.contains_key(var_id))
                        .map(|&var_id| AbsoluteAddr {
                            instance_id: *id,
                            var_id,
                        })
                        .collect(),
                },
            );
        }
        ff_runtime_relocations.insert(
            *id,
            FfRuntimeRelocation {
                error_codes: runtime_error_codes.clone(),
                event_site_base: runtime_event_site_base,
            },
        );
        let mut runtime_event_site_map = HashMap::default();
        for (local_site, site) in sim_module.runtime_event_sites.iter().enumerate() {
            let global_site = runtime_event_sites.len() as u32;
            runtime_event_site_map.insert(local_site as u32, global_site);
            runtime_event_sites.push(site.clone());
        }

        let arena_start = global_arena.len();
        let mut relocated_module = flatten_with_trace(
            sim_module,
            path,
            expanded,
            global_boundaries,
            unpacked_element_widths,
            &mut global_arena,
            trace_opts,
            trace.as_deref_mut(),
        )?;
        remap_for_fold_runtime_event_sites(
            &mut global_arena,
            arena_start,
            &runtime_event_site_map,
        )?;
        for observer in &mut relocated_module.comb_observers {
            observer.site_id = runtime_event_site_map[&observer.site_id];
            observer.activation_group = runtime_event_site_map[&observer.activation_group];
        }
        comb_blocks.extend(relocated_module.comb_blocks);
        comb_observers.extend(relocated_module.comb_observers);

        // Relocate sequential blocks for this instance
        for (trigger_set, eu) in &sim_module.eval_apply_ff_blocks {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);

            eval_apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    },
                    &runtime_error_codes,
                    &runtime_event_site_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        },
                        &runtime_error_codes,
                        &runtime_event_site_map,
                    ),
                );
            }
        }

        for (trigger_set, eu) in &sim_module.eval_only_ff_blocks {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            eval_only_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    },
                    &runtime_error_codes,
                    &runtime_event_site_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_only_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        },
                        &runtime_error_codes,
                        &runtime_event_site_map,
                    ),
                );
            }
        }

        for (trigger_set, eu) in &sim_module.apply_ff_blocks {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    },
                    &runtime_error_codes,
                    &runtime_event_site_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        },
                        &runtime_error_codes,
                        &runtime_event_site_map,
                    ),
                );
            }
        }
    }
    Ok((
        global_arena,
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        ff_access_summaries,
        ff_runtime_relocations,
        comb_blocks,
        comb_observers,
        runtime_errors,
        runtime_event_sites,
        next_runtime_error_code,
    ))
}

fn build_comb_observer_capture_paths(
    comb_blocks: &mut Vec<LogicPath<AbsoluteAddr>>,
    observers: &mut [CombObserver<AbsoluteAddr>],
    sites: &[RuntimeEventSite],
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<(), ParserError> {
    if observers.is_empty() {
        return Ok(());
    }

    annotate_comb_capture_enable_sites(comb_blocks, observers);

    let mut group_members: HashMap<u32, Vec<usize>> = HashMap::default();
    for (idx, observer) in observers.iter().enumerate() {
        group_members
            .entry(observer.activation_group)
            .or_default()
            .push(idx);
    }
    let mut emitted_group_triggers = HashSet::default();
    let mut previous_primary_capture_path: Option<LogicPathId> = None;
    let mut previous_trigger_capture_path: Option<LogicPathId> = None;
    for observer_idx in 0..observers.len() {
        let observer = &observers[observer_idx];
        let has_statement_position_dependency =
            observer_has_statement_position_dependency(comb_blocks, observer);
        let order_before = observer_order_before(comb_blocks, observer);
        let order_after = observer_order_after(comb_blocks, observer);
        let trigger_paths = if has_statement_position_dependency {
            observer_trigger_paths(comb_blocks, observer)
        } else {
            Vec::new()
        };
        if observer.captured_in_loop {
            let Some(loop_runner) = observer.loop_runner else {
                continue;
            };
            let sources: HashSet<_> = observer
                .sensitivity
                .iter()
                .copied()
                .filter(|atom| !observer_written_input_overlaps(observer, atom))
                .filter(|atom| !observer_statement_position_overlaps(comb_blocks, observer, atom))
                .collect();
            let path_id = LogicPathId(comb_blocks.len());
            if let Some(prev) = previous_primary_capture_path {
                comb_blocks[prev.0].order_before.insert(path_id);
            }
            for idx in &order_after {
                comb_blocks[idx.0].order_before.insert(path_id);
            }
            comb_blocks.push(LogicPath {
                target: LogicPathTarget::CombCaptureEvent {
                    site_id: observer.site_id,
                    guard: None,
                    emit_on_true: true,
                    args: Vec::new(),
                    loop_runner: Some(loop_runner),
                    fatal_error_code: None,
                    consume_enabled: true,
                },
                sources,
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: observer.local_inputs.clone(),
                order_before: order_before.clone(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr: loop_runner,
            });
            previous_primary_capture_path = Some(path_id);
            for trigger_idx in trigger_paths {
                let Some(trigger_target) = comb_blocks[trigger_idx.0].target.var().copied() else {
                    continue;
                };
                let trigger_order_before =
                    direct_consumers_of_path_target(comb_blocks, trigger_idx);
                let path_id = LogicPathId(comb_blocks.len());
                if let Some(prev) = previous_trigger_capture_path {
                    comb_blocks[prev.0].order_before.insert(path_id);
                }
                comb_blocks[trigger_idx.0].order_before.insert(path_id);
                comb_blocks.push(LogicPath {
                    target: LogicPathTarget::CombCaptureEvent {
                        site_id: observer.site_id,
                        guard: None,
                        emit_on_true: true,
                        args: Vec::new(),
                        loop_runner: Some(loop_runner),
                        fatal_error_code: None,
                        consume_enabled: true,
                    },
                    sources: std::iter::once(trigger_target).collect(),
                    previous_sources: HashSet::default(),
                    address_sources: HashSet::default(),
                    local_inputs: observer.local_inputs.clone(),
                    order_before: trigger_order_before,
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr: loop_runner,
                });
                previous_trigger_capture_path = Some(path_id);
            }
            continue;
        }
        let local_input_ids: HashSet<_> = observer
            .local_inputs
            .iter()
            .map(|(addr, _)| *addr)
            .collect();
        let mut sources: HashSet<_> = observer
            .observed_inputs
            .iter()
            .copied()
            .filter(|atom| !observer_written_input_overlaps(observer, atom))
            .filter(|atom| !local_input_ids.contains(&atom.id))
            .filter(|atom| !observer_statement_position_overlaps(comb_blocks, observer, atom))
            .collect();
        for (_, node) in &observer.local_inputs {
            let mut local_sources = HashSet::default();
            crate::flattening::collect_inputs(*node, arena, &mut local_sources);
            sources.extend(
                local_sources
                    .into_iter()
                    .filter(|atom| !observer_written_input_overlaps(observer, atom))
                    .filter(|atom| !local_input_ids.contains(&atom.id))
                    .filter(|atom| {
                        !observer_statement_position_overlaps(comb_blocks, observer, atom)
                    }),
            );
        }
        let expr = match observer.guard.or_else(|| observer.args.first().copied()) {
            Some(expr) => expr,
            None => arena.alloc(celox_slt::SLTNode::Constant(
                num_bigint::BigUint::from(1u8),
                num_bigint::BigUint::from(0u8),
                1,
                false,
            ))?,
        };
        let emit_on_true = matches!(
            sites[observer.site_id as usize].kind,
            RuntimeEventKind::Display
        );
        let fatal_error_code = matches!(
            sites[observer.site_id as usize].kind,
            RuntimeEventKind::AssertFatal
        )
        .then_some(observer.site_id as i64);
        let pre_lower_nodes = observer_pre_lower_nodes(observer, arena);
        for idx in &order_after {
            comb_blocks[idx.0]
                .pre_lower_nodes
                .extend(pre_lower_nodes.iter().copied());
        }
        let path_id = LogicPathId(comb_blocks.len());
        if let Some(prev) = previous_primary_capture_path {
            comb_blocks[prev.0].order_before.insert(path_id);
        }
        for idx in &order_after {
            comb_blocks[idx.0].order_before.insert(path_id);
        }
        comb_blocks.push(LogicPath {
            target: LogicPathTarget::CombCaptureEvent {
                site_id: observer.site_id,
                guard: observer.guard,
                emit_on_true,
                args: observer.args.clone(),
                loop_runner: None,
                fatal_error_code,
                consume_enabled: !trigger_paths.is_empty(),
            },
            sources,
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: observer.local_inputs.clone(),
            order_before,
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        });
        previous_primary_capture_path = Some(path_id);
        for trigger_idx in trigger_paths {
            if !emitted_group_triggers.insert((observer.activation_group, trigger_idx)) {
                continue;
            }
            let Some(trigger_target) = comb_blocks[trigger_idx.0].target.var().copied() else {
                continue;
            };
            let trigger_order_before = direct_consumers_of_path_target(comb_blocks, trigger_idx);
            for &member_idx in &group_members[&observer.activation_group] {
                let member = &observers[member_idx];
                let member_emit_on_true = matches!(
                    sites[member.site_id as usize].kind,
                    RuntimeEventKind::Display
                );
                let member_fatal_error_code = matches!(
                    sites[member.site_id as usize].kind,
                    RuntimeEventKind::AssertFatal
                )
                .then_some(member.site_id as i64);
                let member_expr = match member
                    .loop_runner
                    .or(member.guard)
                    .or_else(|| member.args.first().copied())
                {
                    Some(expr) => expr,
                    None => arena.alloc(celox_slt::SLTNode::Constant(
                        num_bigint::BigUint::from(1u8),
                        num_bigint::BigUint::from(0u8),
                        1,
                        false,
                    ))?,
                };
                let path_id = LogicPathId(comb_blocks.len());
                if let Some(prev) = previous_trigger_capture_path {
                    comb_blocks[prev.0].order_before.insert(path_id);
                }
                comb_blocks[trigger_idx.0].order_before.insert(path_id);
                comb_blocks.push(LogicPath {
                    target: LogicPathTarget::CombCaptureEvent {
                        site_id: member.site_id,
                        guard: member.guard,
                        emit_on_true: member_emit_on_true,
                        args: member.args.clone(),
                        loop_runner: member.loop_runner,
                        fatal_error_code: member_fatal_error_code,
                        consume_enabled: true,
                    },
                    sources: std::iter::once(trigger_target).collect(),
                    previous_sources: HashSet::default(),
                    address_sources: HashSet::default(),
                    local_inputs: member.local_inputs.clone(),
                    order_before: trigger_order_before.clone(),
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr: member_expr,
                });
                previous_trigger_capture_path = Some(path_id);
            }
        }
    }
    Ok(())
}

fn apply_always_comb_previous_source_ordering(comb_blocks: &mut [LogicPath<AbsoluteAddr>]) {
    let targets: Vec<_> = comb_blocks
        .iter()
        .map(|path| path.target.var().copied())
        .collect();

    for (idx, path) in comb_blocks.iter_mut().enumerate() {
        if path.previous_sources.is_empty() {
            continue;
        }

        let previous_sources = path.previous_sources.clone();
        let address_sources = path.address_sources.clone();
        path.sources.retain(|source| {
            let is_previous = previous_sources.iter().any(|previous| {
                previous.id == source.id && previous.access.overlaps(&source.access)
            });
            let is_address = address_sources
                .iter()
                .any(|address| address.id == source.id && address.access.overlaps(&source.access));
            !is_previous || is_address
        });

        let mut order_before = Vec::new();
        for (target_idx, target) in targets.iter().enumerate() {
            if target_idx == idx {
                continue;
            }
            let Some(target) = target else {
                continue;
            };
            if previous_sources.iter().any(|previous| {
                previous.id == target.id && previous.access.overlaps(&target.access)
            }) {
                order_before.push(LogicPathId(target_idx));
            }
        }
        path.order_before.extend(order_before);
    }
}

fn observer_written_input_overlaps(
    observer: &CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    observer
        .written_input_atoms
        .iter()
        .any(|written| written.id == atom.id && written.access.overlaps(&atom.access))
}

fn observer_statement_position_overlaps(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    atom_overlaps_any(atom, observer_affected_by_preceding_writes(paths, observer))
}

fn observer_has_statement_position_dependency(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> bool {
    if observer.preceding_writes.is_empty() {
        return false;
    }
    let affected = observer_affected_by_preceding_writes(paths, observer);
    observer
        .position_inputs
        .iter()
        .chain(observer.observed_inputs.iter())
        .any(|input| atom_overlaps_any(input, &affected))
}

fn observer_trigger_paths(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> Vec<LogicPathId> {
    let mut seen_targets = HashSet::default();
    let affected = observer_affected_by_preceding_writes(paths, observer);
    paths
        .iter()
        .enumerate()
        .filter_map(|(idx, path)| {
            let target = path.target.var()?;
            if observer_written_input_overlaps(observer, target) {
                return None;
            }
            let matches_observer_operand = observer
                .position_inputs
                .iter()
                .chain(observer.observed_inputs.iter())
                .any(|atom| target.id == atom.id && target.access.overlaps(&atom.access));
            if !matches_observer_operand
                || !atom_overlaps_any(target, &affected)
                || !seen_targets.insert((target.id, target.access.lsb, target.access.msb))
            {
                return None;
            }
            Some(LogicPathId(idx))
        })
        .collect()
}

fn observer_pre_lower_nodes(
    observer: &CombObserver<AbsoluteAddr>,
    arena: &SLTNodeArena<AbsoluteAddr>,
) -> Vec<NodeId> {
    let local_input_ids: HashSet<_> = observer.local_inputs.iter().map(|(id, _)| *id).collect();
    let mut nodes = Vec::with_capacity(observer.args.len() + usize::from(observer.guard.is_some()));
    if let Some(guard) = observer.guard {
        nodes.push(guard);
    }
    nodes.extend(observer.args.iter().copied());
    nodes.extend(observer.local_inputs.iter().filter_map(|(_, node)| {
        matches!(arena.get(*node), celox_slt::SLTNode::Capture { .. }).then_some(*node)
    }));
    nodes
        .into_iter()
        .filter(|node| {
            let mut inputs = HashSet::default();
            crate::flattening::collect_inputs(*node, arena, &mut inputs);
            // A capture independent of local bindings can be materialized at
            // its statement position. Bound captures still need the event's
            // environment, unless they contain an earlier formal snapshot.
            // Constants need no early materialization.
            !inputs.is_empty()
                && (capture_contains_nested_snapshot(*node, arena)
                    || inputs
                        .iter()
                        .all(|input| !local_input_ids.contains(&input.id)))
        })
        .collect()
}

fn capture_contains_nested_snapshot(node: NodeId, arena: &SLTNodeArena<AbsoluteAddr>) -> bool {
    let celox_slt::SLTNode::Capture { expr, .. } = arena.get(node) else {
        return false;
    };
    let mut work = vec![*expr];
    let mut visited = HashSet::default();
    while let Some(node) = work.pop() {
        if !visited.insert(node) {
            continue;
        }
        match arena.get(node) {
            celox_slt::SLTNode::Capture { .. } => return true,
            celox_slt::SLTNode::Input { index, .. } => {
                work.extend(index.iter().map(|entry| entry.node));
            }
            celox_slt::SLTNode::Constant(..) => {}
            celox_slt::SLTNode::Binary(lhs, _, rhs) => {
                work.push(*lhs);
                work.push(*rhs);
            }
            celox_slt::SLTNode::Unary(_, inner) => work.push(*inner),
            celox_slt::SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                work.push(*cond);
                work.push(*then_expr);
                work.push(*else_expr);
            }
            celox_slt::SLTNode::Concat(parts) => {
                work.extend(parts.iter().map(|(part, _)| *part));
            }
            celox_slt::SLTNode::Slice { expr, .. } => work.push(*expr),
            celox_slt::SLTNode::ForFold {
                start,
                end,
                result,
                initials,
                updates,
                effects,
                continue_cond,
                ..
            } => {
                if let celox_slt::SLTLoopBound::Expr(node) = start {
                    work.push(*node);
                }
                if let celox_slt::SLTLoopBound::Expr(node) = end {
                    work.push(*node);
                }
                if let celox_slt::SLTForFoldResult::Transient { initial, update } = result {
                    work.push(*initial);
                    work.push(*update);
                }
                work.extend(initials.iter().map(|state| state.expr));
                work.extend(updates.iter().map(|state| state.expr));
                for effect in effects {
                    match effect {
                        celox_slt::SLTForEffect::Event { guard, args, .. } => {
                            work.extend(*guard);
                            work.extend(args.iter().copied());
                        }
                        celox_slt::SLTForEffect::Runner(runner) => work.push(*runner),
                    }
                }
                work.push(*continue_cond);
            }
            celox_slt::SLTNode::ForFoldGroup {
                entry_guard,
                states,
                ..
            } => {
                work.push(*entry_guard);
                for state in states {
                    work.push(state.initial);
                    work.push(state.update);
                }
            }
        }
    }
    false
}

fn annotate_comb_capture_enable_sites(
    comb_blocks: &mut [LogicPath<AbsoluteAddr>],
    observers: &[CombObserver<AbsoluteAddr>],
) {
    let mut group_sites: HashMap<u32, Vec<u32>> = HashMap::default();
    for observer in observers {
        group_sites
            .entry(observer.activation_group)
            .or_default()
            .push(observer.site_id);
    }
    for observer in observers {
        for atom in &observer.sensitivity {
            for path in comb_blocks.iter_mut() {
                let Some(target) = path.target.var() else {
                    continue;
                };
                if target.id == atom.id && target.access.overlaps(&atom.access) {
                    for site_id in &group_sites[&observer.activation_group] {
                        if !path.comb_capture_enable_sites.contains(site_id) {
                            path.comb_capture_enable_sites.push(*site_id);
                        }
                    }
                }
            }
        }
    }
}

fn observer_order_after(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> HashSet<LogicPathId> {
    let mut result = HashSet::default();
    if !observer_has_statement_position_dependency(paths, observer) {
        return result;
    }
    for written in &observer.preceding_writes {
        for (idx, path) in paths.iter().enumerate() {
            let Some(target) = path.target.var() else {
                continue;
            };
            if target.id == written.id && target.access.overlaps(&written.access) {
                result.insert(LogicPathId(idx));
            }
        }
    }
    result
}

fn observer_order_before(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> HashSet<LogicPathId> {
    let preceding_writes = observer.preceding_writes.iter().collect::<Vec<_>>();
    let affected_by_preceding_writes = observer_has_statement_position_dependency(paths, observer)
        .then(|| observer_affected_by_preceding_writes(paths, observer));
    let mut result = HashSet::default();
    for (idx, path) in paths.iter().enumerate() {
        let Some(target) = path.target.var() else {
            continue;
        };
        let already_written = preceding_writes
            .iter()
            .any(|written| target.id == written.id && target.access.overlaps(&written.access));
        let is_later_observed_write = observer_written_input_overlaps(observer, target);
        let is_later_affected_write = affected_by_preceding_writes
            .as_ref()
            .is_some_and(|affected| atom_overlaps_any(target, affected));
        if !already_written && (is_later_observed_write || is_later_affected_write) {
            result.insert(LogicPathId(idx));
        }
    }
    result
}

/// Place a trigger-capture between the write which activated it and each
/// immediate dataflow consumer of that write. Transitive consumers remain
/// ordered by the ordinary LogicPath dependency graph.
fn direct_consumers_of_path_target(
    paths: &[LogicPath<AbsoluteAddr>],
    trigger: LogicPathId,
) -> HashSet<LogicPathId> {
    let Some(target) = paths.get(trigger.0).and_then(|path| path.target.var()) else {
        return HashSet::default();
    };
    paths
        .iter()
        .enumerate()
        .filter_map(|(index, path)| {
            (index != trigger.0
                && path
                    .sources
                    .iter()
                    .any(|source| source.id == target.id && source.access.overlaps(&target.access)))
            .then_some(LogicPathId(index))
        })
        .collect()
}

fn observer_affected_by_preceding_writes(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &CombObserver<AbsoluteAddr>,
) -> HashSet<VarAtomBase<AbsoluteAddr>> {
    let mut affected: HashSet<VarAtomBase<AbsoluteAddr>> =
        observer.preceding_writes.iter().copied().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for path in paths {
            let Some(target) = path.target.var() else {
                continue;
            };
            if !path
                .sources
                .iter()
                .any(|source| atom_overlaps_any(source, &affected))
            {
                continue;
            }
            if affected.insert(*target) {
                changed = true;
            }
        }
    }
    affected
}

fn atom_overlaps_any<A: Eq + std::hash::Hash + Copy>(
    atom: &VarAtomBase<A>,
    atoms: impl IntoIterator<Item = impl std::borrow::Borrow<VarAtomBase<A>>>,
) -> bool {
    atoms.into_iter().any(|other| {
        let other = other.borrow();
        atom.id == other.id && atom.access.overlaps(&other.access)
    })
}

fn analyze_clock_dependencies(
    eval_apply_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    eval_only_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    apply_ffs: &mut HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    config: &BuildConfig,
) -> (Vec<AbsoluteAddr>, BTreeSet<AbsoluteAddr>) {
    // Build static clock dependency graph & Topo Sort
    let mut clock_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    let mut unique_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();

    // 1. Identify all variables written by FFs (direct sequential outputs)
    let mut ff_outputs: BTreeSet<AbsoluteAddr> = BTreeSet::new();
    unique_clocks.extend(eval_apply_ffs.keys().copied());

    // Include event-typed signals even when no FF is directly driven by them.
    // A testbench clock may only feed a combinationally gated clock, in which
    // case it would otherwise be absent from the dependency graph entirely.
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, var) in &sim_module.variables {
            let kind = type_kind_to_domain_kind(&var.r#type.kind, config);
            if !matches!(
                kind,
                DomainKind::ClockPosedge
                    | DomainKind::ClockNegedge
                    | DomainKind::ResetAsyncHigh
                    | DomainKind::ResetAsyncLow
            ) {
                continue;
            }
            let addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *var_id,
            };
            let canonical = clock_domains.get(&addr).copied().unwrap_or(addr);
            unique_clocks.insert(canonical);
            eval_apply_ffs.entry(canonical).or_default();
            eval_only_ffs.entry(canonical).or_default();
            apply_ffs.entry(canonical).or_default();
        }
    }

    for (domain_clock, eus) in &*eval_apply_ffs {
        for eu in eus {
            for bb in eu.blocks.values() {
                for inst in &bb.instructions {
                    if let SIRInstruction::Store(target_addr, ..) = inst {
                        // Direct sequential dependency: the target is driven by this clock
                        let abs = target_addr.absolute_addr();
                        let canonical_target = clock_domains.get(&abs).copied().unwrap_or(abs);

                        ff_outputs.insert(abs);

                        if canonical_target != *domain_clock
                            && unique_clocks.contains(&canonical_target)
                        {
                            clock_deps
                                .entry(canonical_target)
                                .or_default()
                                .insert(*domain_clock);
                        }
                    }
                }
            }
        }
    }

    // 2. Build combinational dependency graph (target -> sources)
    let acd_timing = tracing::enabled!(tracing::Level::DEBUG);
    let acd_start = acd_timing.then(std::time::Instant::now);
    let mut comb_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        let target_abs = target.id;
        let mut sources = HashSet::default();
        crate::flattening::collect_inputs(path.expr, arena, &mut sources);
        for source in sources {
            comb_deps.entry(target_abs).or_default().insert(source.id);
        }
    }
    if let Some(s) = acd_start {
        tracing::debug!(
            "[acd] comb_deps build ({} blocks): {:?}",
            comb_deps.len(),
            s.elapsed()
        );
    }

    // Record clock-to-clock dependencies through combinational logic.  FF
    // propagation below finds divided clocks, but a plain gated clock such as
    // `gated_clk = clk & enable` has no FF source and needs this separate walk.
    fn collect_upstream_clocks(
        node: AbsoluteAddr,
        target_clock: AbsoluteAddr,
        comb_deps: &BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>>,
        clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
        clocks: &BTreeSet<AbsoluteAddr>,
        visited: &mut BTreeSet<AbsoluteAddr>,
        found: &mut BTreeSet<AbsoluteAddr>,
    ) {
        if !visited.insert(node) {
            return;
        }
        let Some(sources) = comb_deps.get(&node) else {
            return;
        };
        for source in sources {
            let canonical = clock_domains.get(source).copied().unwrap_or(*source);
            if canonical != target_clock && clocks.contains(&canonical) {
                found.insert(canonical);
            }
            collect_upstream_clocks(
                *source,
                target_clock,
                comb_deps,
                clock_domains,
                clocks,
                visited,
                found,
            );
        }
    }

    for target_clock in &unique_clocks {
        let mut sources = BTreeSet::new();
        collect_upstream_clocks(
            *target_clock,
            *target_clock,
            &comb_deps,
            clock_domains,
            &unique_clocks,
            &mut BTreeSet::new(),
            &mut sources,
        );
        if !sources.is_empty() {
            clock_deps.entry(*target_clock).or_default().extend(sources);
        }
    }

    // 3. Propagate FF outputs through combinational graph to find all derived variables
    let fp_start = acd_timing.then(std::time::Instant::now);
    let mut derived_from_ff: BTreeSet<AbsoluteAddr> = ff_outputs.clone();
    let mut changed = true;
    let mut fp_rounds = 0u32;
    while changed {
        changed = false;
        fp_rounds += 1;
        for (target, sources) in &comb_deps {
            if !derived_from_ff.contains(target) {
                // If any source is derived from an FF, the target is too
                if sources.iter().any(|s| derived_from_ff.contains(s)) {
                    derived_from_ff.insert(*target);
                    changed = true;
                }
            }
        }
    }
    if let Some(s) = fp_start {
        tracing::debug!(
            "[acd] fixpoint: {fp_rounds} rounds, {} entries, {:?}",
            comb_deps.len(),
            s.elapsed()
        );
    }

    // 4. Any clock domain that is derived from an FF is a cascaded clock!
    // We add them to a special "pseudo-domain" or just add themselves to trigger cascade marking.
    for clk in &unique_clocks {
        if derived_from_ff.contains(clk) {
            // Self-dependency ensures it appears in `clock_deps.keys()`
            clock_deps.entry(*clk).or_default().insert(*clk);
        }
    }

    // Topologically sort the clock domains
    // Sources (no dependencies) should be evaluated first.
    let mut topological_clocks = Vec::new();
    let mut visited = BTreeSet::new();
    let mut temp_visited = BTreeSet::new();

    fn topo_visit(
        node: AbsoluteAddr,
        deps: &BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>>,
        visited: &mut BTreeSet<AbsoluteAddr>,
        temp_visited: &mut BTreeSet<AbsoluteAddr>,
        result: &mut Vec<AbsoluteAddr>,
    ) {
        if visited.contains(&node) {
            return;
        }
        if temp_visited.contains(&node) {
            // Cycle detected in clock generation, ignore and break cycle for now
            return;
        }
        temp_visited.insert(node);

        if let Some(node_deps) = deps.get(&node) {
            for &dep in node_deps {
                topo_visit(dep, deps, visited, temp_visited, result);
            }
        }

        temp_visited.remove(&node);
        visited.insert(node);
        result.push(node);
    }

    // Ensure all unique clocks mapped in eval_apply_ffs are present in the topo sort
    for &clk in &unique_clocks {
        if !visited.contains(&clk) {
            topo_visit(
                clk,
                &clock_deps,
                &mut visited,
                &mut temp_visited,
                &mut topological_clocks,
            );
        }
    }

    // Include other potential event signals (like synchronous resets) so they can be scheduled
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, var) in &sim_module.variables {
            let kind = type_kind_to_domain_kind(&var.r#type.kind, config);
            let is_trigger = matches!(
                kind,
                DomainKind::ClockPosedge
                    | DomainKind::ClockNegedge
                    | DomainKind::ResetAsyncHigh
                    | DomainKind::ResetAsyncLow
            );
            if is_trigger {
                let addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: *var_id,
                };
                let canonical = clock_domains.get(&addr).copied().unwrap_or(addr);
                // Add empty execution units so it becomes a valid event domain for scheduling
                eval_apply_ffs.entry(canonical).or_default();
                eval_only_ffs.entry(canonical).or_default();
                apply_ffs.entry(canonical).or_default();

                if !visited.contains(&canonical) {
                    topo_visit(
                        canonical,
                        &clock_deps,
                        &mut visited,
                        &mut temp_visited,
                        &mut topological_clocks,
                    );
                }
            }
        }
    }

    let mut cascaded_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();
    for (target, sources) in &clock_deps {
        cascaded_clocks.insert(*target);
        for source in sources {
            cascaded_clocks.insert(*source);
        }
    }

    (topological_clocks, cascaded_clocks)
}
