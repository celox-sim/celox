use std::collections::BTreeSet;

use crate::ir::{
    BitAccess, BlockId, ExecutionUnit, GlueAddr, GlueBlock, SIRBuilder, SIRTerminator, SimModule,
    TriggerSet, VarAtomBase,
};

use crate::logic_tree::{
    LogicPath, SLTNode, SLTNodeArena, SymbolicStore, eval_expression, get_width, parse_comb,
    range_store::RangeStore,
};
use crate::parser::{
    ParserError, bitaccess::eval_var_select, bitslicer::BitSlicer, ff::FfParser,
    registry::ModuleRegistry,
};
use crate::{HashMap, HashSet};
use veryl_analyzer::ir::{Component, Declaration, InstDeclaration, Module, VarId};
use veryl_parser::resource_table::StrId;

pub struct ModuleParser<'a> {
    module: &'a Module,
    registry: &'a ModuleRegistry<'a>,
    slicer: BitSlicer,
    store: SymbolicStore<VarId>,
    comb_blocks: Vec<LogicPath<VarId>>,
    comb_boundaries: HashMap<VarId, BTreeSet<usize>>,
    glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    ff_parser: FfParser<'a>,
    arena: SLTNodeArena<VarId>,
}

fn resolve_module_name(component: &Component) -> StrId {
    match component {
        Component::Module(module) => module.name,
        Component::Interface(_) => {
            unreachable!("Interface component must be eliminated before resolve_module_name")
        }
        Component::SystemVerilog(system_verilog) => system_verilog.name,
    }
}

impl<'a> ModuleParser<'a> {
    pub fn parse(
        module: &'a Module,
        registry: &'a ModuleRegistry,
    ) -> Result<SimModule, ParserError> {
        let parser = Self::new(module, registry);
        parser.parse_inner()
    }

    fn new(module: &'a Module, registry: &'a ModuleRegistry) -> Self {
        Self {
            module,
            slicer: BitSlicer::new(module),
            registry,
            store: SymbolicStore::default(),
            comb_blocks: Vec::new(),
            comb_boundaries: HashMap::default(),
            glue_blocks: HashMap::default(),
            ff_parser: FfParser::new(module),
            arena: SLTNodeArena::new(),
        }
    }

    fn parse_comb_declaration(
        &mut self,
        decl: &veryl_analyzer::ir::CombDeclaration,
    ) -> Result<(), ParserError> {
        let (paths, store, boundaries) = parse_comb(self.module, decl, &mut self.arena)?;
        self.store.extend(store);
        self.comb_blocks.extend(paths);
        for (id, bounds) in boundaries {
            self.comb_boundaries.entry(id).or_default().extend(bounds);
        }
        Ok(())
    }

