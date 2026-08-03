use celox_design::ModuleId;
use veryl_analyzer::ir::{Component, Declaration, Module, VarKind};
use veryl_parser::resource_table::{self, StrId};

use crate::{
    BuildConfig, HashMap, HashSet, LoweringPhase, ParserError, SimModule,
    loop_provenance::LoopProvenance, module::ModuleParser,
};

/// A source-language-neutral module supplied by another frontend for use in a
/// Veryl hierarchy.
#[derive(Clone)]
pub struct ExternalModule {
    pub metadata: Module,
    pub sim_module: SimModule,
    pub port_order: Vec<veryl_analyzer::ir::VarId>,
    pub unresolved_instances: Vec<StrId>,
}

/// A module graph owned by another frontend. Module IDs are local to this
/// graph and are remapped when it is embedded into a Veryl design.
#[derive(Clone, Default)]
pub struct ExternalHierarchy {
    pub modules: HashMap<ModuleId, ExternalModule>,
    pub roots: HashMap<StrId, ModuleId>,
}

static EMPTY_EXTERNAL_HIERARCHY: std::sync::LazyLock<ExternalHierarchy> =
    std::sync::LazyLock::new(ExternalHierarchy::default);

/// Veryl modules discovered from the selected top before hierarchy expansion.
///
/// This artifact may retain analyzer references while frontend construction is
/// in progress. It is consumed before source-independent design and SIR
/// artifacts leave the frontend boundary.
pub struct SymbolicRtl<'a> {
    pub modules: HashMap<ModuleId, SimModule>,
    pub module_ir: HashMap<ModuleId, &'a Module>,
    pub module_names: HashMap<ModuleId, StrId>,
    pub root_id: ModuleId,
}

pub fn parse_ir<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    config: &BuildConfig,
    top: &StrId,
) -> Result<SymbolicRtl<'a>, ParserError> {
    parse_ir_with_loop_provenance(ir, &LoopProvenance::default(), config, top)
}

pub fn parse_ir_with_loop_provenance<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &LoopProvenance,
    config: &BuildConfig,
    top: &StrId,
) -> Result<SymbolicRtl<'a>, ParserError> {
    parse_ir_with_external_hierarchy(ir, loop_provenance, &EMPTY_EXTERNAL_HIERARCHY, config, top)
}

