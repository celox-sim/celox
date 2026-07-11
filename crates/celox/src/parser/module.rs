use std::collections::BTreeSet;
use std::time::Instant;

use crate::ir::{
    BinaryOp, BitAccess, BlockId, CombObserver, ExecutionUnit, GlueAddr, GlueBlock,
    InitialMemoryData, InitialMemoryWriteRun, ModuleId, ModuleInitialMemoryValue, RegionedVarAddr,
    RuntimeEventSite, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator, STABLE_REGION,
    SimModule, TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION,
};

use crate::logic_tree::{
    LogicPath, LogicPathTarget, SLTNode, SLTNodeArena, SLTNodeFacts, SymbolicStore,
    coerce_node_width, eval_assignment_expression, eval_expression, get_width, parse_comb,
    range_store::RangeStore,
};
use crate::parser::{
    BuildConfig, LoweringPhase, ParserError,
    bitaccess::{
        PartSelectGeometry, eval_var_select, get_access_width, is_static_access, select_geometry,
    },
    bitslicer::BitSlicer,
    ff::FfParser,
    registry::get_port_type,
    resolve_total_width,
};
use crate::{HashMap, HashSet};
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    AssignDestination, Component, Declaration, Expression, InstDeclaration, Module, Statement,
    SystemFunctionInput, SystemFunctionKind, VarId,
};
use veryl_analyzer::value::Value;
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::resource_table::StrId;

pub struct ModuleParser<'a> {
    module: &'a Module,
    inst_ids: &'a [ModuleId],
    inst_idx: usize,
    slicer: BitSlicer,
    store: SymbolicStore<VarId>,
    comb_blocks: Vec<LogicPath<VarId>>,
    comb_observers: Vec<CombObserver<VarId>>,
    comb_runtime_event_sites: Vec<RuntimeEventSite>,
    comb_boundaries: HashMap<VarId, BTreeSet<usize>>,
    glue_blocks: HashMap<StrId, Vec<GlueBlock>>,
    initial_memory_values: Vec<ModuleInitialMemoryValue>,
    ff_parser: FfParser<'a>,
    arena: SLTNodeArena<VarId>,
    reset_clock_map: HashMap<VarId, VarId>,
}

fn build_dynamic_output_glue(
    module: &Module,
    parent_store: &SymbolicStore<VarId>,
    parent_arena: &mut SLTNodeArena<VarId>,
    glue_arena: &mut SLTNodeArena<GlueAddr>,
    dst: &AssignDestination,
    rhs: crate::logic_tree::NodeId,
    rhs_signed: bool,
) -> Result<
    (
        crate::logic_tree::NodeId,
        BitAccess,
        HashSet<VarAtomBase<GlueAddr>>,
        HashSet<VarAtomBase<GlueAddr>>,
        HashSet<VarAtomBase<GlueAddr>>,
    ),
    ParserError,