    fn parse_inst_declaration(&mut self, decl: &InstDeclaration) -> Result<(), ParserError> {
        if let Component::SystemVerilog(system_verilog) = &decl.component {
            return Err(ParserError::UnsupportedSimulatorParser {
                feature: "systemverilog module instantiation",
                detail: format!("name: {:?}", system_verilog.name),
            });
        }

        let module_name = resolve_module_name(&decl.component);

        // 1. Inputs (Parent -> Child)
        let mut input_ports = Vec::new();
        let mut glue_arena = SLTNodeArena::<GlueAddr>::new();

        // Parent context store
        let mut parent_store = SymbolicStore::default();
        for (id, var) in &self.module.variables {
            let width = var.total_width().unwrap_or(1) * var.r#type.total_array().unwrap_or(1);
            let initial_node = self.arena.alloc(SLTNode::Input {
                variable: *id,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            });
            let mut sources = HashSet::default();
            sources.insert(VarAtomBase::new(*id, 0, width - 1));
            parent_store.insert(*id, RangeStore::new(Some((initial_node, sources)), width));
        }

        for input in &decl.inputs {
            let ((expr_node, expr_sources), _bounds) = eval_expression(
                self.module,
                &parent_store,
                &input.expr,
                &mut self.arena,
                None,
            )?;

            // Map Parent VarId to GlueAddr::Parent
            let mut cache = HashMap::default();
            let mapped_node = self.arena.get(expr_node).map_addr(
                expr_node,
                &self.arena,
                &mut glue_arena,
                &mut cache,
                &|id| GlueAddr::Parent(*id),
            );
            let expr_width = get_width(expr_node, &self.arena);
            // Iterate over target child ports (input.id)
            let mut current_lsb = 0;

            for child_port_id in &input.id {
                let ty = self.registry.get_port_type(module_name, child_port_id);
                let width = ty.width();
                let access = BitAccess::new(current_lsb, current_lsb + width - 1);
                // Slice the expression
                let sliced = if access.lsb == 0 && access.msb == expr_width - 1 {
                    mapped_node
                } else {
                    glue_arena.alloc(SLTNode::Slice {
                        expr: mapped_node,
                        access,
                    })
                };

                let path = LogicPath {
                    target: VarAtomBase::new(GlueAddr::Child(*child_port_id), 0, width - 1),
                    expr: sliced,
                    sources: collect_glue_sources(sliced, &glue_arena),
                };

                let parent_vars: Vec<_> = expr_sources.iter().map(|s| s.id).collect();
                input_ports.push((parent_vars, path));
                current_lsb += width;
            }
        }

        // 2. Outputs (Child -> Parent)
        let mut output_ports = Vec::new();

        for output in &decl.outputs {
            // RHS: Concat of Child Ports.
            let mut parts = Vec::new();
            for child_port_id in &output.id {
                let ty = self.registry.get_port_type(module_name, child_port_id);
                let width = ty.width();
                let node = glue_arena.alloc(SLTNode::Input {
                    variable: GlueAddr::Child(*child_port_id),
                    index: vec![],
                    access: BitAccess::new(0, width - 1),
                });
                parts.push((node, width));
            }
            // Reverse parts so LSB is at the end (for SLTNode::Concat)
            parts.reverse();
            let rhs_node = if parts.len() == 1 {
                parts[0].0
            } else {
                glue_arena.alloc(SLTNode::Concat(parts))
            };

            // LHS: output.dst (AssignDestination).
            let mut current_offset = 0;
            // Iterate destinations from LSB (last in list for multi-dst assign usually? No wait)
            // `emit_multi_dst_assign` iterates `dsts.iter().rev()`.
            // So we strictly follow `emit_multi_dst_assign` logic.
            // "Current offset starts at 0" and "dst in dsts.iter().rev()".
            for dst in output.dst.iter().rev() {
                // Determine width of this part
                let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select);
                let part_width = access.msb - access.lsb + 1;

                // Extract this part from rhs_node
                let slice_access = BitAccess::new(current_offset, current_offset + part_width - 1);

                let rhs_part = if slice_access.lsb == 0
                    && slice_access.msb == get_width(rhs_node, &glue_arena) - 1
                {
                    rhs_node
                } else {
                    glue_arena.alloc(SLTNode::Slice {
                        expr: rhs_node,
                        access: slice_access,
                    })
                };

                // Emit path directly for this specific bit range assigned by the output
                // RHS_part is derived from RHS_node, which is Concat of inputs.
                // The sources should be the Union of all inputs involved.
                // Ideally we should filter sources that overlap with the slice, but for now union is safe.

                // Collect sources from rhs_node components manually
                let mut sources = HashSet::default();
                for child_port_id in &output.id {
                    // Each child port is a source
                    let ty = self.registry.get_port_type(module_name, child_port_id);
                    let width = ty.width();
                    sources.insert(VarAtomBase::new(
                        GlueAddr::Child(*child_port_id),
                        0,
                        width - 1,
                    ));
                }

                let path = LogicPath {
                    target: VarAtomBase::new(GlueAddr::Parent(dst.id), access.lsb, access.msb),
                    sources,
                    expr: rhs_part,
                };
                output_ports.push((vec![dst.id], path));

                current_offset += part_width;
            }
        }

        // Construct GlueBlock
        let block = GlueBlock {
            module_name,
            input_ports,
            output_ports,
            arena: glue_arena,
        };

