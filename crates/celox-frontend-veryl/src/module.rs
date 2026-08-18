use std::collections::BTreeSet;
use std::time::Instant;

use crate::{
    BuildConfig, GlueAddr, GlueBlock, HashMap, HashSet, LoweringPhase, ModuleInitialMemoryValue,
    ParserError, RegionedVarAddr, SimModule,
    bitaccess::{
        PartSelectGeometry, SelectGeometry, eval_var_select_with_geometry, is_static_access,
        select_geometry,
    },
    bitslicer::BitSlicer,
    ff::FfParser,
    logic_tree::{
        CombEffectCollector, SymbolicStore, apply_assignment_destination, coerce_node_width,
        collect_and_advance_expression, collect_expression_effects, collect_written_expression,
        combine_parts_with_default, eval_assignment_expression_effectful, eval_expression,
        expression_contains_runtime_effect, get_width, parse_comb_with_loop_recovery,
        subtract_written_sensitivity,
    },
    loop_provenance::{LoopProvenance, LoopRecoveryCandidate},
    registry::get_port_type,
    resolve_total_width,
};
use celox_design::{
    BinaryOp, BitAccess, InitialStateData as InitialMemoryData,
    InitialStateWriteRun as InitialMemoryWriteRun, ModuleId, RuntimeEventSite,
    SPARSE_WORKING_REGION, STABLE_REGION, TriggerSet, UnaryOp, VarAtomBase, WORKING_REGION,
};
use celox_sir::{BlockId, ExecutionUnit, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator};
use celox_slt::{
    CombObserver, FfAccessSummary, LogicPath, LogicPathTarget, NodeId, RangeStore,
    SLTForFoldGroupState, SLTLoopBound, SLTNode, SLTNodeArena, SLTNodeArenaEditError, SLTNodeFacts,
    SLTNodeFactsError,
};
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, Component, Declaration, Expression, Factor,
    InstDeclaration, Module, Statement, SystemFunctionInput, SystemFunctionKind, VarId,
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
    loop_candidates: Vec<LoopRecoveryCandidate>,
}

fn build_dynamic_output_glue(
    module: &Module,
    geometry: &SelectGeometry,
    parent_store: &mut SymbolicStore<VarId>,
    parent_arena: &mut SLTNodeArena<VarId>,
    glue_arena: &mut SLTNodeArena<GlueAddr>,
    child_port_id: VarId,
    dst: &AssignDestination,
    rhs: NodeId,
    rhs_signed: bool,
    preview_rhs: NodeId,
    preview_rhs_sources: HashSet<VarAtomBase<VarId>>,
    preview_rhs_is_2state: bool,
) -> Result<
    (
        NodeId,
        BitAccess,
        HashSet<VarAtomBase<GlueAddr>>,
        HashSet<VarAtomBase<GlueAddr>>,
        HashSet<VarAtomBase<GlueAddr>>,
    ),
    ParserError,