> {
    let geometry = select_geometry(module, dst.id, &dst.index, &dst.select)?;
    let mut offset = glue_arena.alloc(SLTNode::Constant(
        BigUint::from(0u8),
        BigUint::from(0u8),
        64,
        false,
    ));

    let mut sources = collect_glue_sources(rhs, glue_arena);
    let mut address_sources = HashSet::default();

    let mut index_exprs = dst.index.0.clone();
    index_exprs.extend(dst.select.0.clone());
    let dim_limit = geometry.dimension_count;

    for (dimension, index_expr) in index_exprs[..dim_limit].iter().enumerate() {
        let ((index, _), _) =
            eval_expression(module, parent_store, index_expr, parent_arena, None)?;
        let mut cache = HashMap::default();
        let mapped =
            parent_arena
                .get(index)
                .map_addr(index, parent_arena, glue_arena, &mut cache, &|id| {
                    GlueAddr::Parent(*id)
                });
        let mapped_sources = collect_glue_sources(mapped, glue_arena);
        sources.extend(mapped_sources.iter().copied());
        address_sources.extend(mapped_sources);
        let Some(stride) = geometry.strides.get(dimension).copied() else {
            return Err(ParserError::illegal_context(
                "dynamic output port destination",
                format!(
                    "index dimension {dimension} is outside the {}-dimension destination",
                    geometry.strides.len()
                ),
                Some(&dst.token),
            ));
        };
        let stride = glue_arena.alloc(SLTNode::Constant(
            BigUint::from(stride),
            BigUint::from(0u8),
            64,
            false,
        ));
        let term = glue_arena.alloc(SLTNode::Binary(mapped, BinaryOp::Mul, stride));
        offset = glue_arena.alloc(SLTNode::Binary(offset, BinaryOp::Add, term));
    }

    if let Some(part) = geometry.part {
        let anchor_expr = index_exprs.last().ok_or_else(|| {
            ParserError::illegal_context(
                "dynamic output port destination",
                "part select is missing its anchor expression",
                Some(&dst.token),
            )
        })?;
        let Some(weight) = geometry.strides.get(dim_limit).copied() else {
            return Err(ParserError::illegal_context(
                "dynamic output port destination",
                format!(
                    "part-select dimension {dim_limit} is outside the {}-dimension destination",
                    geometry.strides.len()
                ),
                Some(&dst.token),
            ));
        };
        let part_offset = match part {
            PartSelectGeometry::Colon { lsb, .. } => {
                let bit_offset = lsb.checked_mul(weight).ok_or_else(|| {
                    ParserError::illegal_context(
                        "dynamic output port destination",
                        "colon-select offset overflows usize",
                        Some(&dst.token),
                    )
                })?;
                glue_arena.alloc(SLTNode::Constant(
                    BigUint::from(bit_offset),
                    BigUint::from(0u8),
                    64,
                    false,
                ))
            }
            PartSelectGeometry::PlusColon { .. }
            | PartSelectGeometry::MinusColon { .. }
            | PartSelectGeometry::Step { .. } => {
                let ((anchor, _), _) =
                    eval_expression(module, parent_store, anchor_expr, parent_arena, None)?;
                let mut cache = HashMap::default();
                let anchor = parent_arena.get(anchor).map_addr(
                    anchor,
                    parent_arena,
                    glue_arena,
                    &mut cache,
                    &|id| GlueAddr::Parent(*id),
                );
                let mapped_sources = collect_glue_sources(anchor, glue_arena);
                sources.extend(mapped_sources.iter().copied());
                address_sources.extend(mapped_sources);

                let element_offset = match part {
                    PartSelectGeometry::PlusColon { .. } => anchor,
                    PartSelectGeometry::MinusColon { elements } => {
                        let decrement = elements.checked_sub(1).ok_or_else(|| {
                            ParserError::illegal_context(
                                "dynamic output port destination",
                                "minus-colon width underflows",
                                Some(&dst.token),
                            )
                        })?;
                        let decrement = glue_arena.alloc(SLTNode::Constant(
                            BigUint::from(decrement),
                            BigUint::from(0u8),
                            64,
                            false,
                        ));
                        glue_arena.alloc(SLTNode::Binary(anchor, BinaryOp::Sub, decrement))
                    }
                    PartSelectGeometry::Step { elements } => {
                        let elements = glue_arena.alloc(SLTNode::Constant(
                            BigUint::from(elements),
                            BigUint::from(0u8),
                            64,
                            false,
                        ));
                        glue_arena.alloc(SLTNode::Binary(anchor, BinaryOp::Mul, elements))
                    }
                    PartSelectGeometry::Colon { .. } => {
                        return Err(ParserError::illegal_context(
                            "dynamic output port destination",
                            "inconsistent colon-select geometry",
                            Some(&dst.token),
                        ));
                    }
                };
                if weight == 1 {
                    element_offset
                } else {
                    let weight = glue_arena.alloc(SLTNode::Constant(
                        BigUint::from(weight),
                        BigUint::from(0u8),
                        64,
                        false,
                    ));
                    glue_arena.alloc(SLTNode::Binary(element_offset, BinaryOp::Mul, weight))
                }
            }
        };
        offset = glue_arena.alloc(SLTNode::Binary(offset, BinaryOp::Add, part_offset));
    }

    let access_width = get_access_width(module, dst.id, &dst.index, &dst.select)?;
    let variable = &module.variables[&dst.id];
    let variable_width = resolve_total_width(module, variable)?;
    if variable_width == 0 || access_width == 0 || access_width > variable_width {
        return Err(ParserError::illegal_context(
            "dynamic output port destination",
            format!("destination width {access_width} must be in 1..={variable_width}"),
            Some(&dst.token),
        ));
    }
    let full_access = BitAccess::new(0, variable_width - 1);
    let old_value = glue_arena.alloc(SLTNode::Input {
        variable: GlueAddr::Parent(dst.id),
        signed: variable.r#type.signed,
        index: Vec::new(),
        access: full_access,
    });

    let low_mask = (BigUint::from(1u8) << access_width) - BigUint::from(1u8);
    let low_mask = glue_arena.alloc(SLTNode::Constant(
        low_mask,
        BigUint::from(0u8),
        variable_width,
        false,
    ));
    let shifted_mask = glue_arena.alloc(SLTNode::Binary(low_mask, BinaryOp::Shl, offset));
    let keep_mask = glue_arena.alloc(SLTNode::Unary(UnaryOp::BitNot, shifted_mask));

    // First apply assignment coercion to the selected destination width.  Only
    // after truncation/sign-extension is complete may the value be embedded in
    // the full variable; otherwise high RHS bits can corrupt adjacent fields.
    let rhs = coerce_node_width(glue_arena, rhs, Some(access_width), rhs_signed);
    let rhs = if access_width < variable_width {
        let padding_width = variable_width - access_width;
        let padding = glue_arena.alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            padding_width,
            false,
        ));
        glue_arena.alloc(SLTNode::Concat(vec![
            (padding, padding_width),
            (rhs, access_width),
        ]))
    } else {
        rhs
    };
    let shifted_rhs = glue_arena.alloc(SLTNode::Binary(rhs, BinaryOp::Shl, offset));
    let shifted_rhs = glue_arena.alloc(SLTNode::Binary(shifted_rhs, BinaryOp::And, shifted_mask));
    let kept_value = glue_arena.alloc(SLTNode::Binary(old_value, BinaryOp::And, keep_mask));
    let updated_value = glue_arena.alloc(SLTNode::Binary(kept_value, BinaryOp::Or, shifted_rhs));

    let prefix = eval_var_select(module, dst.id, &dst.index, &dst.select)?;
    let result = if prefix == full_access {
        updated_value
    } else {
        glue_arena.alloc(SLTNode::Slice {
            expr: updated_value,
            access: prefix,
        })
    };
    let previous_sources = std::iter::once(VarAtomBase::new(
        GlueAddr::Parent(dst.id),
        prefix.lsb,
        prefix.msb,
    ))
    .collect();
    Ok((result, prefix, sources, previous_sources, address_sources))
}

