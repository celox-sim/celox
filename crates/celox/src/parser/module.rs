use std::collections::BTreeSet;

use crate::ir::{
    BitAccess, BlockId, ExecutionUnit, GlueAddr, GlueBlock, ModuleId, ModuleInitialMemoryValue,
    SIRBuilder, SIRTerminator, SimModule, TriggerSet, VarAtomBase,
};

use crate::logic_tree::{
    LogicPath, SLTNode, SLTNodeArena, SymbolicStore, eval_expression, get_width, parse_comb,
    range_store::RangeStore,
};
use crate::parser::{
    BuildConfig, LoweringPhase, ParserError, bitaccess::eval_var_select, bitslicer::BitSlicer,
    ff::FfParser, registry::get_port_type, resolve_total_width,
};
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    AssignDestination, Component, Declaration, Expression, InstDeclaration, Module, Statement,
    SystemFunctionInput, SystemFunctionKind, VarId,
};
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::resource_table::StrId;

pub struct ModuleParser<'a> {
    module: &'a Module,
    inst_ids: &'a [ModuleId],
    inst_idx: usize,
    slicer: BitSlicer,
    store: SymbolicStore<VarId>,
    comb_blocks: Vec<LogicPath<VarId>>,
    comb_boundaries: HashMap<VarId, BTreeSet<usize>>,
    glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    initial_memory_values: Vec<ModuleInitialMemoryValue>,
    ff_parser: FfParser<'a>,
    arena: SLTNodeArena<VarId>,
    reset_clock_map: HashMap<VarId, VarId>,
}

impl<'a> ModuleParser<'a> {
    pub fn parse(
        module: &'a Module,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
    ) -> Result<SimModule, ParserError> {
        let parser = Self::new(module, config, inst_ids)?;
        parser.parse_inner()
    }