> {
    let mut offset = glue_arena.alloc(SLTNode::Constant(
        BigUint::from(0u8),
        BigUint::from(0u8),
        64,
        false,
    ))?;
    let mut parent_offset = parent_arena.alloc(SLTNode::Constant(
        BigUint::from(0u8),
        BigUint::from(0u8),
        64,
        false,
    ))?;

    let mut sources = collect_glue_sources(rhs, glue_arena);
    let mut address_sources = HashSet::default();
    let mut preview_sources = preview_rhs_sources;

    let dim_limit = geometry.dimension_count;

    for (dimension, index_expr) in dst
        .index
        .0
        .iter()
        .chain(&dst.select.0)
        .take(dim_limit)
        .enumerate()
    {
        let ((index, index_sources), _) = crate::logic_tree::eval_expression_effectful(
            module,
            parent_store,
            index_expr,
            parent_arena,
            None,
        )?;
        preview_sources.extend(index_sources);
        let mut cache = HashMap::default();
        let mapped = parent_arena.get(index).map_addr(
            index,
            parent_arena,
            glue_arena,
            &mut cache,
            &|id| {
                if *id == child_port_id {
                    GlueAddr::Child(*id)
                } else {
                    GlueAddr::Parent(*id)
                }
            },
        )?;
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
        ))?;
        let term = glue_arena.alloc(SLTNode::Binary(mapped, BinaryOp::Mul, stride))?;
        offset = glue_arena.alloc(SLTNode::Binary(offset, BinaryOp::Add, term))?;
        let parent_stride = parent_arena.alloc(SLTNode::Constant(
            BigUint::from(geometry.strides[dimension]),
            BigUint::from(0u8),
            64,
            false,
        ))?;
        let parent_term =
            parent_arena.alloc(SLTNode::Binary(index, BinaryOp::Mul, parent_stride))?;
        parent_offset =
            parent_arena.alloc(SLTNode::Binary(parent_offset, BinaryOp::Add, parent_term))?;
    }

    if let Some(part) = geometry.part {
        let anchor_expr = dst.select.0.last().ok_or_else(|| {
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
        let (part_offset, parent_part_offset) = match part {
            PartSelectGeometry::Colon { lsb, .. } => {
                let bit_offset = lsb.checked_mul(weight).ok_or_else(|| {
                    ParserError::illegal_context(
                        "dynamic output port destination",
                        "colon-select offset overflows usize",
                        Some(&dst.token),
                    )
                })?;
                (
                    glue_arena.alloc(SLTNode::Constant(
                        BigUint::from(bit_offset),
                        BigUint::from(0u8),
                        64,
                        false,
                    ))?,
                    parent_arena.alloc(SLTNode::Constant(
                        BigUint::from(bit_offset),
                        BigUint::from(0u8),
                        64,
                        false,
                    ))?,
                )
            }
            PartSelectGeometry::PlusColon { .. }
            | PartSelectGeometry::MinusColon { .. }
            | PartSelectGeometry::Step { .. } => {
                let ((anchor, anchor_sources), _) = crate::logic_tree::eval_expression_effectful(
                    module,
                    parent_store,
                    anchor_expr,
                    parent_arena,
                    None,
                )?;
                preview_sources.extend(anchor_sources);
                let parent_anchor = anchor;
                let mut cache = HashMap::default();
                let anchor = parent_arena.get(anchor).map_addr(
                    anchor,
                    parent_arena,
                    glue_arena,
                    &mut cache,
                    &|id| {
                        if *id == child_port_id {
                            GlueAddr::Child(*id)
                        } else {
                            GlueAddr::Parent(*id)
                        }
                    },
                )?;
                let mapped_sources = collect_glue_sources(anchor, glue_arena);
                sources.extend(mapped_sources.iter().copied());
                address_sources.extend(mapped_sources);

                let (element_offset, parent_element_offset) = match part {
                    PartSelectGeometry::PlusColon { .. } => (anchor, parent_anchor),
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
                        ))?;
                        let parent_decrement = parent_arena.alloc(SLTNode::Constant(
                            BigUint::from(elements - 1),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        (
                            glue_arena.alloc(SLTNode::Binary(anchor, BinaryOp::Sub, decrement))?,
                            parent_arena.alloc(SLTNode::Binary(
                                parent_anchor,
                                BinaryOp::Sub,
                                parent_decrement,
                            ))?,
                        )
                    }
                    PartSelectGeometry::Step { elements } => {
                        let element_count = elements;
                        let elements = glue_arena.alloc(SLTNode::Constant(
                            BigUint::from(element_count),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        let parent_elements = parent_arena.alloc(SLTNode::Constant(
                            BigUint::from(element_count),
                            BigUint::from(0u8),
                            64,
                            false,
                        ))?;
                        (
                            glue_arena.alloc(SLTNode::Binary(anchor, BinaryOp::Mul, elements))?,
                            parent_arena.alloc(SLTNode::Binary(
                                parent_anchor,
                                BinaryOp::Mul,
                                parent_elements,
                            ))?,
                        )
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
                    (element_offset, parent_element_offset)
                } else {
                    let weight_value = weight;
                    let weight = glue_arena.alloc(SLTNode::Constant(
                        BigUint::from(weight_value),
                        BigUint::from(0u8),
                        64,
                        false,
                    ))?;
                    let parent_weight = parent_arena.alloc(SLTNode::Constant(
                        BigUint::from(weight_value),
                        BigUint::from(0u8),
                        64,
                        false,
                    ))?;
                    (
                        glue_arena.alloc(SLTNode::Binary(element_offset, BinaryOp::Mul, weight))?,
                        parent_arena.alloc(SLTNode::Binary(
                            parent_element_offset,
                            BinaryOp::Mul,
                            parent_weight,
                        ))?,
                    )
                }
            }
        };
        offset = glue_arena.alloc(SLTNode::Binary(offset, BinaryOp::Add, part_offset))?;
        parent_offset = parent_arena.alloc(SLTNode::Binary(
            parent_offset,
            BinaryOp::Add,
            parent_part_offset,
        ))?;
    }

    let access_width = geometry.selected_width;
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
    // Keep the instance preview sparse. Untracked ranges represent the
    // unmodified parent input and are materialized only for the destination
    // touched by this dynamic connection.
    let range_store = parent_store
        .entry(dst.id)
        .or_insert_with(|| RangeStore::new(None, variable_width));
    let parts = range_store.get_parts_ref(full_access).map_err(|error| {
        ParserError::illegal_context(
            "dynamic output port destination",
            error.to_string(),
            Some(&dst.token),
        )
    })?;
    let (old_value, old_sources) = combine_parts_with_default(dst.id, 0, parts, parent_arena)
        .map_err(|error| ParserError::SltVerify {
            phase: "dynamic output port destination",
            error,
        })?;
    preview_sources.extend(old_sources.into_iter().filter(|source| source.id != dst.id));
    let preview_old_value = old_value;
    let mut cache = HashMap::default();
    let old_value = parent_arena.get(old_value).map_addr(
        old_value,
        parent_arena,
        glue_arena,
        &mut cache,
        &|id| {
            if *id == child_port_id {
                GlueAddr::Child(*id)
            } else {
                GlueAddr::Parent(*id)
            }
        },
    )?;
    sources.extend(collect_glue_sources(old_value, glue_arena));

    let low_mask = (BigUint::from(1u8) << access_width) - BigUint::from(1u8);
    let low_mask = glue_arena.alloc(SLTNode::Constant(
        low_mask,
        BigUint::from(0u8),
        variable_width,
        false,
    ))?;
    let shifted_mask = glue_arena.alloc(SLTNode::Binary(low_mask, BinaryOp::Shl, offset))?;
    let keep_mask = glue_arena.alloc(SLTNode::Unary(UnaryOp::BitNot, shifted_mask))?;

    // First apply assignment coercion to the selected destination width.  Only
    // after truncation/sign-extension is complete may the value be embedded in
    // the full variable; otherwise high RHS bits can corrupt adjacent fields.
    let rhs = coerce_node_width(glue_arena, rhs, Some(access_width), rhs_signed)?;
    let rhs = if variable.r#type.is_2state() && !preview_rhs_is_2state {
        glue_arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, rhs))?
    } else {
        rhs
    };
    let rhs = if access_width < variable_width {
        let padding_width = variable_width - access_width;
        let padding = glue_arena.alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            padding_width,
            false,
        ))?;
        glue_arena.alloc(SLTNode::Concat(vec![
            (padding, padding_width),
            (rhs, access_width),
        ]))?
    } else {
        rhs
    };
    let shifted_rhs = glue_arena.alloc(SLTNode::Binary(rhs, BinaryOp::Shl, offset))?;
    let shifted_rhs =
        glue_arena.alloc(SLTNode::Binary(shifted_rhs, BinaryOp::And, shifted_mask))?;
    let kept_value = glue_arena.alloc(SLTNode::Binary(old_value, BinaryOp::And, keep_mask))?;
    let updated_value = glue_arena.alloc(SLTNode::Binary(kept_value, BinaryOp::Or, shifted_rhs))?;

    let parent_low_mask = (BigUint::from(1u8) << access_width) - BigUint::from(1u8);
    let parent_low_mask = parent_arena.alloc(SLTNode::Constant(
        parent_low_mask,
        BigUint::from(0u8),
        variable_width,
        false,
    ))?;
    let parent_shifted_mask = parent_arena.alloc(SLTNode::Binary(
        parent_low_mask,
        BinaryOp::Shl,
        parent_offset,
    ))?;
    let parent_keep_mask =
        parent_arena.alloc(SLTNode::Unary(UnaryOp::BitNot, parent_shifted_mask))?;
    let preview_rhs = coerce_node_width(parent_arena, preview_rhs, Some(access_width), rhs_signed)?;
    let preview_rhs = if variable.r#type.is_2state() && !preview_rhs_is_2state {
        parent_arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, preview_rhs))?
    } else {
        preview_rhs
    };
    let preview_rhs = if access_width < variable_width {
        let padding_width = variable_width - access_width;
        let padding = parent_arena.alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            padding_width,
            false,
        ))?;
        parent_arena.alloc(SLTNode::Concat(vec![
            (padding, padding_width),
            (preview_rhs, access_width),
        ]))?
    } else {
        preview_rhs
    };
    let parent_shifted_rhs =
        parent_arena.alloc(SLTNode::Binary(preview_rhs, BinaryOp::Shl, parent_offset))?;
    let parent_shifted_rhs = parent_arena.alloc(SLTNode::Binary(
        parent_shifted_rhs,
        BinaryOp::And,
        parent_shifted_mask,
    ))?;
    let parent_kept_value = parent_arena.alloc(SLTNode::Binary(
        preview_old_value,
        BinaryOp::And,
        parent_keep_mask,
    ))?;
    let parent_updated_value = parent_arena.alloc(SLTNode::Binary(
        parent_kept_value,
        BinaryOp::Or,
        parent_shifted_rhs,
    ))?;

    let prefix = eval_var_select_with_geometry(&dst.index, &dst.select, geometry)?;
    let result = if prefix == full_access {
        updated_value
    } else {
        glue_arena.alloc(SLTNode::Slice {
            expr: updated_value,
            access: prefix,
        })?
    };
    let preview_result = if prefix == full_access {
        parent_updated_value
    } else {
        parent_arena.alloc(SLTNode::Slice {
            expr: parent_updated_value,
            access: prefix,
        })?
    };
    range_store
        .update(prefix, Some((preview_result, preview_sources)))
        .map_err(|error| {
            ParserError::illegal_context(
                "dynamic output port destination preview",
                error.to_string(),
                Some(&dst.token),
            )
        })?;
    let previous_sources = std::iter::once(VarAtomBase::new(
        GlueAddr::Parent(dst.id),
        prefix.lsb,
        prefix.msb,
    ))
    .collect();
    Ok((result, prefix, sources, previous_sources, address_sources))
}