fn verify_glue_block(
    block: &GlueBlock,
    variable_widths: &HashMap<GlueAddr, usize>,
) -> Result<(), ParserError> {
    const PHASE: &str = "after module glue lowering";
    let facts = SLTNodeFacts::verify(&block.arena).map_err(|error| ParserError::SltVerify {
        phase: PHASE,
        error,
    })?;
    let fail = |invariant, node, message| ParserError::SltVerify {
        phase: PHASE,
        error: crate::logic_tree::SLTNodeFactsError::new(invariant, node, message),
    };
    let verify_atom = |atom: &VarAtomBase<GlueAddr>,
                       role: &'static str,
                       node: crate::logic_tree::NodeId|
     -> Result<usize, ParserError> {
        let width = atom
            .access
            .msb
            .checked_sub(atom.access.lsb)
            .and_then(|span| span.checked_add(1))
            .ok_or_else(|| {
                fail(
                    "ROOT.ACCESS_ORDERED_REPRESENTABLE",
                    node,
                    format!(
                        "{role} access [{}:{}] is malformed",
                        atom.access.msb, atom.access.lsb
                    ),
                )
            })?;
        let Some(&variable_width) = variable_widths.get(&atom.id) else {
            return Err(fail(
                "ROOT.VARIABLE_EXISTS",
                node,
                format!("{role} variable is absent from the glue semantic type table"),
            ));
        };
        if variable_width == 0 || atom.access.msb >= variable_width {
            return Err(fail(
                "ROOT.ACCESS_IN_VARIABLE_BOUNDS",
                node,
                format!(
                    "{role} access [{}:{}] is outside variable width {variable_width}",
                    atom.access.msb, atom.access.lsb
                ),
            ));
        }
        Ok(width)
    };

    for (node_index, node) in block.arena.iter().enumerate() {
        if let SLTNode::Input {
            variable, access, ..
        } = node
        {
            verify_atom(
                &VarAtomBase {
                    id: *variable,
                    access: *access,
                },
                "glue input",
                crate::logic_tree::NodeId(node_index),
            )?;
        }
    }

    for (_, path) in block.input_ports.iter().chain(&block.output_ports) {
        let expression_width = facts
            .require_lowerable(path.expr, "glue-path result")
            .map_err(|error| ParserError::SltVerify {
                phase: PHASE,
                error,
            })?;
        let Some(target) = path.target.var() else {
            return Err(fail(
                "ROOT.GLUE_TARGET_IS_VARIABLE",
                path.expr,
                "glue path has a non-variable target".to_string(),
            ));
        };
        let target_width = verify_atom(target, "glue target", path.expr)?;
        if expression_width != target_width {
            return Err(fail(
                "ROOT.RESULT_WIDTH_MATCHES_TARGET",
                path.expr,
                format!(
                    "glue result width {expression_width} does not equal target width {target_width}"
                ),
            ));
        }
        for source in path
            .sources
            .iter()
            .chain(&path.previous_sources)
            .chain(&path.address_sources)
        {
            verify_atom(source, "glue source", path.expr)?;
        }
        for address in &path.address_sources {
            if !path
                .sources
                .iter()
                .any(|source| source.id == address.id && source.access.overlaps(&address.access))
            {
                return Err(fail(
                    "ROOT.ADDRESS_SOURCE_IS_CURRENT_SOURCE",
                    path.expr,
                    "glue address source is absent from the current-value sources".to_string(),
                ));
            }
        }
        for &node in path
            .pre_lower_nodes
            .iter()
            .chain(path.local_inputs.iter().map(|(_, node)| node))
        {
            facts
                .require_lowerable(node, "glue path auxiliary value")
                .map_err(|error| ParserError::SltVerify {
                    phase: PHASE,
                    error,
                })?;
        }
    }
    Ok(())
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
            comb_observers: Vec::new(),
            comb_runtime_event_sites: Vec::new(),
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
        let arena_start = self.arena.len();
        let (paths, store, boundaries, mut observers, sites) =
            parse_comb(self.module, decl, &mut self.arena)?;
        let site_offset = self.comb_runtime_event_sites.len();
        for observer in &mut observers {
            observer.site_id += site_offset as u32;
            observer.activation_group = site_offset as u32;
        }
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, arena_start..arena_end, site_offset as u32)?;
        self.store.extend(store);
        self.comb_blocks.extend(paths);
        self.comb_observers.extend(observers);
        self.comb_runtime_event_sites.extend(sites);
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
            let child_port_id = input.id;
            let ty = get_port_type(child_module, &child_port_id)?;
            let width = ty.width();
            if width == 0 {
                return Err(ParserError::illegal_context(
                    "input port connection",
                    "child input port has zero width",
                    Some(&input.expr.token_range()),
                ));
            }
            let ((expr_node, expr_sources), _bounds) = eval_assignment_expression(
                self.module,
                &parent_store,
                &input.expr,
                &mut self.arena,
                width,
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

            let path = LogicPath {
                target: LogicPathTarget::Var(VarAtomBase::new(
                    GlueAddr::Child(child_port_id),
                    0,
                    width - 1,
                )),
                expr: mapped_node,
                sources: collect_glue_sources(mapped_node, &glue_arena),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                pre_lower_nodes: Vec::new(),
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
            if width == 0 {
                return Err(ParserError::illegal_context(
                    "output port connection",
                    "child output port has zero width",
                    output.dst.first().map(|destination| &destination.token),
                ));
            }
            let child_port = child_module.variables.get(&child_port_id).ok_or_else(|| {
                ParserError::illegal_context(
                    "output port connection",
                    "child output variable is absent from the semantic module",
                    output.dst.first().map(|destination| &destination.token),
                )
            })?;
            let rhs_node = glue_arena.alloc(SLTNode::Input {
                variable: GlueAddr::Child(child_port_id),
                signed: child_port.r#type.signed,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            });

            // LHS: output.dst (AssignDestination).
            let mut current_offset = 0usize;
            // Iterate destinations from LSB (last in list for multi-dst assign usually? No wait)
            // `emit_multi_dst_assign` iterates `dsts.iter().rev()`.
            // So we strictly follow `emit_multi_dst_assign` logic.
            // "Current offset starts at 0" and "dst in dsts.iter().rev()".
            for dst in output.dst.iter().rev() {
                let prefix_access = eval_var_select(self.module, dst.id, &dst.index, &dst.select)?;
                let part_width = get_access_width(self.module, dst.id, &dst.index, &dst.select)?;

                // Extract this part from rhs_node
                let slice_end = current_offset.checked_add(part_width).ok_or_else(|| {
                    ParserError::illegal_context(
                        "output port destination",
                        "concatenated destination width overflows usize",
                        Some(&dst.token),
                    )
                })?;
                if part_width == 0 || slice_end > width {
                    return Err(ParserError::illegal_context(
                        "output port destination",
                        format!(
                            "destination slice {current_offset}..{slice_end} does not fit output width {width}"
                        ),
                        Some(&dst.token),
                    ));
                }
                let slice_access = BitAccess::new(current_offset, slice_end - 1);

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

                let (expr, access, sources, previous_sources, address_sources) =
                    if is_static_access(&dst.index, &dst.select) {
                        let mut sources = HashSet::default();
                        sources.insert(VarAtomBase::new(
                            GlueAddr::Child(child_port_id),
                            0,
                            width - 1,
                        ));
                        (
                            rhs_part,
                            prefix_access,
                            sources,
                            HashSet::default(),
                            HashSet::default(),
                        )
                    } else {
                        build_dynamic_output_glue(
                            self.module,
                            &parent_store,
                            &mut self.arena,
                            &mut glue_arena,
                            dst,
                            rhs_part,
                            output.dst.len() == 1
                                && child_module.variables[&child_port_id].r#type.signed,
                        )?
                    };

                let path = LogicPath {
                    target: LogicPathTarget::Var(VarAtomBase::new(
                        GlueAddr::Parent(dst.id),
                        access.lsb,
                        access.msb,
                    )),
                    sources,
                    previous_sources,
                    address_sources,
                    local_inputs: Vec::new(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    pre_lower_nodes: Vec::new(),
                    expr,
                };
                output_ports.push((vec![dst.id], path));

                current_offset = slice_end;
            }
            if current_offset != width {
                return Err(ParserError::illegal_context(
                    "output port destination",
                    format!(
                        "concatenated destinations cover {current_offset} bits, but child output has width {width}"
                    ),
                    output.dst.first().map(|dst| &dst.token),
                ));
            }
        }

        // Construct GlueBlock
        let block = GlueBlock {
            module_id,
            input_ports,
            output_ports,
            arena: glue_arena,
        };

        let mut glue_widths = HashMap::default();
        for (id, variable) in &self.module.variables {
            glue_widths.insert(
                GlueAddr::Parent(*id),
                resolve_total_width(self.module, variable)?,
            );
        }
        for (id, variable) in &child_module.variables {
            glue_widths.insert(
                GlueAddr::Child(*id),
                resolve_total_width(child_module, variable)?,
            );
        }
        verify_glue_block(&block, &glue_widths)?;

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
        let mut context = veryl_analyzer::Context::default();
        context.variables = self.module.variables.clone();
        for stmt in &decl.statements {
            self.parse_initial_statement(stmt, &mut context)?;
        }
        Ok(())
    }

    fn parse_initial_statement(
        &mut self,
        stmt: &Statement,
        context: &mut veryl_analyzer::Context,
    ) -> Result<(), ParserError> {
        match stmt {
            Statement::SystemFunctionCall(call) => {
                if let SystemFunctionKind::Readmemh(filename, output) = &call.kind {
                    let value =
                        self.parse_readmem_file(filename, output.0.as_slice(), 16, context)?;
                    self.initial_memory_values.push(value);
                }
                Ok(())
            }
            Statement::If(if_stmt) => {
                let cond = if_stmt
                    .cond
                    .clone()
                    .eval_value(context)
                    .and_then(|value| value.to_usize());
                let Some(cond) = cond else {
                    return Ok(());
                };
                let branch = if cond != 0 {
                    &if_stmt.true_side
                } else {
                    &if_stmt.false_side
                };
                for stmt in branch {
                    self.parse_initial_statement(stmt, context)?;
                }
                Ok(())
            }
            Statement::For(for_stmt) => {
                let Some(iter) = for_stmt.range.eval_iter(context) else {
                    return Ok(());
                };
                for i in iter {
                    if let Some(var) = context.variables.get_mut(&for_stmt.var_id)
                        && let Some(total_width) = for_stmt.var_type.total_width()
                    {
                        let val = Value::new(i as u64, total_width, for_stmt.var_type.signed);
                        var.set_value(&[], val, None);
                    }
                    for stmt in &for_stmt.body {
                        self.parse_initial_statement(stmt, context)?;
                    }
                }
                Ok(())
            }
            Statement::Null => Ok(()),
            Statement::Unsupported(token) => Err(ParserError::illegal_context(
                "initial statement",
                "only direct $readmemh calls are valid in simulator-lowered initial blocks",
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
        context: &mut veryl_analyzer::Context,
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
            [dst] if dst.select.is_empty() && dst.select.1.is_none() => dst,
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
        let start_addr = if dst.index.0.is_empty() {
            0
        } else {
            let Some(indices) = dst.index.eval_value(context) else {
                return Err(ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh destination index",
                    "destination index must be compile-time constant",
                    Some(&dst.token),
                ));
            };
            let Some(index) = var.r#type.array.calc_index(&indices) else {
                return Err(ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh destination index",
                    format!("destination index {indices:?} is out of range"),
                    Some(&dst.token),
                ));
            };
            index
        };
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
        let timing = readmem_timing_enabled();
        let total_start = timing.then(Instant::now);
        if timing {
            eprintln!(
                "[readmem-timing] start file={} depth={} element_width={} start_addr={} radix={}",
                path.display(),
                depth,
                element_width,
                start_addr,
                radix
            );
        }

        let read_start = timing.then(Instant::now);
        let content = std::fs::read_to_string(&path).map_err(|err| {
            ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh file",
                format!("failed to read {}: {err}", path.display()),
                Some(&filename_arg.0.comptime().token),
            )
        })?;
        if let Some(start) = read_start {
            eprintln!(
                "[readmem-timing] read file={} bytes={} elapsed={:?}",
                path.display(),
                content.len(),
                start.elapsed()
            );
        }

        let parse_start = timing.then(Instant::now);
        let writes = parse_memory_write_runs(
            &content,
            radix,
            element_width,
            start_addr,
            depth,
            &dst.token,
        )?;
        if let Some(start) = parse_start {
            eprintln!(
                "[readmem-timing] parse file={} words={} runs={} elapsed={:?}",
                path.display(),
                writes.words,
                writes.runs.len(),
                start.elapsed()
            );
        }
        if let Some(start) = total_start {
            eprintln!(
                "[readmem-timing] done file={} elapsed={:?}",
                path.display(),
                start.elapsed()
            );
        }

        Ok(ModuleInitialMemoryValue {
            var_id: dst.id,
            data: InitialMemoryData::Writes(writes.runs),
        })
    }

    fn resolve_readmem_path(
        &self,
        filename: &str,
        token: &veryl_parser::token_range::TokenRange,
    ) -> std::path::PathBuf {
        let source_path = token.beg.source.to_string();
        let source_path = (!source_path.is_empty()).then(|| std::path::Path::new(&source_path));
        let cwd = std::env::current_dir().ok();
        resolve_readmem_path_with_fallback(filename, source_path, cwd.as_deref())
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
            // --- eval_only and eval_apply ---
            // Run parse_ff_group once. Clone the builder before sealing so that
            // eval_only and eval_apply are produced from independent builder states,
            // each with their own register namespace (no shared RegisterIds).
            let mut builder = SIRBuilder::new();
            let ff_group = self.ff_parser.parse_ff_group(decls, &mut builder)?;
            let targets = ff_group.targets;
            let dynamic_write_vars = ff_group.dynamic_write_vars;
            let commits = build_ff_region_copies(&targets, WORKING_REGION, STABLE_REGION);
            let eval_apply_commits = build_ff_region_copies_skipping(
                &targets,
                WORKING_REGION,
                STABLE_REGION,
                &dynamic_write_vars,
            );

            // Clone before sealing: eval_apply_builder gets the commit instructions appended.
            let mut eval_apply_builder = builder.clone();
            for commit in &eval_apply_commits {
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
            rewrite_working_accesses_to_stable(&mut eval_apply_eu, &dynamic_write_vars);

            // Build seeds (STABLE -> WORKING) and prepend to both eval_only and eval_apply.
            let seeds = build_ff_region_copies(&targets, STABLE_REGION, WORKING_REGION);
            if let Some(entry) = eval_only_eu.blocks.get_mut(&BlockId(0)) {
                let mut s = seeds.clone();
                s.append(&mut entry.instructions);
                entry.instructions = s;
            }
            let eval_apply_seeds = build_ff_region_copies_skipping(
                &targets,
                STABLE_REGION,
                WORKING_REGION,
                &dynamic_write_vars,
            );
            if let Some(entry) = eval_apply_eu.blocks.get_mut(&BlockId(0)) {
                let mut s = eval_apply_seeds;
                s.append(&mut entry.instructions);
                entry.instructions = s;
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
        let ff_site_count = self.ff_parser.runtime_event_sites().len() as u32;
        for observer in &mut self.comb_observers {
            observer.site_id += ff_site_count;
            observer.activation_group += ff_site_count;
        }
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, 0..arena_end, ff_site_count)?;
        let mut runtime_event_sites = self.ff_parser.runtime_event_sites().clone();
        runtime_event_sites.extend(self.comb_runtime_event_sites);
        let mut variable_widths = HashMap::default();
        for (id, variable) in &self.module.variables {
            variable_widths.insert(*id, resolve_total_width(self.module, variable)?);
        }
        super::verify_slt_roots(
            &self.arena,
            &self.comb_blocks,
            &self.comb_observers,
            &variable_widths,
            "after module symbolic lowering",
        )?;
        Ok(SimModule {
            variables: self.module.variables.clone(),
            name: self.module.name,
            glue_blocks: self.glue_blocks,
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            comb_blocks: self.comb_blocks,
            comb_observers: self.comb_observers,
            runtime_errors: self.ff_parser.runtime_errors().clone(),
            runtime_event_sites,
            initial_memory_values: self.initial_memory_values,
            comb_boundaries,
            arena: self.arena,
            store: self.store,
            reset_clock_map: self.reset_clock_map,
        })
    }
}

fn build_ff_region_copies(
    targets: &[VarAtomBase<RegionedVarAddr>],
    src_region: u32,
    dst_region: u32,
) -> Vec<SIRInstruction<RegionedVarAddr>> {
    build_ff_region_copies_skipping(targets, src_region, dst_region, &HashSet::default())
}

fn build_ff_region_copies_skipping(
    targets: &[VarAtomBase<RegionedVarAddr>],
    src_region: u32,
    dst_region: u32,
    skip_vars: &HashSet<VarId>,
) -> Vec<SIRInstruction<RegionedVarAddr>> {
    let mut ranges_by_var: HashMap<VarId, Vec<BitAccess>> = HashMap::default();
    let mut var_order = Vec::new();
    let mut seen_vars = HashSet::default();
    for target in targets {
        if skip_vars.contains(&target.id.var_id) {
            continue;
        }
        if seen_vars.insert(target.id.var_id) {
            var_order.push(target.id.var_id);
        }
        ranges_by_var
            .entry(target.id.var_id)
            .or_default()
            .push(target.access);
    }

    let mut copies = Vec::new();
    for var_id in var_order {
        let Some(mut ranges) = ranges_by_var.remove(&var_id) else {
            continue;
        };
        ranges.sort_by_key(|range| (range.lsb, range.msb));

        let mut current: Option<BitAccess> = None;
        for range in ranges {
            match current {
                Some(mut cur) if range.lsb <= cur.msb.saturating_add(1) => {
                    cur.msb = cur.msb.max(range.msb);
                    current = Some(cur);
                }
                Some(cur) => {
                    push_ff_region_copy(&mut copies, var_id, cur, src_region, dst_region);
                    current = Some(range);
                }
                None => current = Some(range),
            }
        }
        if let Some(cur) = current {
            push_ff_region_copy(&mut copies, var_id, cur, src_region, dst_region);
        }
    }

    copies
}

fn rewrite_working_accesses_to_stable(
    eu: &mut ExecutionUnit<RegionedVarAddr>,
    dynamic_write_vars: &HashSet<VarId>,
) {
    if dynamic_write_vars.is_empty() {
        return;
    }
    for block in eu.blocks.values_mut() {
        for inst in &mut block.instructions {
            rewrite_inst_working_access_to_stable(inst, dynamic_write_vars);
        }
    }
}

fn rewrite_inst_working_access_to_stable(
    inst: &mut SIRInstruction<RegionedVarAddr>,
    dynamic_write_vars: &HashSet<VarId>,
) {
    let rewrite_addr = |addr: &mut RegionedVarAddr| {
        if addr.region == WORKING_REGION && dynamic_write_vars.contains(&addr.var_id) {
            addr.region = STABLE_REGION;
        }
    };
    match inst {
        SIRInstruction::Load(_, addr, _, _) | SIRInstruction::Store(addr, _, _, _, _, _) => {
            rewrite_addr(addr);
        }
        SIRInstruction::Commit(src, dst, _, _, _) => {
            rewrite_addr(src);
            rewrite_addr(dst);
        }
        SIRInstruction::Imm(..)
        | SIRInstruction::Binary(..)
        | SIRInstruction::Unary(..)
        | SIRInstruction::Concat(..)
        | SIRInstruction::Slice(..)
        | SIRInstruction::Mux(..)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => {}
    }
}

fn push_ff_region_copy(
    copies: &mut Vec<SIRInstruction<RegionedVarAddr>>,
    var_id: VarId,
    range: BitAccess,
    src_region: u32,
    dst_region: u32,
) {
    copies.push(SIRInstruction::Commit(
        RegionedVarAddr {
            region: src_region,
            var_id,
        },
        RegionedVarAddr {
            region: dst_region,
            var_id,
        },
        SIROffset::Static(range.lsb),
        range.msb - range.lsb + 1,
        Vec::new(),
    ));
}

fn remap_for_effect_site_ids<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    range: std::ops::Range<usize>,
    offset: u32,
) -> Result<(), ParserError> {
    if offset == 0 {
        return Ok(());
    }
    arena
        .remap_for_fold_effect_sites(range, |site_id, fatal_error_code| {
            site_id
                .checked_add(offset)
                .map(|site_id| Some((site_id, fatal_error_code)))
                .ok_or(crate::logic_tree::SLTNodeArenaEditError::SiteIdOverflow { site_id, offset })
        })
        .map_err(|error| {
            ParserError::illegal_context("ForFold runtime-event remap", error.to_string(), None)
        })
}

fn collect_glue_sources(
    expr: crate::logic_tree::NodeId,
    arena: &SLTNodeArena<GlueAddr>,
) -> HashSet<VarAtomBase<GlueAddr>> {
    let mut set = HashSet::default();
    collect_glue_sources_with_window(expr, None, arena, &mut set);
    set
}

fn readmem_timing_enabled() -> bool {
    std::env::var_os("CELOX_READMEM_TIMING").is_some()
        || std::env::var_os("CELOX_PHASE_TIMING").is_some()
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
            effects,
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
            for effect in effects {
                if let Some(guard) = effect.guard {
                    collect_glue_sources_with_window(guard, None, arena, set);
                }
                for arg in &effect.args {
                    collect_glue_sources_with_window(*arg, None, arena, set);
                }
            }
            collect_glue_sources_with_window(*continue_cond, None, arena, set);
            set.retain(|atom| atom.id != *loop_var);
        }
        SLTNode::Constant(_, _, _, _) => {}
    }
}

