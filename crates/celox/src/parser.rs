use crate::{HashMap, HashSet, flatting};
use thiserror::Error;

use crate::parser::{module::ModuleParser, registry::ModuleRegistry};
use veryl_analyzer::ir::{Component, VarPath};
use veryl_parser::resource_table::{self, StrId};
pub mod bitaccess;
mod bitslicer;
pub mod ff;
pub mod module;
pub mod registry;
mod scheduler;
use crate::ir::{
    AbsoluteAddr, DomainKind, ExecutionUnit, GlueAddr, InstanceId, InstancePath, Program,
    RegionedAbsoluteAddr, STABLE_REGION, SimModule, VariableInfo,
};
use crate::logic_tree::{LogicPath, SLTNodeArena};
pub use scheduler::SchedulerError;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Error, Debug)]
pub enum ParserError {
    #[error(transparent)]
    Scheduler(SchedulerError<String>),

    #[error("Unsupported in FF lowering: {feature} ({detail})")]
    UnsupportedFFLowering {
        feature: &'static str,
        detail: String,
    },

    #[error("Unsupported in comb lowering: {feature} ({detail})")]
    UnsupportedCombLowering {
        feature: &'static str,
        detail: String,
    },

    #[error("Unsupported in simulator parser: {feature} ({detail})")]
    UnsupportedSimulatorParser {
        feature: &'static str,
        detail: String,
    },
}

pub fn parse_ir(
    ir: &veryl_analyzer::ir::Ir,
) -> Result<(ModuleRegistry<'_>, HashMap<StrId, SimModule>), ParserError> {
    let mut module_registry = ModuleRegistry {
        modules: HashMap::default(),
    };
    // scan module
    for component in &ir.components {
        match component {
            Component::Module(module) => {
                module_registry.modules.insert(module.name, module);
            }
            Component::Interface(_interface) => {
                unreachable!("Interface component must be eliminated before simulator parse_ir")
            }
            Component::SystemVerilog(system_verilog) => {
                return Err(ParserError::UnsupportedSimulatorParser {
                    feature: "systemverilog component",
                    detail: format!("name: {:?}", system_verilog.name),
                });
            }
        }
    }
    // parse module
    let mut modules = HashMap::default();
    for component in &ir.components {
        match component {
            Component::Module(module) => {
                let m = ModuleParser::parse(module, &module_registry)?;
                modules.insert(m.name, m);
            }
            Component::Interface(_interface) => {
                unreachable!("Interface component must be eliminated before simulator parse_ir")
            }
            Component::SystemVerilog(system_verilog) => {
                return Err(ParserError::UnsupportedSimulatorParser {
                    feature: "systemverilog component",
                    detail: format!("name: {:?}", system_verilog.name),
                });
            }
        }
    }

    Ok((module_registry, modules))
}