fn collect_parent_address_expression_sources(
    module: &Module,
    store: &SymbolicStore<VarId>,
    target: VarId,
    expression: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    out: &mut HashMap<VarId, HashSet<VarAtomBase<VarId>>>,
) -> Result<(), ParserError> {
    let ((_, sources), _) = eval_expression(module, store, expression, arena, None)?;
    out.entry(target).or_default().extend(sources);
    collect_parent_output_address_sources(module, store, expression, arena, out)
}

fn collect_parent_output_address_sources(
    module: &Module,
    store: &SymbolicStore<VarId>,
    expression: &Expression,
    arena: &mut SLTNodeArena<VarId>,
    out: &mut HashMap<VarId, HashSet<VarAtomBase<VarId>>>,
) -> Result<(), ParserError> {
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::FunctionCall(call) => {
                for input in call.inputs.values() {
                    collect_parent_output_address_sources(module, store, input, arena, out)?;
                }
                for destinations in call.outputs.values() {
                    for destination in destinations {
                        for address in destination
                            .index
                            .0
                            .iter()
                            .chain(destination.select.0.iter())
                        {
                            collect_parent_address_expression_sources(
                                module,
                                store,
                                destination.id,
                                address,
                                arena,
                                out,
                            )?;
                        }
                    }
                }
                Ok(())
            }
            Factor::Variable(_, index, select, _) => {
                for address in index.0.iter().chain(select.0.iter()) {
                    collect_parent_output_address_sources(module, store, address, arena, out)?;
                }
                Ok(())
            }
            Factor::HierVariable(reference) => Err(ParserError::illegal_context(
                "hierarchical variable reference",
                format!(
                    "`{}` is only valid in a native testbench block",
                    reference.var_path
                ),
                Some(&reference.comptime.token),
            )),
            Factor::SystemFunctionCall(call) => {
                let mut collect = |input: &SystemFunctionInput| {
                    collect_parent_output_address_sources(module, store, &input.0, arena, out)
                };
                match &call.kind {
                    SystemFunctionKind::Clog2(input)
                    | SystemFunctionKind::Onehot(input)
                    | SystemFunctionKind::Signed(input)
                    | SystemFunctionKind::Unsigned(input) => collect(input),
                    SystemFunctionKind::Display(inputs) | SystemFunctionKind::Write(inputs) => {
                        for input in inputs {
                            collect(input)?;
                        }
                        Ok(())
                    }
                    SystemFunctionKind::Assert { cond, args, .. } => {
                        collect(cond)?;
                        for input in args {
                            collect(input)?;
                        }
                        Ok(())
                    }
                    SystemFunctionKind::Bits(_)
                    | SystemFunctionKind::Size(_)
                    | SystemFunctionKind::Readmemh(_, _)
                    | SystemFunctionKind::Finish => Ok(()),
                }
            }
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
        },
        Expression::Unary(_, inner, _) => {
            collect_parent_output_address_sources(module, store, inner, arena, out)
        }
        Expression::Binary(lhs, veryl_analyzer::ir::Op::Pow, _, _) => {
            collect_parent_output_address_sources(module, store, lhs, arena, out)
        }
        Expression::Binary(lhs, _, rhs, _) => {
            collect_parent_output_address_sources(module, store, lhs, arena, out)?;
            collect_parent_output_address_sources(module, store, rhs, arena, out)
        }
        Expression::Ternary(cond, then_expression, else_expression, _) => {
            collect_parent_output_address_sources(module, store, cond, arena, out)?;
            collect_parent_output_address_sources(module, store, then_expression, arena, out)?;
            collect_parent_output_address_sources(module, store, else_expression, arena, out)
        }
        Expression::Concatenation(parts, _) => {
            for (part, _) in parts {
                collect_parent_output_address_sources(module, store, part, arena, out)?;
            }
            Ok(())
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(expression, _) => {
                        collect_parent_output_address_sources(
                            module, store, expression, arena, out,
                        )?;
                    }
                    ArrayLiteralItem::Defaul(expression) => {
                        collect_parent_output_address_sources(
                            module, store, expression, arena, out,
                        )?;
                    }
                }
            }
            Ok(())
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, expression) in fields {
                collect_parent_output_address_sources(module, store, expression, arena, out)?;
            }
            Ok(())
        }
    }
}

