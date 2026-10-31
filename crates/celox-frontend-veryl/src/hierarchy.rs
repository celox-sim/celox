use celox_design::ModuleId;
use std::sync::atomic::{AtomicUsize, Ordering};
use veryl_analyzer::ir::{Component, Declaration, Module, TypeKind, VarKind};
use veryl_analyzer::{symbol::SymbolKind, symbol_table};
use veryl_parser::{
    resource_table::{self, StrId},
    text_table,
};

use super::{
    artifact::{VerylSymbolicRtl, project_id_map, project_module_with_ids},
    loop_provenance::LoopProvenance,
    module::ModuleParser,
};
use crate::{
    BuildConfig, HashMap, HashSet, LoweringPhase, ParserError,
    symbolic::artifact::{ExternalHierarchy, SymbolicRtl},
};

static EMPTY_EXTERNAL_HIERARCHY: std::sync::LazyLock<ExternalHierarchy> =
    std::sync::LazyLock::new(ExternalHierarchy::default);

pub fn parse_ir<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    config: &BuildConfig,
    top: &StrId,
) -> Result<VerylSymbolicRtl<'a>, ParserError> {
    parse_ir_with_loop_provenance(ir, &LoopProvenance::default(), config, top)
}

pub fn parse_ir_with_loop_provenance<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &LoopProvenance,
    config: &BuildConfig,
    top: &StrId,
) -> Result<VerylSymbolicRtl<'a>, ParserError> {
    parse_ir_with_external_hierarchy(ir, loop_provenance, &EMPTY_EXTERNAL_HIERARCHY, config, top)
}

pub fn parse_ir_with_external_hierarchy<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &LoopProvenance,
    external: &'a ExternalHierarchy,
    config: &BuildConfig,
    top: &StrId,
) -> Result<VerylSymbolicRtl<'a>, ParserError> {
    parse_external_hierarchy(ir, loop_provenance, external, config, top, None)
}

/// Link the explicitly identified Veryl exports without matching SV namesakes.
/// `native_modules` maps each external module name that stands for a Veryl
/// body to the index of that body in `ir.components`, so bodies that share a
/// source name stay distinct.
pub fn parse_ir_with_external_hierarchy_and_native_modules<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &LoopProvenance,
    external: &'a ExternalHierarchy,
    config: &BuildConfig,
    top: &StrId,
    native_modules: &HashMap<String, usize>,
) -> Result<VerylSymbolicRtl<'a>, ParserError> {
    parse_external_hierarchy(
        ir,
        loop_provenance,
        external,
        config,
        top,
        Some(native_modules),
    )
}