struct ParsedMemoryWrites {
    runs: Vec<InitialMemoryWriteRun>,
    words: usize,
}

fn parse_memory_write_runs(
    content: &str,
    radix: u32,
    width: usize,
    start_addr: usize,
    depth: usize,
    location: &veryl_parser::token_range::TokenRange,
) -> Result<ParsedMemoryWrites, ParserError> {
    let mut runs: Vec<InitialMemoryWriteRun> = Vec::new();
    let mut addr = 0usize;
    let mut words = 0usize;
    for word_token in memory_tokens(content) {
        if let Some(address) = word_token.strip_prefix('@') {
            addr = usize::from_str_radix(address, 16).map_err(|err| {
                ParserError::unsupported(
                    111,
                    LoweringPhase::SimulatorParser,
                    "$readmemh address",
                    format!("invalid address directive {word_token}: {err}"),
                    None,
                )
            })?;
            continue;
        }
        let (value, mask) = parse_memory_word(&word_token, radix, width)?;
        let Some(dst_addr) = start_addr.checked_add(addr) else {
            return Err(ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh address",
                "address exceeds destination depth",
                Some(location),
            ));
        };
        if dst_addr >= depth {
            return Err(ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh address",
                format!("address {dst_addr} exceeds destination depth {depth}"),
                Some(location),
            ));
        }

        let bit_offset = dst_addr * width;
        let value_bytes = biguint_to_fixed_le_bytes(&value, width);
        let mask_bytes = biguint_to_fixed_le_bytes(&mask, width);

        if let Some(last) = runs.last_mut()
            && last.bit_offset + last.bit_width == bit_offset
            && last.bit_offset % 8 == 0
            && last.bit_width % 8 == 0
            && width % 8 == 0
        {
            last.bit_width += width;
            last.value_bytes.extend(value_bytes);
            last.mask_bytes.extend(mask_bytes);
        } else {
            runs.push(InitialMemoryWriteRun {
                bit_offset,
                bit_width: width,
                value_bytes,
                mask_bytes,
            });
        }

        words += 1;
        addr = addr.checked_add(1).ok_or_else(|| {
            ParserError::unsupported(
                111,
                LoweringPhase::SimulatorParser,
                "$readmemh address",
                "address exceeds destination depth",
                Some(location),
            )
        })?;
    }
    Ok(ParsedMemoryWrites { runs, words })
}

