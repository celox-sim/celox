use std::fmt::Debug;
use std::{collections::BTreeSet, hash::Hash};

use veryl_analyzer::ir::VarId;

use crate::HashMap;
use crate::ir::{
    AbsoluteAddr, BitAccess, GlueAddr, GlueBlock, InstanceId, InstancePath, RelocationModule,
    SimModule, VarAtomBase,
};
use crate::logic_tree::{LogicPath, NodeId, SLTNode, SLTNodeArena, get_width};

pub fn flatting(
    module: &SimModule,
    path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    global_boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
) -> RelocationModule {
    let instance_id = instance_ids[path];
    let cv = &|id: &VarId| AbsoluteAddr {
        instance_id,
        var_id: *id,
    };

    let mut comb_cache = HashMap::default();
    let mut comb_blocks: Vec<_> = module
        .comb_blocks
        .iter()
        .map(|e| convert_logic_path(e, &module.arena, arena, &mut comb_cache, &cv))
        .collect();
    for (child_instance_name, gbs) in &module.glue_blocks {
        for (idx, gb) in gbs.iter().enumerate() {
            let mut glue_cache = HashMap::default();
            let mut child_path = path.0.clone();
            child_path.push((*child_instance_name, idx));
            let child_id = instance_ids[&InstancePath(child_path)];
            comb_blocks.extend(convert_glue_block(
                gb,
                instance_id,
                child_id,
                &gb.arena,
                arena,
                &mut glue_cache,
            ));
        }
    }
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.pre_atomized_comb_blocks
    {
        if let Some((ref mut blocks, _)) = t.pre_atomized_comb_blocks {
            blocks.extend(comb_blocks.clone());
        } else {
            t.pre_atomized_comb_blocks = Some((comb_blocks.clone(), arena.clone()));
        }
    }

    // Atomize logic paths
    let atomized_comb_blocks = atomize_logic_paths(&comb_blocks, global_boundaries, arena);

    if let Some(t) = trace
        && trace_opts.atomized_comb_blocks
    {
        if let Some((ref mut blocks, _)) = t.atomized_comb_blocks {
            blocks.extend(atomized_comb_blocks.clone());
        } else {
            t.atomized_comb_blocks = Some((atomized_comb_blocks.clone(), arena.clone()));
        }
    }

    RelocationModule {
        #[cfg(test)]
        variables: module.variables.clone(),
        eval_apply_ff_blocks: HashMap::default(),
        eval_only_ff_blocks: HashMap::default(),
        apply_ff_blocks: HashMap::default(),
        comb_blocks: atomized_comb_blocks,
    }
}