fn parse_external_hierarchy<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &LoopProvenance,
    external: &'a ExternalHierarchy,
    config: &BuildConfig,
    top: &StrId,
    native_modules: Option<&HashMap<String, usize>>,
) -> Result<VerylSymbolicRtl<'a>, ParserError> {
    // Token-to-scope resolution uses process-global analyzer state and is not
    // safe while independent compilations run on other test threads. Snapshot
    // the thread-local symbols once for hierarchy-bearing designs instead.
    // Designs without instances avoid this work entirely; this is the common
    // path for small compilation benchmarks.
    let has_instances = ir.components.iter().any(|component| {
        matches!(component, Component::Module(module) if module.declarations.iter().any(|declaration| matches!(declaration, Declaration::Inst(_))))
    });
    let symbols = has_instances.then(symbol_table::get_all);

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
            Component::SystemVerilog(_) => {}
        }
    }

    let mut modules = HashMap::default();
    let mut module_ir = HashMap::default();
    let mut module_names = HashMap::default();
    let mut signature_to_id = HashMap::default();
    let mut next_id = 0usize;

    let root_id = ModuleId(next_id);
    next_id += 1;
    let top_name = resource_table::get_str_value(*top).unwrap_or_default();
    let root_ir = name_to_ir.get(top).copied();
    let external_root = external.roots.get(&top_name).copied().filter(|_| {
        root_ir.is_none() || native_modules.is_none_or(|names| names.contains_key(&top_name))
    });
    if root_ir.is_none() && external_root.is_none() {
        return Err(ParserError::TopNotFound { name: top_name });
    }
    if generic_names.contains(top) {
        return Err(ParserError::GenericTop {
            name: resource_table::get_str_value(*top).unwrap_or_default(),
        });
    }
    if let Some(root_ir) = root_ir {
        signature_to_id.insert(root_ir.signature.clone(), root_id);
        module_ir.insert(root_id, root_ir);
    }
    module_names.insert(root_id, top_name);

    let mut external_ids = HashMap::default();
    let mut external_module_ids = external.modules.keys().copied().collect::<Vec<_>>();
    external_module_ids.sort_by_key(|id| id.0);
    for local_id in external_module_ids {
        let global_id = if Some(local_id) == external_root {
            root_id
        } else {
            let id = ModuleId(next_id);
            next_id += 1;
            id
        };
        external_ids.insert(local_id, global_id);
    }

    let mut worklist = root_ir
        .into_iter()
        .map(|module| (root_id, module))
        .collect::<Vec<_>>();
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
        for binding in external_module.sim_module.package_bindings.values_mut() {
            binding.package = external_ids[&binding.package];
        }
        module_names.insert(global_id, external_module.sim_module.name.clone());
        let native_module = match native_modules {
            Some(names) => match names.get(&external_module.sim_module.name) {
                Some(&index) => match ir.components.get(index) {
                    Some(Component::Module(module)) => Some(module),
                    _ => {
                        return Err(ParserError::illegal_context(
                            "external module hierarchy",
                            format!(
                                "native module `{}` maps to component {index}, which is not a module",
                                external_module.sim_module.name
                            ),
                            None,
                        ));
                    }
                },
                None => None,
            },
            None => name_to_ir
                .get(&resource_table::insert_str(
                    &external_module.sim_module.name,
                ))
                .copied(),
        };
        if let Some(native_module) = native_module
            && !module_ir.contains_key(&global_id)
        {
            module_names.insert(
                global_id,
                resource_table::get_str_value(native_module.name).unwrap_or_default(),
            );
            module_ir.insert(global_id, native_module);
            signature_to_id.insert(native_module.signature.clone(), global_id);
            worklist.push((global_id, native_module));
        }
        modules.insert(global_id, external_module.sim_module.clone());
        external_modules_by_global.insert(global_id, external_module);
    }

    let mut inst_sequences = HashMap::default();
    let mut validated_external = HashSet::default();
    if let Some(root) = external_root {
        validate_external_module_graph(
            root,
            external,
            &mut HashSet::default(),
            &mut validated_external,
        )?;
    }
    let mut index = 0;
    while index < worklist.len() {
        let (module_id, ir_module) = worklist[index];
        index += 1;
        reject_generic_interface_ports(ir_module)?;

        let mut inst_ids = Vec::new();
        for declaration in &ir_module.declarations {
            if let Declaration::Inst(inst_decl) = declaration {
                match &*inst_decl.component {
                    Component::SystemVerilog(sv) => {
                        let external_name =
                            resource_table::get_str_value(sv.name).unwrap_or_default();
                        let local_id = external.roots.get(&external_name).ok_or_else(|| {
                            ParserError::MissingExternalModule {
                                name: external_name.clone(),
                                source_location: None,
                            }
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
                            module_names.insert(
                                child_id,
                                resource_table::get_str_value(child_name).unwrap_or_default(),
                            );
                            module_ir.insert(child_id, child_module);
                            worklist.push((child_id, child_module));
                            inst_ids.push(child_id);
                        } else {
                            // A value generic need not leave unknown variable types or
                            // Param variables in the specialized body. Its module name
                            // alone therefore cannot identify a reusable specialization.
                            let child_id = if let Some(&existing) =
                                signature_to_id.get(&child_module.signature)
                            {
                                existing
                            } else {
                                let id = ModuleId(next_id);
                                next_id += 1;
                                signature_to_id.insert(child_module.signature.clone(), id);
                                module_names.insert(
                                    id,
                                    resource_table::get_str_value(child_name).unwrap_or_default(),
                                );
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
    let text_snapshot = text_table::export_tables();
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
    // Each worker owns lowering arenas and loop-equivalence proof scratch.
    // Bound their overlap instead of multiplying it by every host CPU.
    #[cfg(not(target_arch = "wasm32"))]
    let worker_count = tasks
        .len()
        .min(std::thread::available_parallelism().map_or(1, usize::from))
        .min(4);
    // Browser WASI cannot synchronously join the scoped threads used below.
    // The whole frontend already runs in a dedicated browser worker, so keep
    // module lowering serial inside the wasm module.
    #[cfg(target_arch = "wasm32")]
    let worker_count = 1;
    let next_task = AtomicUsize::new(0);
    let parsed_modules = if worker_count <= 1 {
        tasks
            .iter()
            .map(|&(module_id, ir_module, inst_ids)| {
                ModuleParser::parse_with_loop_provenance_and_external_modules(
                    ir_module,
                    loop_provenance,
                    config,
                    inst_ids,
                    &external_modules_by_global,
                )
                .map(|module| (module_id, module))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        std::thread::scope(|scope| {
            let handles = (0..worker_count)
                .map(|_| {
                    scope.spawn(|| {
                        resource_table::import_tables(&resource_snapshot);
                        text_table::import_tables(&text_snapshot);
                        let mut parsed = Vec::new();
                        loop {
                            let task = next_task.fetch_add(1, Ordering::Relaxed);
                            let Some(&(module_id, ir_module, inst_ids)) = tasks.get(task) else {
                                break;
                            };
                            let module =
                                ModuleParser::parse_with_loop_provenance_and_external_modules(
                                    ir_module,
                                    loop_provenance,
                                    config,
                                    inst_ids,
                                    &external_modules_by_global,
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
        })?
    };
    let mut source_id_maps = module_ir
        .iter()
        .map(|(&module_id, module)| Ok((module_id, project_id_map(module)?)))
        .collect::<Result<HashMap<_, _>, ParserError>>()?;
    for (&module_id, module) in &external_modules_by_global {
        source_id_maps.entry(module_id).or_insert_with(|| {
            module
                .sim_module
                .variables
                .keys()
                .map(|id| (veryl_analyzer::ir::VarId::from_raw(id.0), *id))
                .collect()
        });
    }
    let mut inferred_latch_diagnostics: Vec<crate::FrontendDiagnostic> = Vec::new();
    for (module_id, mut sim_module) in parsed_modules {
        let ir_module = module_ir[&module_id];
        if let Some(symbols) = &symbols
            && let Some(module_symbol) = symbols.iter().find(|symbol| {
                matches!(symbol.kind, SymbolKind::Module(_))
                    && symbol.id == ir_module.signature.symbol
                    && symbol.token.source == ir_module.token.beg.source
            })
        {
            let module_namespace = module_symbol.inner_namespace();
            sim_module.indexed_instance_names = symbols
                .iter()
                .filter_map(|symbol| match &symbol.kind {
                    SymbolKind::Instance(property)
                        if !property.array.is_empty()
                            && symbol.namespace.included(&module_namespace) =>
                    {
                        Some(symbol.token.text)
                    }
                    _ => None,
                })
                .collect();
        }
        let id_map = source_id_maps[&module_id].clone();
        let (sim_module, id_map, diagnostics) =
            project_module_with_ids(&sim_module, ir_module, config, id_map, &source_id_maps)?;
        for diagnostic in diagnostics {
            if !inferred_latch_diagnostics
                .iter()
                .any(|existing| existing.same_site(&diagnostic))
            {
                inferred_latch_diagnostics.push(diagnostic);
            }
        }
        modules.insert(module_id, sim_module);
        source_id_maps.insert(module_id, id_map);
    }

    fn check_cycles(
        id: ModuleId,
        modules: &HashMap<ModuleId, crate::symbolic::artifact::SimModule>,
        active: &mut HashSet<ModuleId>,
        done: &mut HashSet<ModuleId>,
    ) -> Result<(), ParserError> {
        if done.contains(&id) {
            return Ok(());
        }
        if !active.insert(id) {
            return Err(ParserError::illegal_context(
                "recursive mixed-language module instantiation",
                modules[&id].name.clone(),
                None,
            ));
        }
        for block in modules[&id].glue_blocks.values().flatten() {
            check_cycles(block.module_id, modules, active, done)?;
        }
        active.remove(&id);
        done.insert(id);
        Ok(())
    }
    check_cycles(
        root_id,
        &modules,
        &mut HashSet::default(),
        &mut HashSet::default(),
    )?;

    Ok(VerylSymbolicRtl {
        symbolic: SymbolicRtl {
            modules,
            module_names,
            root_id,
            packages: external
                .packages
                .iter()
                .map(|(name, module_id)| (name.clone(), external_ids[module_id]))
                .collect(),
            instance_values: Vec::new(),
        },
        module_ir,
        source_id_maps,
        inferred_latch_diagnostics,
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
        return Err(ParserError::illegal_context(
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
        return Err(ParserError::MissingExternalModule {
            name: name.to_string(),
            source_location: None,
        });
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

/// A generic interface port (`interface` or `interface::modport`) stays an
/// `AbstractInterface` in the analyzer IR: the module that declares it is not
/// specialized per instance, so the members behind the port are not variables
/// of that module and a connection forwarding it is an unknown expression.
/// Reject the port instead of failing on that expression during lowering.
fn reject_generic_interface_ports(module: &Module) -> Result<(), ParserError> {
    let mut ports = module
        .port_types
        .iter()
        .filter(|(_, (r#type, _))| matches!(r#type.kind, TypeKind::AbstractInterface(_)))
        .map(|(path, _)| path.to_string())
        .collect::<Vec<_>>();
    if ports.is_empty() {
        return Ok(());
    }
    ports.sort();
    Err(ParserError::unsupported(
        1088,
        LoweringPhase::SimulatorParser,
        "generic interface port",
        format!(
            "module {} port {}",
            resource_table::get_str_value(module.name).unwrap_or_default(),
            ports.join(", ")
        ),
        Some(&module.token),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::{Analyzer, Context, attribute_table};
    use veryl_parser::Parser;

    #[test]
    fn rejects_native_module_indices_outside_the_ir() {
        symbol_table::clear();
        attribute_table::clear();
        let code = "module Top {} module Child {}";
        let metadata = veryl_metadata::Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"top.veryl").unwrap();
        let analyzer = Analyzer::new(&metadata);
        assert!(analyzer.analyze_pass1("prj", &parser.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        let mut ir = veryl_analyzer::ir::Ir::default();
        let mut context = Context::default();
        assert!(
            analyzer
                .analyze_pass2(&parser.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        let config = BuildConfig::default();
        let external = crate::external_signatures(&ir, &config).unwrap();
        let native = [("Child".to_string(), ir.components.len())]
            .into_iter()
            .collect::<HashMap<_, _>>();
        let error = parse_ir_with_external_hierarchy_and_native_modules(
            &ir,
            &LoopProvenance::default(),
            &external,
            &config,
            &resource_table::insert_str("Top"),
            &native,
        )
        .err()
        .expect("an out-of-range index must be rejected");
        assert!(error.to_string().contains("not a module"), "{error}");
    }
}