pub fn parse_ir_with_external_hierarchy<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &LoopProvenance,
    external: &'a ExternalHierarchy,
    config: &BuildConfig,
    top: &StrId,
) -> Result<SymbolicRtl<'a>, ParserError> {
    let mut name_to_ir: HashMap<StrId, &'a Module> = HashMap::default();
    let mut generic_names: HashSet<StrId> = HashSet::default();
    for component in &ir.components {
        match component {
            Component::Module(module) => {
                let is_generic = module
                    .variables
                    .values()
                    .any(|variable| variable.r#type.is_unknown());
                if is_generic {
                    generic_names.insert(module.name);
                }
                name_to_ir.insert(module.name, module);
            }
            Component::Interface(_) => {
                unreachable!("Interface component must be eliminated before simulator parse_ir")
            }
            Component::SystemVerilog(sv) => {
                if !external.roots.contains_key(&sv.name) {
                    return Err(ParserError::unsupported(
                        64,
                        LoweringPhase::SimulatorParser,
                        "systemverilog component",
                        format!("module \"{}\" was not supplied", sv.name),
                        None,
                    ));
                }
            }
        }
    }

    let mut modules = HashMap::default();
    let mut module_ir = HashMap::default();
    let mut module_names = HashMap::default();
    let mut name_to_id = HashMap::default();
    let mut next_id = 0usize;

    let root_id = ModuleId(next_id);
    next_id += 1;
    let root_ir = name_to_ir
        .get(top)
        .ok_or_else(|| ParserError::TopNotFound {
            name: resource_table::get_str_value(*top).unwrap_or_default(),
        })?;
    if generic_names.contains(top) {
        return Err(ParserError::GenericTop {
            name: resource_table::get_str_value(*top).unwrap_or_default(),
        });
    }
    name_to_id.insert(*top, root_id);
    module_names.insert(root_id, *top);
    module_ir.insert(root_id, *root_ir);

    let mut external_ids = HashMap::default();
    let mut external_module_ids = external.modules.keys().copied().collect::<Vec<_>>();
    external_module_ids.sort_by_key(|id| id.0);
    for local_id in external_module_ids {
        let global_id = ModuleId(next_id);
        next_id += 1;
        external_ids.insert(local_id, global_id);
    }

    let mut external_modules_by_global = HashMap::default();
    for (&local_id, external_module) in &external.modules {
        let global_id = external_ids[&local_id];
        let mut external_module = external_module.clone();
        for blocks in external_module.sim_module.glue_blocks.values_mut() {
            for block in blocks {
                block.module_id = *external_ids.get(&block.module_id).ok_or_else(|| {
                    ParserError::illegal_context(
                        "external module hierarchy",
                        format!(
                            "module {} references unknown child {}",
                            local_id, block.module_id
                        ),
                        None,
                    )
                })?;
            }
        }
        module_names.insert(global_id, external_module.sim_module.name);
        module_ir.insert(global_id, &external.modules[&local_id].metadata);
        modules.insert(global_id, external_module.sim_module.clone());
        external_modules_by_global.insert(global_id, external_module);
    }

    let mut worklist = vec![(root_id, *root_ir)];
    let mut inst_sequences = HashMap::default();
    let mut validated_external = HashSet::default();
    let mut index = 0;
    while index < worklist.len() {
        let (module_id, ir_module) = worklist[index];
        index += 1;

        let mut inst_ids = Vec::new();
        for declaration in &ir_module.declarations {
            if let Declaration::Inst(inst_decl) = declaration {
                match &*inst_decl.component {
                    Component::SystemVerilog(sv) => {
                        let local_id = external.roots.get(&sv.name).ok_or_else(|| {
                            ParserError::unsupported(
                                64,
                                LoweringPhase::SimulatorParser,
                                "systemverilog module instantiation",
                                format!("module \"{}\" was not supplied", sv.name),
                                None,
                            )
                        })?;
                        validate_external_module_graph(
                            *local_id,
                            external,
                            &mut HashSet::default(),
                            &mut validated_external,
                        )?;
                        inst_ids.push(external_ids[local_id]);
                    }
                    Component::Module(child_module) => {
                        let child_name = child_module.name;
                        let has_params = child_module
                            .variables
                            .values()
                            .any(|variable| variable.kind == VarKind::Param);
                        if generic_names.contains(&child_name) || has_params {
                            let child_id = ModuleId(next_id);
                            next_id += 1;
                            module_names.insert(child_id, child_name);
                            module_ir.insert(child_id, child_module);
                            worklist.push((child_id, child_module));
                            inst_ids.push(child_id);
                        } else {
                            let child_id = if let Some(&existing) = name_to_id.get(&child_name) {
                                existing
                            } else {
                                let id = ModuleId(next_id);
                                next_id += 1;
                                name_to_id.insert(child_name, id);
                                module_names.insert(id, child_name);
                                module_ir.insert(id, child_module);
                                worklist.push((id, child_module));
                                id
                            };
                            inst_ids.push(child_id);
                        }
                    }
                    Component::Interface(_) => {
                        unreachable!("Interface component in inst declaration")
                    }
                }
            }
        }
        inst_sequences.insert(module_id, inst_ids);
    }

    // veryl-parser's resource table is process-global, so module parsing must
    // remain serial even though modules are otherwise independent.
    for (module_id, ir_module) in &module_ir {
        if external_modules_by_global.contains_key(module_id) {
            continue;
        }
        let inst_ids = inst_sequences
            .get(module_id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let sim_module = ModuleParser::parse_with_loop_provenance_and_external_modules(
            ir_module,
            loop_provenance,
            config,
            inst_ids,
            &external_modules_by_global,
        )?;
        modules.insert(*module_id, sim_module);
    }

    Ok(SymbolicRtl {
        modules,
        module_ir,
        module_names,
        root_id,
    })
}

fn validate_external_module_graph(
    module_id: ModuleId,
    external: &ExternalHierarchy,
    active: &mut HashSet<ModuleId>,
    complete: &mut HashSet<ModuleId>,
) -> Result<(), ParserError> {
    if complete.contains(&module_id) {
        return Ok(());
    }
    if !active.insert(module_id) {
        return Err(ParserError::unsupported(
            64,
            LoweringPhase::SimulatorParser,
            "recursive systemverilog module instantiation",
            format!("cycle includes external module {module_id}"),
            None,
        ));
    }
    let module = external.modules.get(&module_id).ok_or_else(|| {
        ParserError::illegal_context(
            "external module hierarchy",
            format!("unknown external module {module_id}"),
            None,
        )
    })?;
    if let Some(name) = module.unresolved_instances.first() {
        return Err(ParserError::unsupported(
            64,
            LoweringPhase::SimulatorParser,
            "systemverilog module instantiation",
            format!("module \"{name}\" was not supplied"),
            None,
        ));
    }
    for blocks in module.sim_module.glue_blocks.values() {
        for block in blocks {
            validate_external_module_graph(block.module_id, external, active, complete)?;
        }
    }
    active.remove(&module_id);
    complete.insert(module_id);
    Ok(())
}