/// Atomizes the given logic paths based on the provided boundary map.
fn atomize_logic_paths(
    paths: &Vec<LogicPath<AbsoluteAddr>>,
    boundaries: &HashMap<AbsoluteAddr, BTreeSet<usize>>,
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Vec<LogicPath<AbsoluteAddr>> {
    let mut atomized_paths = Vec::new();

    for path in paths {
        if let Some(bounds) = boundaries.get(&path.target.id) {
            // This variable has defined boundaries, so we need to split it.
            let atoms = path.target.access.calculate_atoms(bounds);

            // Extract the set of variable IDs that were originally declared as sources.
            // This acts as a "source mask" to filter out unintended dependencies.
            let original_source_ids: crate::HashSet<_> =
                path.sources.iter().map(|s| s.id).collect();

            for atom_access in atoms {
                // Calculate the relative bit range of the atom within the original expression's output.
                let relative_atom_access = crate::ir::BitAccess::new(
                    atom_access.lsb - path.target.access.lsb,
                    atom_access.msb - path.target.access.lsb,
                );

                // Wrap the original expression in a Slice node to extract the required bits.
                let new_expr = arena.alloc(SLTNode::Slice {
                    expr: path.expr,
                    access: relative_atom_access,
                });

                // Extract all potential inputs from the new expression tree.
                let mut expr_inputs = crate::HashSet::default();
                collect_inputs(new_expr, arena, &mut expr_inputs);
                // Engineering Fix: Intersect expr_inputs with the original source IDs.
                // This ensures that we only include dependencies that were already present
                // in the original path, effectively stripping away accidental self-loops.
                let filtered_sources: crate::HashSet<_> = expr_inputs
                    .into_iter()
                    .filter(|input_atom| original_source_ids.contains(&input_atom.id))
                    .collect();

                let target = VarAtomBase::new(path.target.id, atom_access.lsb, atom_access.msb);

                let new_path = LogicPath {
                    target,
                    sources: filtered_sources,
                    expr: new_expr,
                };
                atomized_paths.push(new_path);
            }
        } else {
            // No boundaries defined for this target, so just add it as is.
            atomized_paths.push(path.clone());
        }
    }
    atomized_paths
}

pub fn collect_inputs<A: Hash + Eq + Clone + Debug>(
    expr: NodeId,
    arena: &SLTNodeArena<A>,
    set: &mut crate::HashSet<VarAtomBase<A>>,
) {
    collect_inputs_with_window(expr, None, arena, set);
}

fn collect_inputs_with_window<A: Hash + Eq + Clone + Debug>(
    expr: NodeId,
    window: Option<BitAccess>,
    arena: &SLTNodeArena<A>,
    set: &mut crate::HashSet<VarAtomBase<A>>,
) {
    match arena.get(expr) {
        SLTNode::Input {
            variable,
            access,
            index,
        } => {
            // Register the variable and its bit range as an input.
            if !index.is_empty() {
                // --- Dynamic Indexing Case ---
                // For scheduling safety, we MUST ignore the `window` here.
                // Dynamic access can point to different bits within the range,
                // so we need to cover the entire reachable bounding box.

                let element_width = get_width(expr, arena);
                let full_width = access.msb - access.lsb + 1;

                let mut max_reachable_elements = 1usize;
                for idx in index {
                    let idx_width = get_width(idx.node, arena);
                    let reachable = 1usize.checked_shl(idx_width as u32).unwrap_or(usize::MAX);
                    max_reachable_elements = max_reachable_elements.saturating_mul(reachable);
                }

                // Clamp by the actual number of elements in the variable.
                let actual_elements = full_width / element_width;
                let effective_elements = std::cmp::min(max_reachable_elements, actual_elements);

                // Calculate the bounding box:
                // LSB: Always the start of the first element (access.lsb).
                // MSB: The end of the last reachable element.
                let reachable_lsb = access.lsb;
                let reachable_msb = access.lsb + (effective_elements * element_width) - 1;

                set.insert(VarAtomBase::new(
                    variable.clone(),
                    reachable_lsb,
                    std::cmp::min(reachable_msb, access.msb),
                ));
            } else {
                // If the index is empty, it means the variable is statically indexed.
                // In this case, we can apply the window to minimize the dependencies.
                let full_width = access.msb - access.lsb + 1;
                let win = window.unwrap_or(BitAccess::new(0, full_width - 1));

                set.insert(VarAtomBase::new(
                    variable.clone(),
                    access.lsb + win.lsb,
                    access.lsb + win.msb,
                ));
            }

            // Also collect inputs from the index expressions (dynamic indexing).
            for idx in index {
                collect_inputs_with_window(idx.node, None, arena, set);
            }
        }
        SLTNode::Slice { expr, access } => {
            let composed = if let Some(win) = window {
                BitAccess::new(access.lsb + win.lsb, access.lsb + win.msb)
            } else {
                *access
            };
            collect_inputs_with_window(*expr, Some(composed), arena, set)
        }
        SLTNode::Concat(parts) => {
            if let Some(win) = window {
                // Concat bit layout: LSB is at the end of `parts`.
                // Walk from LSB side to map the requested window to each part.
                let mut part_lsb = 0usize;
                for (part, width) in parts.iter().rev() {
                    let part_msb = part_lsb + width - 1;
                    if win.overlaps(&BitAccess::new(part_lsb, part_msb)) {
                        let ov_lsb = std::cmp::max(win.lsb, part_lsb);
                        let ov_msb = std::cmp::min(win.msb, part_msb);
                        let local = BitAccess::new(ov_lsb - part_lsb, ov_msb - part_lsb);
                        collect_inputs_with_window(*part, Some(local), arena, set);
                    }
                    part_lsb += width;
                }
            } else {
                for (part, _) in parts {
                    collect_inputs_with_window(*part, None, arena, set);
                }
            }
        }
        SLTNode::Binary(lhs, _, rhs) => {
            collect_inputs_with_window(*lhs, None, arena, set);
            collect_inputs_with_window(*rhs, None, arena, set);
        }
        SLTNode::Unary(_, inner) => collect_inputs_with_window(*inner, None, arena, set),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_inputs_with_window(*cond, None, arena, set);
            collect_inputs_with_window(*then_expr, None, arena, set);
            collect_inputs_with_window(*else_expr, None, arena, set);
        }
        SLTNode::Constant(_, _, _) => {}
    }
}
fn convert_logic_path<A: Hash + Eq + Clone, B: Hash + Eq + Clone>(
    lp: &LogicPath<A>,
    arena: &SLTNodeArena<A>,
    target_arena: &mut SLTNodeArena<B>,
    cache: &mut HashMap<NodeId, NodeId>,
    f: &impl Fn(&A) -> B,
) -> LogicPath<B> {
    lp.map_addr(arena, target_arena, cache, f)
}
fn convert_glue_block(
    gb: &GlueBlock,
    parent_id: InstanceId,
    child_id: InstanceId,
    arena: &SLTNodeArena<GlueAddr>,
    target_arena: &mut SLTNodeArena<AbsoluteAddr>,
    cache: &mut HashMap<NodeId, NodeId>,
) -> Vec<LogicPath<AbsoluteAddr>> {
    let GlueBlock {
        module_name: _,
        input_ports,
        output_ports,
        arena: _,
    } = gb;
    let cv = &|addr: &GlueAddr| match addr {
        GlueAddr::Parent(v) => AbsoluteAddr {
            instance_id: parent_id,
            var_id: *v,
        },
        GlueAddr::Child(v) => AbsoluteAddr {
            instance_id: child_id,
            var_id: *v,
        },
    };
    let mut res = Vec::new();

    for (_ports, abb) in input_ports {
        res.push(convert_logic_path(abb, arena, target_arena, cache, cv));
    }
    for (_ports, abb) in output_ports {
        res.push(convert_logic_path(abb, arena, target_arena, cache, cv));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{module::ModuleParser, registry::ModuleRegistry};
    use veryl_analyzer::{
        self, Analyzer, Context, attribute_table,
        ir::{Component, Ir, VarPath},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::{Parser, resource_table::StrId};

    fn setup(code: &str) -> (HashMap<StrId, SimModule>, Ir) {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"").unwrap();

        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = veryl_analyzer::ir::Ir::default();

        analyzer.analyze_pass1("prj", &parser.veryl);
        Analyzer::analyze_post_pass1();
        analyzer.analyze_pass2("prj", &parser.veryl, &mut context, Some(&mut ir));
        Analyzer::analyze_post_pass2();

        let mut modules = HashMap::default();
        let mut ir_modules = HashMap::default();
        for component in &ir.components {
            if let Component::Module(m) = component {
                modules.insert(m.name, m.clone());
                ir_modules.insert(m.name, m);
            }
        }

        let mut sim_modules = HashMap::default();
        let registry = ModuleRegistry {
            modules: ir_modules,
        };
        for module in modules.values() {
            let sim_module = ModuleParser::parse(module, &registry).expect("module parse failed");
            sim_modules.insert(module.name, sim_module);
        }

        (sim_modules, ir)
    }

    #[test]
    fn test_flatting_simple_hierarchy() {
        let code = r#"
            module child(
                 i: input logic<1>,
                 o: output logic<1>,
            ) {
                assign o = i;
            }

            module top(
                i: input logic<1>,
                o: output logic<1>,
            ) {
                var i_c: logic<1>;
                var o_c: logic<1>;

                assign i_c = i;

                inst c: child(
                    i: i_c,
                    o: o_c,
                );

                assign o = o_c;
            }
        "#;

        let (sim_modules, ir) = setup(code);

        let top_module_ir = ir
            .components
            .iter()
            .find_map(|c| {
                if let Component::Module(m) = c {
                    if m.name.to_string() == "top" {
                        Some(m)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();
        let child_module_ir = ir
            .components
            .iter()
            .find_map(|c| {
                if let Component::Module(m) = c {
                    if m.name.to_string() == "child" {
                        Some(m)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();

        let top_module_sim = &sim_modules[&top_module_ir.name];
        let child_module_sim = &sim_modules[&child_module_ir.name];

        let mut instance_ids = HashMap::default();
        let top_path = InstancePath(vec![]);
        let child_instance_name = *top_module_sim.glue_blocks.keys().next().unwrap();
        let child_path = InstancePath(vec![(child_instance_name, 0)]);
        assert!(
            instance_ids
                .insert(top_path.clone(), InstanceId(0))
                .is_none()
        );
        assert!(instance_ids.insert(child_path, InstanceId(1)).is_none());

        let find_var_id = |sim_module: &SimModule, name: &str| {
            let str_id = StrId::from(name);
            let var_path = VarPath(vec![str_id]);
            sim_module.find_var_id(&var_path)
        };

        let top_i_id = find_var_id(top_module_sim, "i");
        let top_o_id = find_var_id(top_module_sim, "o");
        let top_ic_id = find_var_id(top_module_sim, "i_c");
        let top_oc_id = find_var_id(top_module_sim, "o_c");

        let child_i_id = find_var_id(child_module_sim, "i");
        let child_o_id = find_var_id(child_module_sim, "o");

        let mut arena = SLTNodeArena::new();
        let relocation_module = flatting(
            top_module_sim,
            &top_path,
            &instance_ids,
            &HashMap::default(),
            &mut arena,
            &crate::debug::TraceOptions::default(),
            None,
        );

        // Expected logic paths:
        // 1. i_c = i; (in top)
        // 2. o = o_c; (in top)
        // 3. c.i = i_c; (glue)
        // 4. o_c = c.o; (glue)
        assert_eq!(relocation_module.comb_blocks.len(), 4);
        let paths = &relocation_module.comb_blocks;

        // Check path 1: i_c = i
        let path1 = paths
            .iter()
            .find(|p| {
                p.target.id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: top_ic_id,
                    })
            })
            .unwrap();
        assert_eq!(path1.sources.len(), 1);
        assert!(path1.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: top_i_id
            },
            0,
            0
        )));

        // Check path 2: o = o_c
        let path2 = paths
            .iter()
            .find(|p| {
                p.target.id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: top_o_id,
                    })
            })
            .unwrap();
        assert_eq!(path2.sources.len(), 1);
        assert!(path2.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: top_oc_id
            },
            0,
            0
        )));

        // Check path 3: c.i = i_c (child input)
        let path3 = paths
            .iter()
            .find(|p| {
                p.target.id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(1),
                        var_id: child_i_id,
                    })
            })
            .unwrap();
        assert_eq!(path3.sources.len(), 1);
        assert!(path3.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: top_ic_id
            },
            0,
            0
        )));

        // Check path 4: o_c = c.o (child output)
        let path4 = paths
            .iter()
            .find(|p| {
                p.target.id
                    == (AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: top_oc_id,
                    })
            })
            .unwrap();
        assert_eq!(path4.sources.len(), 1);
        assert!(path4.sources.contains(&VarAtomBase::new(
            AbsoluteAddr {
                instance_id: InstanceId(1),
                var_id: child_o_id
            },
            0,
            0
        )));
    }
}