        self.glue_blocks.entry(decl.name).or_default().push(block);
        Ok(())
    }

    fn parse_inner(mut self) -> Result<SimModule, ParserError> {
        let mut ff_groups: HashMap<TriggerSet<VarId>, Vec<&veryl_analyzer::ir::FfDeclaration>> =
            HashMap::default();

        // 1. Parse all declarations
        for decl in self.module.declarations.iter() {
            match decl {
                Declaration::Ff(ff_decl) => {
                    let trigger_set = self.ff_parser.detect_trigger_set(ff_decl);
                    ff_groups.entry(trigger_set).or_default().push(ff_decl);
                }
                Declaration::Comb(comb_decl) => {
                    self.parse_comb_declaration(comb_decl)?;
                }
                Declaration::Inst(inst_decl) => {
                    self.parse_inst_declaration(inst_decl)
                        .map_err(ParserError::from)?;
                }
                _ => {}
            }
        }

        // 2. Build FF blocks per trigger set.
        //    parse_ff_group emits only WORKING-region stores (pure eval).
        //    We build three variants:
        //      eval_only  = seeds (STABLE->WORKING) + stores
        //      apply      = commits (WORKING->STABLE) only
        //      eval_apply = eval_only with commits appended to the Return block
        let mut eval_only_ff_blocks = HashMap::default();
        let mut apply_ff_blocks = HashMap::default();
        let mut eval_apply_ff_blocks = HashMap::default();

        for (trigger_set, decls) in &ff_groups {
            // Shared commit list (WORKING -> STABLE), one entry per unique written var.
            let mut commits: Vec<crate::ir::SIRInstruction<crate::ir::RegionedVarAddr>> =
                Vec::new();
            let mut seen_var = HashSet::default();
            for var_id in FfParser::collect_written_vars(decls) {
                if seen_var.insert(var_id) {
                    let var = &self.module.variables[&var_id];
                    let width =
                        var.total_width().unwrap_or(1) * var.r#type.total_array().unwrap_or(1);
                    commits.push(crate::ir::SIRInstruction::Commit(
                        crate::ir::RegionedVarAddr {
                            region: crate::ir::WORKING_REGION,
                            var_id,
                        },
                        crate::ir::RegionedVarAddr {
                            region: crate::ir::STABLE_REGION,
                            var_id,
                        },
                        crate::ir::SIROffset::Static(0),
                        width,
                        Vec::new(),
                    ));
                }
            }

            // --- eval_only and eval_apply ---
            // Run parse_ff_group once. Clone the builder before sealing so that
            // eval_only and eval_apply are produced from independent builder states,
            // each with their own register namespace (no shared RegisterIds).
            let mut builder = SIRBuilder::new();
            self.ff_parser.parse_ff_group(decls, &mut builder)?;

            // Clone before sealing: eval_apply_builder gets the commit instructions appended.
            let mut eval_apply_builder = builder.clone();
            for commit in &commits {
                eval_apply_builder.emit(commit.clone());
            }

            // Seal and drain eval_only.
            builder.seal_block(SIRTerminator::Return);
            let (bbs, regs, _) = builder.drain();
            let mut eval_only_eu = ExecutionUnit {
                blocks: bbs,
                entry_block_id: BlockId(0),
                register_map: regs,
            };

            // Seal and drain eval_apply.
            eval_apply_builder.seal_block(SIRTerminator::Return);
            let (ea_bbs, ea_regs, _) = eval_apply_builder.drain();
            let mut eval_apply_eu = ExecutionUnit {
                blocks: ea_bbs,
                entry_block_id: BlockId(0),
                register_map: ea_regs,
            };

            // Build seeds (STABLE -> WORKING) and prepend to both eval_only and eval_apply.
            let mut seeds: Vec<crate::ir::SIRInstruction<crate::ir::RegionedVarAddr>> = Vec::new();
            let mut seen_seed = HashSet::default();
            for var_id in FfParser::collect_written_vars(decls) {
                if seen_seed.insert(var_id) {
                    let var = &self.module.variables[&var_id];
                    let width =
                        var.total_width().unwrap_or(1) * var.r#type.total_array().unwrap_or(1);
                    seeds.push(crate::ir::SIRInstruction::Commit(
                        crate::ir::RegionedVarAddr {
                            region: crate::ir::STABLE_REGION,
                            var_id,
                        },
                        crate::ir::RegionedVarAddr {
                            region: crate::ir::WORKING_REGION,
                            var_id,
                        },
                        crate::ir::SIROffset::Static(0),
                        width,
                        Vec::new(),
                    ));
                }
            }
            for eu in [&mut eval_only_eu, &mut eval_apply_eu] {
                if let Some(entry) = eu.blocks.get_mut(&BlockId(0)) {
                    let mut s = seeds.clone();
                    s.append(&mut entry.instructions);
                    entry.instructions = s;
                }
            }

            // --- apply: minimal EU containing only commit instructions ---
            let mut apply_builder = SIRBuilder::new();
            for commit in &commits {
                apply_builder.emit(commit.clone());
            }
            apply_builder.seal_block(SIRTerminator::Return);
            let (apply_bbs, apply_regs, _) = apply_builder.drain();
            let apply_eu = ExecutionUnit {
                blocks: apply_bbs,
                entry_block_id: BlockId(0),
                register_map: apply_regs,
            };

            eval_only_ff_blocks.insert(trigger_set.clone(), eval_only_eu);
            apply_ff_blocks.insert(trigger_set.clone(), apply_eu);
            eval_apply_ff_blocks.insert(trigger_set.clone(), eval_apply_eu);
        }

        // Keep both boundary sources:
        // - BitSlicer: assignment destination-based split points
        // - parse_comb: expression/read-driven split points
        let mut comb_boundaries = self.slicer.boundaries().clone();
        for (id, bounds) in self.comb_boundaries {
            comb_boundaries.entry(id).or_default().extend(bounds);
        }
        Ok(SimModule {
            variables: self.module.variables.clone(),
            name: self.module.name,
            glue_blocks: self.glue_blocks,
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            comb_blocks: self.comb_blocks,
            comb_boundaries,
            arena: self.arena,
            store: self.store,
        })
    }
}