fn biguint_to_fixed_le_bytes(value: &BigUint, width: usize) -> Vec<u8> {
    let byte_len = width.div_ceil(8);
    let mut out = vec![0; byte_len];
    let src = value.to_bytes_le();
    let copy_len = src.len().min(byte_len);
    out[..copy_len].copy_from_slice(&src[..copy_len]);
    if width % 8 != 0 && !out.is_empty() {
        let keep = (1u8 << (width % 8)) - 1;
        *out.last_mut().unwrap() &= keep;
    }
    out
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

fn resolve_readmem_path_with_fallback(
    filename: &str,
    source_path: Option<&std::path::Path>,
    cwd: Option<&std::path::Path>,
) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(filename);
    if path.is_absolute() {
        return path;
    }

    let source_relative = source_path
        .and_then(std::path::Path::parent)
        .map(|parent| parent.join(&path))
        .unwrap_or_else(|| path.clone());
    if source_relative.exists() {
        return source_relative;
    }

    if let Some(cwd) = cwd {
        let cwd_relative = cwd.join(&path);
        if cwd_relative.exists() {
            return cwd_relative;
        }
    }

    source_relative
}

#[cfg(test)]
mod tests {
    use super::resolve_readmem_path_with_fallback;

    #[test]
    fn readmem_path_falls_back_to_project_root() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("tb");
        let data_dir = tmp.path().join("test/hex");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("boot.hex"), "00\n").unwrap();

        let resolved = resolve_readmem_path_with_fallback(
            "test/hex/boot.hex",
            Some(&source_dir.join("testbench.veryl")),
            Some(tmp.path()),
        );

        assert_eq!(resolved, data_dir.join("boot.hex"));
    }

    #[test]
    fn readmem_path_prefers_source_relative_file() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("tb");
        let source_data_dir = source_dir.join("test/hex");
        let root_data_dir = tmp.path().join("test/hex");
        std::fs::create_dir_all(&source_data_dir).unwrap();
        std::fs::create_dir_all(&root_data_dir).unwrap();
        std::fs::write(source_data_dir.join("boot.hex"), "11\n").unwrap();
        std::fs::write(root_data_dir.join("boot.hex"), "00\n").unwrap();

        let resolved = resolve_readmem_path_with_fallback(
            "test/hex/boot.hex",
            Some(&source_dir.join("testbench.veryl")),
            Some(tmp.path()),
        );

        assert_eq!(resolved, source_data_dir.join("boot.hex"));
    }
}