fn create_absolute_addr(
    instance_path: &[(String, usize)],
    var_path: &[String],
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
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
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
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
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
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
pub(crate) fn flatten(
    top: &StrId,
    registry: &ModuleRegistry,
    modules: HashMap<StrId, SimModule>,
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
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
) -> Result<Program, ParserError> {
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.sim_modules
    {
        t.sim_modules = Some(modules.clone());
    }

    let (expanded, instance_modules) = expand_hierarchy(top, &modules);
    let global_boundaries = propagate_boundaries(&expanded, &instance_modules, &modules);

    let clock_domains = unify_clock_domains(&expanded, &instance_modules, &modules);
    let (global_arena, mut eval_apply_ffs, eval_only_ffs, apply_ffs, comb_blocks) = relocate_units(
        &expanded,
        &instance_modules,
        &modules,
        &global_boundaries,
        &clock_domains,
        trace_opts,
        &mut trace,
    );
    let ignored_loops = parse_ignored_loops(ignored_loops, &instance_modules, &modules, &expanded);
    let true_loops = parse_true_loops(true_loops, &instance_modules, &modules, &expanded);

    let (topological_clocks, cascaded_clocks) = analyze_clock_dependencies(
        &mut eval_apply_ffs,
        &comb_blocks,
        &global_arena,
        &clock_domains,
        &expanded,
        &instance_modules,
        &modules,
    );

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.flattened_comb_blocks
    {
        t.flattened_comb_blocks = Some((comb_blocks.clone(), global_arena.clone()));
    }

    let schduled = scheduler::sort(
        comb_blocks,
        &global_arena,
        &ignored_loops,
        &true_loops,
        four_state,
    )
    .map_err(|e| {
        let program = Program {
            eval_apply_ffs: HashMap::default(),
            eval_only_ffs: HashMap::default(),
            apply_ffs: HashMap::default(),
            eval_comb: Vec::new(),
            instance_ids: expanded.clone(),
            instance_module: instance_modules.clone(),
            module_variables: module_variables(registry),
            clock_domains: HashMap::default(),
            topological_clocks: Vec::new(),
            cascaded_clocks: BTreeSet::new(),
            arena: SLTNodeArena::new(),
            num_events: 0,
        };
        let mut target_arena = SLTNodeArena::new();
        ParserError::Scheduler(e.map_addr(&global_arena, &mut target_arena, &|addr| {
            program.get_path(addr)
        }))
    })?;
    let schduled: Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>> = schduled
        .into_iter()
        .map(|eu| crate::ir::ExecutionUnit {
            entry_block_id: eu.entry_block_id,
            blocks: eu
                .blocks
                .into_iter()
                .map(|(id, bb)| {
                    (
                        id,
                        crate::ir::BasicBlock {
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

    if let Some(t) = trace
        && trace_opts.scheduled_units
    {
        t.scheduled_units = Some(schduled.clone());
    }

    // Conditional Population: only include split blocks if multiple active FF domains exist.
    // This optimization saves JIT resources for simple designs and designs where only one clock is active.
    let active_ff_domains = eval_apply_ffs
        .values()
        .filter(|eus| !eus.is_empty())
        .count();

    let (eval_only_ffs, apply_ffs) = if active_ff_domains > 1 {
        (eval_only_ffs, apply_ffs)
    } else {
        (HashMap::default(), HashMap::default())
    };

    let num_events = topological_clocks.len();
    let program = Program {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        eval_comb: schduled,
        instance_ids: expanded,
        instance_module: instance_modules,
        module_variables: module_variables(registry),
        clock_domains,
        topological_clocks,
        cascaded_clocks,
        arena: global_arena,
        num_events,
    };

    // --- Trigger Injection ---
    let mut trigger_map: HashMap<AbsoluteAddr, Vec<crate::ir::TriggerIdWithKind>> =
        HashMap::default();
    let module_vars = &program.module_variables;
    for (id, addr) in program.topological_clocks.iter().enumerate() {
        if let Some(module_name) = program.instance_module.get(&addr.instance_id) {
            if let Some(vars) = module_vars.get(module_name) {
                // Find variable info by var_id
                if let Some(info) = vars.values().find(|v| v.id == addr.var_id) {
                    let kind = info.kind;
                    trigger_map
                        .entry(*addr)
                        .or_default()
                        .push(crate::ir::TriggerIdWithKind { kind, id });
                }
            }
        }
    }

    let mut program = program;
    for units in program.eval_apply_ffs.values_mut() {
        for eu in units {
            for bb in eu.blocks.values_mut() {
                for inst in &mut bb.instructions {
                    match inst {
                        crate::ir::SIRInstruction::Store(addr, .., triggers) => {
                            let abs = addr.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        crate::ir::SIRInstruction::Commit(_, dst, .., triggers) => {
                            let abs = dst.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for units in program.eval_only_ffs.values_mut() {
        for eu in units {
            for bb in eu.blocks.values_mut() {
                for inst in &mut bb.instructions {
                    match inst {
                        crate::ir::SIRInstruction::Store(addr, .., triggers) => {
                            let abs = addr.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        crate::ir::SIRInstruction::Commit(_, dst, .., triggers) => {
                            let abs = dst.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for units in program.apply_ffs.values_mut() {
        for eu in units {
            for bb in eu.blocks.values_mut() {
                for inst in &mut bb.instructions {
                    match inst {
                        crate::ir::SIRInstruction::Store(addr, ..) => {
                            let abs = addr.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                if let crate::ir::SIRInstruction::Store(.., triggers) = inst {
                                    *triggers = ts.clone();
                                }
                            }
                        }
                        crate::ir::SIRInstruction::Commit(_, dst, ..) => {
                            let abs = dst.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                if let crate::ir::SIRInstruction::Commit(.., triggers) = inst {
                                    *triggers = ts.clone();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for eu in &mut program.eval_comb {
        for bb in eu.blocks.values_mut() {
            for inst in &mut bb.instructions {
                match inst {
                    crate::ir::SIRInstruction::Store(addr, .., triggers) => {
                        let abs = addr.absolute_addr();
                        let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                        if let Some(ts) = trigger_map.get(&canonical) {
                            *triggers = ts.clone();
                        }
                    }
                    crate::ir::SIRInstruction::Commit(_, dst, .., triggers) => {
                        let abs = dst.absolute_addr();
                        let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                        if let Some(ts) = trigger_map.get(&canonical) {
                            *triggers = ts.clone();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(program)
}
fn module_variables(registry: &ModuleRegistry) -> HashMap<StrId, HashMap<VarPath, VariableInfo>> {
    let mut res = HashMap::default();
    for (name, module) in &registry.modules {
        let mut variables = HashMap::default();
        for (id, varibale) in &module.variables {
            variables.insert(
                varibale.path.clone(),
                VariableInfo {
                    width: varibale.total_width().unwrap()
                        * varibale.r#type.total_array().unwrap_or(1),
                    id: *id,
                    is_4state: is_4state_type(&varibale.r#type.kind),
                    kind: type_kind_to_domain_kind(&varibale.r#type.kind),
                    var_kind: varibale.kind,
                    type_kind: type_kind_to_port_type_kind(&varibale.r#type.kind),
                },
            );
        }
        res.insert(*name, variables);
    }
    res
}

fn type_kind_to_port_type_kind(kind: &veryl_analyzer::ir::TypeKind) -> crate::ir::PortTypeKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ClockNegedge => {
            crate::ir::PortTypeKind::Clock
        }
        TypeKind::Reset
        | TypeKind::ResetAsyncHigh
        | TypeKind::ResetAsyncLow
        | TypeKind::ResetSyncHigh
        | TypeKind::ResetSyncLow => crate::ir::PortTypeKind::Reset,
        TypeKind::Logic => crate::ir::PortTypeKind::Logic,
        TypeKind::Bit => crate::ir::PortTypeKind::Bit,
        _ => crate::ir::PortTypeKind::Other,
    }
}

fn type_kind_to_domain_kind(kind: &veryl_analyzer::ir::TypeKind) -> DomainKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock | TypeKind::ClockPosedge => DomainKind::ClockPosedge,
        TypeKind::ClockNegedge => DomainKind::ClockNegedge,
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
    top: &StrId,
    modules: &HashMap<StrId, SimModule>,
) -> (
    HashMap<InstancePath, InstanceId>,
    HashMap<InstanceId, StrId>,
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
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
) -> HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>> {
    let mut current_boundaries = HashMap::default();

    // Initialize with local boundaries
    for id in expanded.values() {
        let module_name = &instance_modules[id];
        let sim_module = &modules[module_name];
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
            let module_name = &instance_modules[id];
            let sim_module = &modules[module_name];

            for (inst_name, glue_blocks) in &sim_module.glue_blocks {
                for (idx, glue_block) in glue_blocks.iter().enumerate() {
                    let mut child_path = path.0.clone();
                    child_path.push((*inst_name, idx));
                    let child_id = expanded[&InstancePath(child_path)];

                    // Propagate from Parent to Child (Input Ports)
                    for (parent_vars, child_addr) in &glue_block.input_ports {
                        if let GlueAddr::Child(child_var_id) = child_addr.target.id {
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
    target: &StrId,
    path: Vec<(StrId, usize)>,
    modules: &HashMap<StrId, SimModule>,
    expanded: &mut HashMap<InstancePath, InstanceId>,
    instance_modules: &mut HashMap<InstanceId, StrId>,
    instance_id: &mut usize,
) {
    let module = &modules[target];
    for (inst_name, gbs) in &module.glue_blocks {
        for (idx, gb) in gbs.iter().enumerate() {
            let mut path = path.clone();
            path.push((*inst_name, idx));
            let id = InstanceId(*instance_id);
            expanded.insert(InstancePath(path.clone()), id);
            instance_modules.insert(id, gb.module_name);
            *instance_id += 1;
            expand(
                &gb.module_name,
                path,
                modules,
                expanded,
                instance_modules,
                instance_id,
            );
        }
    }
}

pub fn parse(
    top: &StrId,
    ir: &veryl_analyzer::ir::Ir,
    optimize: bool,
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
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
) -> Result<Program, ParserError> {
    let (regsitry, sim_modules) = parse_ir(ir)?;
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.analyzer_ir
    {
        t.analyzer_ir = Some(ir.to_string());
    }
    let mut program = flatten(
        top,
        &regsitry,
        sim_modules,
        ignored_loops,
        true_loops,
        four_state,
        trace_opts,
        trace.as_deref_mut(),
    )?;

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.pre_optimized_sir
    {
        t.pre_optimized_sir = Some(program.clone());
    }

    if optimize {
        crate::optimizer::optimize(&mut program);
    }

    if let Some(t) = trace
        && trace_opts.post_optimized_sir
    {
        t.post_optimized_sir = Some(program.clone());
    }

    Ok(program)
}

fn relocate_executation_unit<A, B>(
    eu: &ExecutionUnit<A>,
    f: &impl Fn(&A) -> B,
) -> ExecutionUnit<B> {
    ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .iter()
            .map(|(id, block)| {
                (
                    *id,
                    crate::ir::BasicBlock {
                        id: block.id,
                        instructions: block
                            .instructions
                            .iter()
                            .map(|inst| inst.map_addr(f))
                            .collect(),
                        params: block.params.clone(),
                        terminator: block.terminator.clone(),
                    },
                )
            })
            .collect(),
        register_map: eu.register_map.clone(),
    }
}

fn unify_clock_domains(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();

    for (path, id) in expanded {
        let module_name = &instance_modules[id];
        let sim_module = &modules[module_name];

        // Internal aliases (e.g. `assign clk_internal = clk_port;`)
        for logic_path in &sim_module.comb_blocks {
            // Only unify direct aliases, not complex logic like gated clocks
            if logic_path.sources.len() == 1 {
                let expr_node = sim_module.arena.get(logic_path.expr);
                let is_alias = match expr_node {
                    crate::logic_tree::SLTNode::Input { .. }
                    | crate::logic_tree::SLTNode::Slice { .. } => true,
                    _ => false,
                };
                if is_alias {
                    let target_abs = AbsoluteAddr {
                        instance_id: *id,
                        var_id: logic_path.target.id,
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
                    if let GlueAddr::Child(child_var_id) = logic_path.target.id {
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
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    trace_opts: &crate::debug::TraceOptions,
    trace: &mut Option<&mut crate::debug::CompilationTrace>,
) -> (
    SLTNodeArena<AbsoluteAddr>,
    HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
    HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
    HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
    Vec<crate::logic_tree::LogicPath<AbsoluteAddr>>,
) {
    let mut global_arena = SLTNodeArena::<AbsoluteAddr>::new();
    let mut eval_apply_ffs: HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>,
    > = HashMap::default();
    let mut eval_only_ffs: HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>,
    > = HashMap::default();
    let mut apply_ffs: HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut comb_blocks = Vec::new();

    for (path, id) in expanded {
        let module_name = &instance_modules[id];
        let sim_module = &modules[module_name];

        let relocated_module = flatting::flatting(
            sim_module,
            path,
            expanded,
            global_boundaries,
            &mut global_arena,
            trace_opts,
            trace.as_deref_mut(),
        );
        comb_blocks.extend(relocated_module.comb_blocks);

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

            eval_apply_ffs
                .entry(canonical_addr)
                .or_default()
                .push(relocate_executation_unit(eu, &|addr| {
                    RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    }
                }));

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_apply_ffs
                    .entry(canonical_addr)
                    .or_default()
                    .push(relocate_executation_unit(eu, &|addr| {
                        RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        }
                    }));
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
            eval_only_ffs
                .entry(canonical_addr)
                .or_default()
                .push(relocate_executation_unit(eu, &|addr| {
                    RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    }
                }));

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_only_ffs
                    .entry(canonical_addr)
                    .or_default()
                    .push(relocate_executation_unit(eu, &|addr| {
                        RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        }
                    }));
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
            apply_ffs
                .entry(canonical_addr)
                .or_default()
                .push(relocate_executation_unit(eu, &|addr| {
                    RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    }
                }));

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                apply_ffs
                    .entry(canonical_addr)
                    .or_default()
                    .push(relocate_executation_unit(eu, &|addr| {
                        RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        }
                    }));
            }
        }
    }
    (
        global_arena,
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        comb_blocks,
    )
}

fn analyze_clock_dependencies(
    eval_apply_ffs: &mut HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, StrId>,
    modules: &HashMap<StrId, SimModule>,
) -> (Vec<AbsoluteAddr>, BTreeSet<AbsoluteAddr>) {
    // Build static clock dependency graph & Topo Sort
    let mut clock_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    let mut unique_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();

    // 1. Identify all variables written by FFs (direct sequential outputs)
    let mut ff_outputs: BTreeSet<AbsoluteAddr> = BTreeSet::new();

    for (domain_clock, eus) in &*eval_apply_ffs {
        unique_clocks.insert(*domain_clock);
        for eu in eus {
            for (_, bb) in &eu.blocks {
                for inst in &bb.instructions {
                    if let crate::ir::SIRInstruction::Store(target_addr, ..) = inst {
                        // Direct sequential dependency: the target is driven by this clock
                        let abs = target_addr.absolute_addr();
                        let canonical_target = clock_domains.get(&abs).copied().unwrap_or(abs);

                        ff_outputs.insert(abs);

                        if canonical_target != *domain_clock {
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
    let mut comb_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    for path in comb_blocks {
        let target_abs = path.target.id;
        let mut sources = crate::HashSet::default();
        crate::flatting::collect_inputs(path.expr, arena, &mut sources);
        for source in sources {
            comb_deps.entry(target_abs).or_default().insert(source.id);
        }
    }

    // 3. Propagate FF outputs through combinational graph to find all derived variables
    let mut derived_from_ff: BTreeSet<AbsoluteAddr> = ff_outputs.clone();
    let mut changed = true;
    while changed {
        changed = false;
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
        let module_name = &instance_modules[id];
        let sim_module = &modules[module_name];
        for (var_id, var) in &sim_module.variables {
            let kind = type_kind_to_domain_kind(&var.r#type.kind);
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