fn build_parent_effect_glue(
    initial_store: &SymbolicStore<VarId>,
    store: &SymbolicStore<VarId>,
    written_accesses: &HashMap<VarId, Vec<BitAccess>>,
    output_address_sources: &HashMap<VarId, HashSet<VarAtomBase<VarId>>>,
    parent_arena: &SLTNodeArena<VarId>,
    glue_arena: &mut SLTNodeArena<GlueAddr>,
    child_port_id: Option<VarId>,
) -> Result<Vec<(Vec<VarId>, LogicPath<GlueAddr>)>, ParserError> {
    let mut paths = Vec::new();

    for (id, accesses) in written_accesses {
        let Some(range_store) = store.get(id) else {
            continue;
        };
        let mut emitted = BTreeSet::new();

        for (&lsb, (value, width, origin)) in &range_store.ranges {
            let Some((expr, _)) = value else {
                continue;
            };
            let msb = lsb
                .checked_add(*width)
                .and_then(|end| end.checked_sub(1))
                .ok_or_else(|| {
                    ParserError::illegal_context(
                        "instance input function output",
                        "symbolic output range overflows usize",
                        None,
                    )
                })?;
            let stored_access = BitAccess::new(lsb, msb);

            for written_access in accesses {
                let target_lsb = stored_access.lsb.max(written_access.lsb);
                let target_msb = stored_access.msb.min(written_access.msb);
                if target_lsb > target_msb || !emitted.insert((target_lsb, target_msb)) {
                    continue;
                }

                let target_access = BitAccess::new(target_lsb, target_msb);
                if let Some(initial_range_store) = initial_store.get(id) {
                    let final_parts =
                        range_store.get_parts_ref(target_access).map_err(|error| {
                            ParserError::illegal_context(
                                "instance input function output",
                                error.to_string(),
                                None,
                            )
                        })?;
                    let initial_parts =
                        initial_range_store
                            .get_parts_ref(target_access)
                            .map_err(|error| {
                                ParserError::illegal_context(
                                    "instance input function output",
                                    error.to_string(),
                                    None,
                                )
                            })?;
                    if final_parts == initial_parts {
                        continue;
                    }
                }

                let target_width = target_msb - target_lsb + 1;
                let relative_lsb = target_lsb.checked_sub(*origin).ok_or_else(|| {
                    ParserError::illegal_context(
                        "instance input function output",
                        "symbolic output range precedes its source origin",
                        None,
                    )
                })?;
                let relative_msb = relative_lsb + target_width - 1;
                let mut cache = HashMap::default();
                let mapped = parent_arena.get(*expr).map_addr(
                    *expr,
                    parent_arena,
                    glue_arena,
                    &mut cache,
                    &|var_id| {
                        if child_port_id == Some(*var_id) {
                            GlueAddr::Child(*var_id)
                        } else {
                            GlueAddr::Parent(*var_id)
                        }
                    },
                )?;
                let mapped = if relative_lsb == 0 && target_width == get_width(mapped, glue_arena) {
                    mapped
                } else {
                    glue_arena.alloc(SLTNode::Slice {
                        expr: mapped,
                        access: BitAccess::new(relative_lsb, relative_msb),
                    })?
                };
                let sources = collect_glue_sources(mapped, glue_arena);
                let target =
                    VarAtomBase::new(GlueAddr::Parent(*id), target_access.lsb, target_access.msb);
                let previous_sources = sources
                    .iter()
                    .copied()
                    .filter(|source| {
                        source.id == target.id && source.access.overlaps(&target.access)
                    })
                    .collect();
                let address_sources = output_address_sources
                    .get(id)
                    .into_iter()
                    .flatten()
                    .map(|source| {
                        VarAtomBase::new(
                            if child_port_id == Some(source.id) {
                                GlueAddr::Child(source.id)
                            } else {
                                GlueAddr::Parent(source.id)
                            },
                            source.access.lsb,
                            source.access.msb,
                        )
                    })
                    .filter(|address| {
                        sources.iter().any(|source| {
                            source.id == address.id && source.access.overlaps(&address.access)
                        })
                    })
                    .collect();

                paths.push((
                    vec![*id],
                    LogicPath {
                        target: LogicPathTarget::Var(target),
                        expr: mapped,
                        sources,
                        previous_sources,
                        address_sources,
                        local_inputs: Vec::new(),
                        order_before: HashSet::default(),
                        comb_capture_enable_sites: Vec::new(),
                        comb_capture_enable_always: false,
                        pre_lower_nodes: Vec::new(),
                    },
                ));
            }
        }
    }

    Ok(paths)
}