    fn new(
        module: &'a Module,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
    ) -> Result<Self, ParserError> {
        Ok(Self {
            module,
            inst_ids,
            inst_idx: 0,
            slicer: BitSlicer::new(module)?,
            store: SymbolicStore::default(),
            comb_blocks: Vec::new(),
            comb_boundaries: HashMap::default(),
            glue_blocks: HashMap::default(),
            initial_memory_values: Vec::new(),
            ff_parser: FfParser::new(module, *config),
            arena: SLTNodeArena::new(),
            reset_clock_map: HashMap::default(),
        })
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

    fn parse_inst_declaration(
        &mut self,
        decl: &InstDeclaration,
        module_id: ModuleId,
    ) -> Result<(), ParserError> {
        if let Component::SystemVerilog(system_verilog) = &*decl.component {
            return Err(ParserError::unsupported(
                64,
                LoweringPhase::SimulatorParser,
                "systemverilog module instantiation",
                format!("name: \"{}\"", system_verilog.name),
                None,
            ));
        }

        let child_module = match &*decl.component {
            Component::Module(m) => m,
            _ => unreachable!(),
        };

        // 1. Inputs (Parent -> Child)
        let mut input_ports = Vec::new();
        let mut glue_arena = SLTNodeArena::<GlueAddr>::new();

        // Parent context store
        let mut parent_store = SymbolicStore::default();
        for (id, var) in &self.module.variables {
            let width = resolve_total_width(self.module, var)?;
            if width == 0 {
                parent_store.insert(*id, RangeStore::new(None, 0));
                continue;
            }
            let initial_node = self.arena.alloc(SLTNode::Input {
                variable: *id,
                signed: var.r#type.signed,
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
            let child_port_id = input.id;
            let ty = get_port_type(child_module, &child_port_id)?;
            let width = ty.width();
            let access = BitAccess::new(0, width - 1);
            let sliced = if expr_width > 0 && access.msb == expr_width - 1 {
                mapped_node
            } else {
                glue_arena.alloc(SLTNode::Slice {
                    expr: mapped_node,
                    access,
                })
            };

            let path = LogicPath {
                target: VarAtomBase::new(GlueAddr::Child(child_port_id), 0, width - 1),
                expr: sliced,
                sources: collect_glue_sources(sliced, &glue_arena),
            };

            let parent_vars: Vec<_> = expr_sources.iter().map(|s| s.id).collect();
            input_ports.push((parent_vars, path));
        }

        // 2. Outputs (Child -> Parent)
        let mut output_ports = Vec::new();

        for output in &decl.outputs {
            let child_port_id = output.id;
            let ty = get_port_type(child_module, &child_port_id)?;
            let width = ty.width();
            let rhs_node = glue_arena.alloc(SLTNode::Input {
                variable: GlueAddr::Child(child_port_id),
                signed: child_module.variables[&child_port_id].r#type.signed,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            });

            // LHS: output.dst (AssignDestination).
            let mut current_offset = 0;
            // Iterate destinations from LSB (last in list for multi-dst assign usually? No wait)
            // `emit_multi_dst_assign` iterates `dsts.iter().rev()`.
            // So we strictly follow `emit_multi_dst_assign` logic.
            // "Current offset starts at 0" and "dst in dsts.iter().rev()".
            for dst in output.dst.iter().rev() {
                // Determine width of this part
                let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select)?;
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
                sources.insert(VarAtomBase::new(
                    GlueAddr::Child(child_port_id),
                    0,
                    width - 1,
                ));

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
            module_id,
            input_ports,
            output_ports,
            arena: glue_arena,
        };

        self.glue_blocks.entry(decl.name).or_default().push(block);
        Ok(())
    }

    fn static_string_expr(expr: &Expression) -> Option<String> {
        if !expr.comptime().r#type.is_string() {
            return None;
        }
        let value = expr.comptime().get_value().ok()?;
        byte_value_to_string(value)
    }

    fn parse_initial_declaration(
        &mut self,
        decl: &veryl_analyzer::ir::InitialDeclaration,
    ) -> Result<(), ParserError> {
        for stmt in &decl.statements {
            self.parse_initial_statement(stmt)?;
        }
        Ok(())
    }

    fn parse_initial_statement(&mut self, stmt: &Statement) -> Result<(), ParserError> {
        match stmt {
            Statement::SystemFunctionCall(call) => {
                if let SystemFunctionKind::Readmemh(filename, output) = &call.kind {
                    let value = self.parse_readmem_file(filename, output.0.as_slice(), 16)?;
                    self.initial_memory_values.push(value);
                }
                Ok(())
            }
            Statement::Null => Ok(()),
            Statement::Unsupported(token) => Err(ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "initial statement",
                "unsupported initial statement; only direct $readmemh calls are currently lowered",
                Some(token),
            )),
            _ => Ok(()),
        }
    }

    fn parse_readmem_file(
        &self,
        filename_arg: &SystemFunctionInput,
        output: &[AssignDestination],
        radix: u32,
    ) -> Result<ModuleInitialMemoryValue, ParserError> {
        let Some(filename) = Self::static_string_expr(&filename_arg.0) else {
            return Err(ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh filename expression",
                "filename must be a compile-time string",
                Some(&filename_arg.0.comptime().token),
            ));
        };
        let dst = match output {
            [dst] if dst.index.0.is_empty() && dst.select.is_empty() && dst.select.1.is_none() => {
                dst
            }
            [dst] => {
                return Err(ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh destination",
                    "destination must be a whole unpacked array variable",
                    Some(&dst.token),
                ));
            }
            _ => {
                return Err(ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh destination",
                    "concatenated destinations are not supported",
                    None,
                ));
            }
        };

        let var = &self.module.variables[&dst.id];
        let depth = var.r#type.total_array().ok_or_else(|| {
            ParserError::unresolved_width(self.module, var, var.r#type.to_string())
        })?;
        if depth <= 1 {
            return Err(ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh destination",
                "destination must be an unpacked array",
                Some(&dst.token),
            ));
        }

        let total_width = resolve_total_width(self.module, var)?;
        let element_width = total_width / depth;
        if element_width == 0 || element_width * depth != total_width {
            return Err(ParserError::unresolved_width(
                self.module,
                var,
                var.r#type.to_string(),
            ));
        }

        let path = self.resolve_readmem_path(&filename, &filename_arg.0.comptime().token);
        let content = std::fs::read_to_string(&path).map_err(|err| {
            ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh file",
                format!("failed to read {}: {err}", path.display()),
                Some(&filename_arg.0.comptime().token),
            )
        })?;
        let words = parse_memory_content(&content, radix, element_width)?;
        let mut value = BigUint::default();
        let mut mask = BigUint::default();
        let mut written_mask = BigUint::default();
        let element_bits = (BigUint::from(1u8) << element_width) - BigUint::from(1u8);

        for (addr, word_value, word_mask) in words {
            if addr >= depth {
                return Err(ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh address",
                    format!("address {addr} exceeds destination depth {depth}"),
                    Some(&dst.token),
                ));
            }
            let shift = addr * element_width;
            value |= word_value << shift;
            mask |= word_mask << shift;
            written_mask |= &element_bits << shift;
        }

        Ok(ModuleInitialMemoryValue {
            var_id: dst.id,
            value,
            mask,
            written_mask,
        })
    }

    fn resolve_readmem_path(
        &self,
        filename: &str,
        token: &veryl_parser::token_range::TokenRange,
    ) -> std::path::PathBuf {
        let path = std::path::PathBuf::from(filename);
        if path.is_absolute() {
            return path;
        }

        let source_path = token.beg.source.to_string();
        if source_path.is_empty() {
            return path;
        }
        std::path::Path::new(&source_path)
            .parent()
            .map(|parent| parent.join(&path))
            .unwrap_or(path)
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
                    // Build reset -> clock mapping
                    if let Some(reset) = &ff_decl.reset {
                        self.reset_clock_map.insert(reset.id, ff_decl.clock.id);
                    }
                }
                Declaration::Comb(comb_decl) => {
                    self.parse_comb_declaration(comb_decl)?;
                }
                Declaration::Inst(inst_decl) => {
                    let mid = self.inst_ids[self.inst_idx];
                    self.inst_idx += 1;
                    self.parse_inst_declaration(inst_decl, mid)?;
                }
                Declaration::Initial(init_decl) => {
                    self.parse_initial_declaration(init_decl)?;
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
                    let width = resolve_total_width(self.module, var)?;
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
                    let width = resolve_total_width(self.module, var)?;
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
            runtime_errors: self.ff_parser.runtime_errors().clone(),
            runtime_event_sites: self.ff_parser.runtime_event_sites().clone(),
            initial_memory_values: self.initial_memory_values,
            comb_boundaries,
            arena: self.arena,
            store: self.store,
            reset_clock_map: self.reset_clock_map,
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
            ..
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
        SLTNode::ForFold {
            loop_var,
            start,
            end,
            initials,
            updates,
            continue_cond,
            ..
        } => {
            if let crate::logic_tree::SLTLoopBound::Expr(node) = start {
                collect_glue_sources_with_window(*node, None, arena, set);
            }
            if let crate::logic_tree::SLTLoopBound::Expr(node) = end {
                collect_glue_sources_with_window(*node, None, arena, set);
            }
            for init in initials {
                collect_glue_sources_with_window(init.expr, None, arena, set);
            }
            for update in updates {
                collect_glue_sources_with_window(update.expr, None, arena, set);
            }
            collect_glue_sources_with_window(*continue_cond, None, arena, set);
            set.retain(|atom| atom.id != *loop_var);
        }
        SLTNode::Constant(_, _, _, _) => {}
    }
}

