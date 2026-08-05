use celox_design::ModuleId;
use std::sync::atomic::{AtomicUsize, Ordering};
use veryl_analyzer::ir::{Component, Declaration, Module, VarKind};
use veryl_parser::resource_table::{self, StrId};

use crate::{
    BuildConfig, HashMap, HashSet, LoweringPhase, ParserError, SimModule,
    loop_provenance::LoopProvenance, module::ModuleParser,
};

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
                return Err(ParserError::unsupported(
                    64,
                    LoweringPhase::SimulatorParser,
                    "systemverilog component",
                    format!("name: \"{}\"", sv.name),
                    None,
                ));
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

    let mut worklist = vec![(root_id, *root_ir)];
    let mut inst_sequences = HashMap::default();
    let mut index = 0;
    while index < worklist.len() {
        let (module_id, ir_module) = worklist[index];
        index += 1;

        let mut inst_ids = Vec::new();
        for declaration in &ir_module.declarations {
            if let Declaration::Inst(inst_decl) = declaration {
                match &*inst_decl.component {
                    Component::SystemVerilog(_) => {
                        let child_id = ModuleId(next_id);
                        next_id += 1;
                        inst_ids.push(child_id);
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

    // Analyzer/parser resources are thread-local. Copy the read-only parser
    // tables into each worker before lowering otherwise independent modules.
    // Hierarchy discovery above remains serial, so ModuleId and instance-ID
    // assignment are identical to the single-threaded construction.
    let resource_snapshot = resource_table::export_tables();
    let tasks = module_ir
        .iter()
        .map(|(&module_id, &ir_module)| {
            let inst_ids = inst_sequences
                .get(&module_id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            (module_id, ir_module, inst_ids)
        })
        .collect::<Vec<_>>();
    let worker_count = tasks
        .len()
        .min(std::thread::available_parallelism().map_or(1, usize::from));
    let next_task = AtomicUsize::new(0);
    let parsed_modules = std::thread::scope(|scope| {
        let handles = (0..worker_count)
            .map(|_| {
                scope.spawn(|| {
                    resource_table::import_tables(&resource_snapshot);
                    let mut parsed = Vec::new();
                    loop {
                        let task = next_task.fetch_add(1, Ordering::Relaxed);
                        let Some(&(module_id, ir_module, inst_ids)) = tasks.get(task) else {
                            break;
                        };
                        let module = ModuleParser::parse_with_loop_provenance(
                            ir_module,
                            loop_provenance,
                            config,
                            inst_ids,
                        )?;
                        parsed.push((module_id, module));
                    }
                    Ok::<_, ParserError>(parsed)
                })
            })
            .collect::<Vec<_>>();
        let mut parsed_modules = Vec::with_capacity(tasks.len());
        for handle in handles {
            let parsed = handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))?;
            parsed_modules.extend(parsed);
        }
        Ok::<_, ParserError>(parsed_modules)
    })?;
    modules.extend(parsed_modules);

    Ok(SymbolicRtl {
        modules,
        module_ir,
        module_names,
        root_id,
    })
}