fn collect_glue_sources(
    expr: crate::logic_tree::NodeId,
    arena: &SLTNodeArena<GlueAddr>,
) -> HashSet<VarAtomBase<GlueAddr>> {
    let mut set = HashSet::default();
    collect_glue_sources_with_window(expr, None, arena, &mut set);
    set
}

fn collect_glue_sources_with_window(
    expr: crate::logic_tree::NodeId,
    window: Option<BitAccess>,
    arena: &SLTNodeArena<GlueAddr>,
    set: &mut HashSet<VarAtomBase<GlueAddr>>,
) {
    match arena.get(expr) {
        SLTNode::Input {
            variable,
            access,
            index,
        } => {
            let full_width = access.msb - access.lsb + 1;
            let win = window.unwrap_or(BitAccess::new(0, full_width - 1));

            set.insert(VarAtomBase::new(
                *variable,
                access.lsb + win.lsb,
                access.lsb + win.msb,
            ));

            // Dynamic index expressions are full dependencies.
            for idx in index {
                collect_glue_sources_with_window(idx.node, None, arena, set);
            }
        }
        SLTNode::Slice { expr, access } => {
            let composed = if let Some(win) = window {
                BitAccess::new(access.lsb + win.lsb, access.lsb + win.msb)
            } else {
                *access
            };
            collect_glue_sources_with_window(*expr, Some(composed), arena, set);
        }
        SLTNode::Concat(parts) => {
            for (part, _) in parts {
                collect_glue_sources_with_window(*part, None, arena, set);
            }
        }
        SLTNode::Binary(lhs, _, rhs) => {
            collect_glue_sources_with_window(*lhs, None, arena, set);
            collect_glue_sources_with_window(*rhs, None, arena, set);
        }
        SLTNode::Unary(_, inner) => {
            collect_glue_sources_with_window(*inner, None, arena, set);
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_glue_sources_with_window(*cond, None, arena, set);
            collect_glue_sources_with_window(*then_expr, None, arena, set);
            collect_glue_sources_with_window(*else_expr, None, arena, set);
        }
        SLTNode::Constant(_, _, _) => {}
    }
}