fn subtract_composed_accesses(
    written_accesses: &mut HashMap<VarId, Vec<BitAccess>>,
    composed_accesses: &[(VarId, BitAccess)],
) {
    for &(id, composed) in composed_accesses {
        let Some(accesses) = written_accesses.get_mut(&id) else {
            continue;
        };
        let mut remaining = Vec::new();
        for access in accesses.drain(..) {
            if !access.overlaps(&composed) {
                remaining.push(access);
                continue;
            }
            if access.lsb < composed.lsb {
                remaining.push(BitAccess::new(access.lsb, composed.lsb - 1));
            }
            if access.msb > composed.msb {
                remaining.push(BitAccess::new(composed.msb + 1, access.msb));
            }
        }
        *accesses = remaining;
    }
    written_accesses.retain(|_, accesses| !accesses.is_empty());
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
        error: SLTNodeFactsError::new(invariant, node, message),
    };
    let verify_atom = |atom: &VarAtomBase<GlueAddr>,
                       role: &'static str,
                       node: NodeId|
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
                NodeId(node_index),
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
        let parser = Self::new(module, Vec::new(), config, inst_ids)?;
        parser.parse_inner()
    }

    pub fn parse_with_loop_provenance(
        module: &'a Module,
        loop_provenance: &LoopProvenance,
        config: &BuildConfig,
        inst_ids: &'a [ModuleId],
    ) -> Result<SimModule, ParserError> {
        let parser = Self::new(
            module,
            loop_provenance.candidates_for_module(module),
            config,
            inst_ids,
        )?;
        parser.parse_inner()
    }

    fn new(
        module: &'a Module,
        loop_candidates: Vec<LoopRecoveryCandidate>,
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
            loop_candidates,
        })
    }

    fn parse_comb_declaration(
        &mut self,
        decl: &veryl_analyzer::ir::CombDeclaration,
    ) -> Result<(), ParserError> {
        let arena_start = self.arena.len();
        let site_offset = self.comb_runtime_event_sites.len() as u32;
        let (paths, store, boundaries, mut observers, sites) = parse_comb_with_loop_recovery(
            self.module,
            decl,
            &mut self.arena,
            &self.loop_candidates,
            site_offset,
        )?;
        for observer in &mut observers {
            observer.site_id += site_offset;
            observer.activation_group = site_offset;
        }
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, arena_start..arena_end, site_offset)?;
        self.store.extend(store);
        self.comb_blocks.extend(paths);
        self.comb_observers.extend(observers);
        self.comb_runtime_event_sites.extend(sites);
        for (id, bounds) in boundaries {
            self.comb_boundaries.entry(id).or_default().extend(bounds);
        }
        Ok(())
    }

    fn attach_connection_effects(
        &mut self,
        arena_start: usize,
        mut effects: CombEffectCollector,
        written_accesses: &HashMap<VarId, Vec<BitAccess>>,
        process_sensitivity: HashSet<VarAtomBase<VarId>>,
        preserved_address_sources: HashSet<VarAtomBase<VarId>>,
    ) -> Result<Vec<u32>, ParserError> {
        if effects.observers.is_empty() && effects.sites.is_empty() {
            return Ok(Vec::new());
        }

        let written_atoms: Vec<_> = written_accesses
            .iter()
            .flat_map(|(&id, accesses)| {
                accesses
                    .iter()
                    .map(move |access| VarAtomBase::new(id, access.lsb, access.msb))
            })
            .collect();
        let mut process_sensitivity =
            subtract_written_sensitivity(process_sensitivity, &written_atoms);
        process_sensitivity.extend(preserved_address_sources);
        let process_sensitivity: Vec<_> = process_sensitivity.into_iter().collect();
        for observer in &mut effects.observers {
            observer.sensitivity = process_sensitivity.clone();
            observer.written_input_atoms = observer
                .observed_inputs
                .iter()
                .chain(observer.position_inputs.iter())
                .copied()
                .filter(|atom| {
                    written_atoms.iter().any(|written| {
                        written.id == atom.id && written.access.overlaps(&atom.access)
                    })
                })
                .collect();
            observer.written_inputs = observer
                .written_input_atoms
                .iter()
                .map(|atom| atom.id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
        }

        let site_offset = self.comb_runtime_event_sites.len();
        for observer in &mut effects.observers {
            observer.site_id += site_offset as u32;
            observer.activation_group = site_offset as u32;
        }
        let site_ids = effects
            .observers
            .iter()
            .map(|observer| observer.site_id)
            .collect();
        let arena_end = self.arena.len();
        remap_for_effect_site_ids(&mut self.arena, arena_start..arena_end, site_offset as u32)?;
        self.comb_observers.extend(effects.observers);
        self.comb_runtime_event_sites.extend(effects.sites);
        Ok(site_ids)
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
        let mut output_ports = Vec::new();
        let mut glue_arena = SLTNodeArena::<GlueAddr>::new();

        // Parent variables are implicit inputs until a connection expression
        // writes them, so instance parsing only needs sparse, touched entries.
        let parent_store = SymbolicStore::default();

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
            let mut written_accesses = HashMap::default();
            collect_written_expression(self.module, &input.expr, &mut written_accesses)?;
            let mut connection_store = parent_store.fork();
            let ((expr_node, expr_sources), _bounds) = eval_assignment_expression_effectful(
                self.module,
                &mut connection_store,
                &input.expr,
                &mut self.arena,
                width,
            )?;
            let mut output_address_sources = HashMap::default();
            collect_parent_output_address_sources(
                self.module,
                &parent_store,
                &input.expr,
                &mut self.arena,
                &mut output_address_sources,
            )?;

            if expression_contains_runtime_effect(self.module, &input.expr) {
                let arena_start = self.arena.len();
                let mut effects = CombEffectCollector::with_capture_namespace(
                    self.comb_runtime_event_sites.len() as u32,
                );
                let effect_store = SymbolicStore::default();
                collect_expression_effects(
                    self.module,
                    &effect_store,
                    &input.expr,
                    &mut self.arena,
                    &mut effects,
                )?;

                let mut process_sensitivity = std::mem::take(&mut effects.sensitivity);
                process_sensitivity.extend(expr_sources.iter().copied());
                for (&id, accesses) in &written_accesses {
                    let Some(range_store) = connection_store.get(&id) else {
                        continue;
                    };
                    for &access in accesses {
                        for (value, _) in range_store.get_parts_ref(access).map_err(|error| {
                            ParserError::illegal_context(
                                "instance input function output",
                                error.to_string(),
                                Some(&input.expr.token_range()),
                            )
                        })? {
                            if let Some((_, sources)) = value {
                                process_sensitivity.extend(sources);
                            }
                        }
                    }
                }
                let preserved_address_sources = output_address_sources
                    .values()
                    .flat_map(|sources| sources.iter().copied())
                    .collect();
                let _ = self.attach_connection_effects(
                    arena_start,
                    effects,
                    &written_accesses,
                    process_sensitivity,
                    preserved_address_sources,
                )?;
            }

            // Map Parent VarId to GlueAddr::Parent
            let mut cache = HashMap::default();
            let mapped_node = self.arena.get(expr_node).map_addr(
                expr_node,
                &self.arena,
                &mut glue_arena,
                &mut cache,
                &|id| GlueAddr::Parent(*id),
            )?;

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
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
            };

            let parent_vars: Vec<_> = expr_sources.iter().map(|s| s.id).collect();
            input_ports.push((parent_vars, path));
            output_ports.extend(build_parent_effect_glue(
                &parent_store,
                &connection_store,
                &written_accesses,
                &output_address_sources,
                &self.arena,
                &mut glue_arena,
                None,
            )?);
        }

        // 2. Outputs (Child -> Parent)
        for output in &decl.outputs {
            // The analyzer includes deliberately unconnected child outputs
            // with an empty destination list. They produce no parent glue;
            // width coverage applies only to connected destinations.
            if output.dst.is_empty() {
                continue;
            }
            let child_port_id = output.id;
            let output_port_start = output_ports.len();
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
            })?;
            // LHS: output.dst (AssignDestination).
            let mut current_offset = 0usize;
            let mut destination_arena = SLTNodeArena::<VarId>::new();
            let mut destination_store = SymbolicStore::default();
            let preview_rhs_node = destination_arena.alloc(SLTNode::Input {
                variable: child_port_id,
                signed: child_port.r#type.signed,
                index: vec![],
                access: BitAccess::new(0, width - 1),
            })?;
            let mut destination_written_accesses = HashMap::default();
            let mut destination_address_sources = HashMap::default();
            let mut composed_output_accesses = Vec::new();
            let output_effect_arena_start = self.arena.len();
            let mut output_effects = CombEffectCollector::with_capture_namespace(
                self.comb_runtime_event_sites.len() as u32,
            );
            let mut output_effect_store = SymbolicStore::default();
            // Iterate destinations from LSB (last in list for multi-dst assign usually? No wait)
            // `emit_multi_dst_assign` iterates `dsts.iter().rev()`.
            // So we strictly follow `emit_multi_dst_assign` logic.
            // "Current offset starts at 0" and "dst in dsts.iter().rev()".
            for dst in output.dst.iter().rev() {
                for address in dst.index.0.iter().chain(dst.select.0.iter()) {
                    let address_sources = collect_and_advance_expression(
                        self.module,
                        &mut output_effect_store,
                        address,
                        &mut self.arena,
                        &mut output_effects,
                    )?;
                    output_effects.sensitivity.extend(address_sources);
                    collect_written_expression(
                        self.module,
                        address,
                        &mut destination_written_accesses,
                    )?;
                    collect_parent_output_address_sources(
                        self.module,
                        &destination_store,
                        address,
                        &mut destination_arena,
                        &mut destination_address_sources,
                    )?;
                }
                let geometry = select_geometry(self.module, dst.id, &dst.index, &dst.select)?;
                let prefix_access =
                    eval_var_select_with_geometry(&dst.index, &dst.select, &geometry)?;
                let part_width = geometry.selected_width;

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
                    })?
                };
                let preview_rhs_part = if slice_access.lsb == 0
                    && slice_access.msb == get_width(preview_rhs_node, &destination_arena) - 1
                {
                    preview_rhs_node
                } else {
                    destination_arena.alloc(SLTNode::Slice {
                        expr: preview_rhs_node,
                        access: slice_access,
                    })?
                };
                let preview_rhs_sources: HashSet<_> = std::iter::once(VarAtomBase::new(
                    child_port_id,
                    slice_access.lsb,
                    slice_access.msb,
                ))
                .collect();

                let dynamic_access = !is_static_access(&dst.index, &dst.select);
                let (expr, access, sources, previous_sources, address_sources) = if !dynamic_access
                {
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
                        &geometry,
                        &mut destination_store,
                        &mut destination_arena,
                        &mut glue_arena,
                        child_port_id,
                        dst,
                        rhs_part,
                        output.dst.len() == 1
                            && child_module.variables[&child_port_id].r#type.signed,
                        preview_rhs_part,
                        preview_rhs_sources.clone(),
                        child_port.r#type.is_2state(),
                    )?
                };
                if dynamic_access {
                    // The dynamic child assignment expression was built from
                    // the advanced destination store, so it already contains
                    // preceding index-call writes within this access.
                    composed_output_accesses.push((dst.id, access));
                }

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
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr,
                };
                output_ports.push((vec![dst.id], path));

                if !dynamic_access {
                    let (next_store, _) = apply_assignment_destination(
                        self.module,
                        destination_store,
                        HashMap::default(),
                        dst,
                        preview_rhs_part,
                        preview_rhs_sources,
                        child_port.r#type.is_2state(),
                        &mut destination_arena,
                    )?;
                    destination_store = next_store;
                }

                // Runtime effects in a later destination execute after this
                // child slice has been written. Model that statement position
                // with a parent read: scheduling orders the observer after the
                // corresponding glue path, and capture then sees the actual
                // child-driven value without introducing a child VarId into
                // the parent module arena.
                let effect_preview = self.arena.alloc(SLTNode::Input {
                    variable: dst.id,
                    signed: self.module.variables[&dst.id].r#type.signed,
                    index: vec![],
                    access,
                })?;
                let effect_sources =
                    std::iter::once(VarAtomBase::new(dst.id, access.lsb, access.msb)).collect();
                let parent_width =
                    resolve_total_width(self.module, &self.module.variables[&dst.id])?;
                output_effect_store
                    .entry(dst.id)
                    .or_insert_with(|| RangeStore::new(None, parent_width))
                    .update(access, Some((effect_preview, effect_sources)))
                    .map_err(|error| {
                        ParserError::illegal_context(
                            "output connection effect preview",
                            error.to_string(),
                            Some(&dst.token),
                        )
                    })?;

                current_offset = slice_end;
            }
            let process_sensitivity = std::mem::take(&mut output_effects.sensitivity);
            let preserved_address_sources = destination_address_sources
                .values()
                .flat_map(|sources| sources.iter().copied())
                .collect();
            let output_effect_site_ids = self.attach_connection_effects(
                output_effect_arena_start,
                output_effects,
                &destination_written_accesses,
                process_sensitivity,
                preserved_address_sources,
            )?;
            for (_, path) in &mut output_ports[output_port_start..] {
                path.comb_capture_enable_sites
                    .extend(output_effect_site_ids.iter().copied());
                path.comb_capture_enable_always = !output_effect_site_ids.is_empty();
            }
            let mut remaining_written_accesses = destination_written_accesses.clone();
            subtract_composed_accesses(&mut remaining_written_accesses, &composed_output_accesses);
            output_ports.extend(build_parent_effect_glue(
                &parent_store,
                &destination_store,
                &remaining_written_accesses,
                &destination_address_sources,
                &destination_arena,
                &mut glue_arena,
                Some(child_port_id),
            )?);
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
            tracing::debug!(
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
            tracing::debug!(
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
            tracing::debug!(
                "[readmem-timing] parse file={} words={} runs={} elapsed={:?}",
                path.display(),
                writes.words,
                writes.runs.len(),
                start.elapsed()
            );
        }
        if let Some(start) = total_start {
            tracing::debug!(
                "[readmem-timing] done file={} elapsed={:?}",
                path.display(),
                start.elapsed()
            );
        }

        Ok(ModuleInitialMemoryValue {
            address: dst.id,
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
        let mut ff_access_summaries = HashMap::default();

        for (trigger_set, decls) in &ff_groups {
            // --- eval_only and eval_apply ---
            // Run parse_ff_group once. Clone the builder before sealing so that
            // eval_only and eval_apply are produced from independent builder states,
            // each with their own register namespace (no shared RegisterIds).
            let mut builder = SIRBuilder::new();
            let ff_group = self.ff_parser.parse_ff_group(decls, &mut builder)?;
            let targets = ff_group.targets;
            let sources = ff_group.sources;
            let dynamic_write_vars = ff_group.dynamic_write_vars;
            ff_access_summaries.insert(
                trigger_set.clone(),
                FfAccessSummary {
                    reads: sources,
                    writes: targets.clone(),
                    dynamic_writes: dynamic_write_vars
                        .iter()
                        .copied()
                        .map(|var_id| RegionedVarAddr {
                            region: WORKING_REGION,
                            var_id,
                        })
                        .collect(),
                },
            );
            let mut commits = build_ff_region_copies_skipping(
                &targets,
                WORKING_REGION,
                STABLE_REGION,
                &dynamic_write_vars,
            );
            commits.extend(build_sparse_ff_commits(&targets, &dynamic_write_vars));

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
            rewrite_dynamic_ff_stores_to_sparse(&mut eval_only_eu, &dynamic_write_vars);
            rewrite_dynamic_ff_stores_to_sparse(&mut eval_apply_eu, &dynamic_write_vars);

            // Build seeds (STABLE -> WORKING) and prepend to both eval_only and eval_apply.
            let seeds = build_ff_region_copies_skipping(
                &targets,
                STABLE_REGION,
                WORKING_REGION,
                &dynamic_write_vars,
            );
            if let Some(entry) = eval_only_eu.blocks.get_mut(&BlockId(0)) {
                let mut s = seeds.clone();
                s.append(&mut entry.instructions);
                entry.instructions = s;
            }
            if let Some(entry) = eval_apply_eu.blocks.get_mut(&BlockId(0)) {
                let mut s = seeds;
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
        let mut variable_signedness = HashMap::default();
        for (id, variable) in &self.module.variables {
            variable_widths.insert(*id, resolve_total_width(self.module, variable)?);
            variable_signedness.insert(*id, variable.r#type.signed);
        }
        celox_slt::verify_symbolic_roots(
            &self.arena,
            &self.comb_blocks,
            &self.comb_observers,
            &variable_widths,
            &variable_signedness,
        )
        .map_err(|error| ParserError::SltVerify {
            phase: "after module symbolic lowering",
            error,
        })?;
        Ok(SimModule {
            variables: self.module.variables.clone(),
            name: self.module.name,
            glue_blocks: self.glue_blocks,
            ff_access_summaries,
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

fn build_sparse_ff_commits(
    targets: &[VarAtomBase<RegionedVarAddr>],
    sparse_vars: &HashSet<VarId>,
) -> Vec<SIRInstruction<RegionedVarAddr>> {
    let mut widths = HashMap::<VarId, usize>::default();
    let mut order = Vec::new();
    for target in targets {
        if !sparse_vars.contains(&target.id.var_id) {
            continue;
        }
        if !widths.contains_key(&target.id.var_id) {
            order.push(target.id.var_id);
        }
        widths
            .entry(target.id.var_id)
            .and_modify(|width| *width = (*width).max(target.access.msb.saturating_add(1)))
            .or_insert_with(|| target.access.msb.saturating_add(1));
    }
    order
        .into_iter()
        .map(|var_id| {
            SIRInstruction::Commit(
                RegionedVarAddr {
                    region: SPARSE_WORKING_REGION,
                    var_id,
                },
                RegionedVarAddr {
                    region: STABLE_REGION,
                    var_id,
                },
                SIROffset::Static(0),
                widths[&var_id],
                Vec::new(),
            )
        })
        .collect()
}

fn rewrite_dynamic_ff_stores_to_sparse(
    eu: &mut ExecutionUnit<RegionedVarAddr>,
    dynamic_write_vars: &HashSet<VarId>,
) {
    if dynamic_write_vars.is_empty() {
        return;
    }
    for block in eu.blocks.values_mut() {
        for inst in &mut block.instructions {
            if let SIRInstruction::Store(addr, _, _, _, _, _) = inst
                && addr.region == WORKING_REGION
                && dynamic_write_vars.contains(&addr.var_id)
            {
                addr.region = SPARSE_WORKING_REGION;
            }
        }
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
                .ok_or(SLTNodeArenaEditError::SiteIdOverflow { site_id, offset })
        })
        .map_err(|error| {
            ParserError::illegal_context("ForFold runtime-event remap", error.to_string(), None)
        })
}

fn collect_glue_sources(
    expr: NodeId,
    arena: &SLTNodeArena<GlueAddr>,
) -> HashSet<VarAtomBase<GlueAddr>> {
    let mut set = HashSet::default();
    collect_glue_sources_with_window(expr, None, arena, &mut set);
    set
}

fn readmem_timing_enabled() -> bool {
    tracing::enabled!(tracing::Level::DEBUG)
}

fn collect_glue_sources_with_window(
    expr: NodeId,
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
        SLTNode::Capture { expr, .. } => {
            collect_glue_sources_with_window(*expr, window, arena, set);
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
            result,
            initials,
            updates,
            effects,
            continue_cond,
            ..
        } => {
            if let SLTLoopBound::Expr(node) = start {
                collect_glue_sources_with_window(*node, None, arena, set);
            }
            if let SLTLoopBound::Expr(node) = end {
                collect_glue_sources_with_window(*node, None, arena, set);
            }
            if let celox_slt::SLTForFoldResult::Transient { initial, update } = result {
                collect_glue_sources_with_window(*initial, None, arena, set);
                collect_glue_sources_with_window(*update, None, arena, set);
            }
            for init in initials {
                collect_glue_sources_with_window(init.expr, None, arena, set);
            }
            for update in updates {
                collect_glue_sources_with_window(update.expr, None, arena, set);
            }
            for effect in effects {
                match effect {
                    celox_slt::SLTForEffect::Event { guard, args, .. } => {
                        if let Some(guard) = guard {
                            collect_glue_sources_with_window(*guard, None, arena, set);
                        }
                        for arg in args {
                            collect_glue_sources_with_window(*arg, None, arena, set);
                        }
                    }
                    celox_slt::SLTForEffect::Runner(runner) => {
                        collect_glue_sources_with_window(*runner, None, arena, set);
                    }
                }
            }
            collect_glue_sources_with_window(*continue_cond, None, arena, set);
            set.retain(|atom| atom.id != *loop_var);
        }
        SLTNode::ForFoldGroup {
            loop_var,
            entry_guard,
            states,
            ..
        } => {
            let mut group_sources = HashSet::default();
            collect_glue_sources_with_window(*entry_guard, None, arena, &mut group_sources);
            for state in states {
                collect_glue_sources_with_window(state.initial, None, arena, &mut group_sources);
            }
            let mut update_sources = HashSet::default();
            for state in states {
                collect_glue_sources_with_window(state.update, None, arena, &mut update_sources);
            }
            update_sources.retain(|atom| {
                atom.id != *loop_var && !carried_glue_states_cover_atom(atom, states)
            });
            group_sources.extend(update_sources);
            set.extend(group_sources);
        }
        SLTNode::Constant(_, _, _, _) => {}
    }
}

fn carried_glue_states_cover_atom(
    atom: &VarAtomBase<GlueAddr>,
    states: &[SLTForFoldGroupState<GlueAddr>],
) -> bool {
    let mut ranges = states
        .iter()
        .filter(|state| state.target.id == atom.id)
        .map(|state| state.target.access)
        .collect::<Vec<_>>();
    ranges.sort_unstable_by_key(|access| (access.lsb, access.msb));

    let mut next = atom.access.lsb;
    for range in ranges {
        if range.msb < next {
            continue;
        }
        if range.lsb > next {
            return false;
        }
        if range.msb >= atom.access.msb {
            return true;
        }
        let Some(after) = range.msb.checked_add(1) else {
            return false;
        };
        next = after;
    }
    false
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
            && width.is_multiple_of(8)
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
    if !width.is_multiple_of(8) && !out.is_empty() {
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
    use num_bigint::{BigInt, BigUint};
    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Expression, Factor, Ir, Op, Statement, VarId},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    use super::{
        collect_glue_sources, collect_parent_output_address_sources,
        resolve_readmem_path_with_fallback,
    };
    use crate::{GlueAddr, HashMap};
    use celox_design::{BinaryOp, BitAccess, VarAtomBase};
    use celox_slt::{SLTForFoldGroupState, SLTNode, SLTNodeArena};

    fn parse_top_module(code: &str) -> veryl_analyzer::ir::Module {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parser.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2(&parser.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

        let top = veryl_parser::resource_table::insert_str("Top");
        ir.components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) if module.name == top => Some(module),
                _ => None,
            })
            .expect("Top module not found")
    }

    #[test]
    fn output_address_sources_skip_power_exponent() {
        let mut module = parse_top_module(
            r#"
module Top (
    base: input logic<2>,
    idx: input logic,
    q: output logic<4>,
    data: output logic<2>,
) {
    function exponent (x: input logic, y: output logic) -> logic {
        y = x;
        return 1'b1;
    }
    always_comb {
        data = 2'b0;
        q = exponent(idx, data[idx]);
        q = base ** 2;
    }
}
"#,
        );
        let comb = module
            .declarations
            .iter_mut()
            .find_map(|declaration| match declaration {
                Declaration::Comb(comb) => Some(comb),
                _ => None,
            })
            .expect("No always_comb found in Top");
        let Statement::Assign(seed) = &comb.statements[1] else {
            panic!("expected seed assignment");
        };
        let seed = seed.expr.clone();
        let Expression::Term(seed_factor) = &seed else {
            panic!("expected function call expression");
        };
        let Factor::FunctionCall(call) = seed_factor.as_ref() else {
            panic!("expected function call expression");
        };
        let data_id = call
            .outputs
            .values()
            .flatten()
            .next()
            .expect("missing output actual")
            .id;
        let Statement::Assign(pow_assign) = &mut comb.statements[2] else {
            panic!("expected power assignment");
        };
        let Expression::Binary(_, Op::Pow, exponent, _) = &mut pow_assign.expr else {
            panic!("expected power expression");
        };
        **exponent = seed;
        let expression = pow_assign.expr.clone();

        let mut arena = SLTNodeArena::new();
        let mut sources = HashMap::default();
        collect_parent_output_address_sources(
            &module,
            &Default::default(),
            &expression,
            &mut arena,
            &mut sources,
        )
        .unwrap();
        assert!(!sources.contains_key(&data_id));
    }

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

    #[test]
    fn for_fold_group_glue_sources_keep_initial_but_hide_scoped_updates() {
        let loop_id = VarId::default();
        let mut state_id = loop_id;
        state_id.inc();
        let mut external_id = state_id;
        external_id.inc();
        let loop_addr = GlueAddr::Parent(loop_id);
        let state_addr = GlueAddr::Parent(state_id);
        let external_addr = GlueAddr::Parent(external_id);

        let mut arena = SLTNodeArena::new();
        let input = |arena: &mut SLTNodeArena<GlueAddr>, variable| {
            arena
                .alloc(SLTNode::Input {
                    variable,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(0, 7),
                })
                .unwrap()
        };
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let initial = input(&mut arena, state_addr);
        let state_input = input(&mut arena, state_addr);
        let loop_input = input(&mut arena, loop_addr);
        let external_input = input(&mut arena, external_addr);
        let uncovered_state_input = arena
            .alloc(SLTNode::Input {
                variable: state_addr,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(8, 15),
            })
            .unwrap();
        let scoped_sum = arena
            .alloc(SLTNode::Binary(state_input, BinaryOp::Add, loop_input))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(scoped_sum, BinaryOp::Add, external_input))
            .unwrap();
        let update = arena
            .alloc(SLTNode::Binary(
                update,
                BinaryOp::Add,
                uncovered_state_input,
            ))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: loop_addr,
                loop_width: 8,
                loop_signed: false,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(state_addr, 0, 7),
                    initial,
                    update,
                }],
            })
            .unwrap();

        let sources = collect_glue_sources(group, &arena);

        assert!(sources.contains(&VarAtomBase::new(state_addr, 0, 7)));
        assert!(sources.contains(&VarAtomBase::new(state_addr, 8, 15)));
        assert!(sources.contains(&VarAtomBase::new(external_addr, 0, 7)));
        assert!(!sources.iter().any(|atom| atom.id == loop_addr));
    }
}