fn parse_memory_content(
    content: &str,
    radix: u32,
    width: usize,
) -> Result<Vec<(usize, BigUint, BigUint)>, ParserError> {
    let mut result = Vec::new();
    let mut addr = 0usize;
    for token in memory_tokens(content) {
        if let Some(address) = token.strip_prefix('@') {
            addr = usize::from_str_radix(address, 16).map_err(|err| {
                ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh address",
                    format!("invalid address directive {token}: {err}"),
                    None,
                )
            })?;
            continue;
        }
        let (value, mask) = parse_memory_word(&token, radix, width)?;
        result.push((addr, value, mask));
        addr += 1;
    }
    Ok(result)
}

fn memory_tokens(content: &str) -> Vec<String> {
    let mut out = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'/' => {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    out.push(' ');
                    continue;
                }
                b'*' => {
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(bytes.len());
                    out.push(' ');
                    continue;
                }
                _ => {}
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out.split_whitespace()
        .map(|token| token.replace('_', ""))
        .filter(|token| !token.is_empty())
        .collect()
}

fn parse_memory_word(
    token: &str,
    radix: u32,
    width: usize,
) -> Result<(BigUint, BigUint), ParserError> {
    let bits_per_digit = match radix {
        2 => 1,
        16 => 4,
        _ => unreachable!(),
    };
    let mut value = BigUint::default();
    let mut mask = BigUint::default();
    for ch in token.chars() {
        value <<= bits_per_digit;
        mask <<= bits_per_digit;
        match ch {
            '0'..='9' | 'a'..='f' | 'A'..='F' => {
                let Some(digit) = ch.to_digit(radix) else {
                    return Err(invalid_memory_word(token));
                };
                value |= BigUint::from(digit);
            }
            'x' | 'X' | '?' => {
                mask |= (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
            }
            'z' | 'Z' => {
                let unknown = (BigUint::from(1u8) << bits_per_digit) - BigUint::from(1u8);
                value |= &unknown;
                mask |= unknown;
            }
            _ => return Err(invalid_memory_word(token)),
        }
    }

    if width == 0 {
        return Ok((BigUint::default(), BigUint::default()));
    }
    let keep = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    Ok((value & &keep, mask & keep))
}

fn invalid_memory_word(token: &str) -> ParserError {
    ParserError::unsupported(
        111,
        LoweringPhase::SimulatorParser,
        "$readmemh data",
        format!("invalid data token {token}"),
        None,
    )
}
