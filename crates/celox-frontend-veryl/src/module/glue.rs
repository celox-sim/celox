//! Glue between a parent and its instances: sources, effects and verification.

use super::*;

// Output connections are implicit continuous assignments (IEEE 1800-2023
// 23.3.3.2); their destination selections must be constant (10.2, Table 10-1).
pub(super) fn check_output_destination(dst: &AssignDestination) -> Result<(), ParserError> {
    if !is_static_access(&dst.index, &dst.select) {
        return Err(ParserError::illegal_context(
            "output port connection",
            "output port destination must use constant indices and selects",
            Some(&dst.token),
        ));
    }
    Ok(())
}

pub(super) fn collect_parent_address_expression_sources(
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

pub(super) fn collect_parent_output_address_sources(
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
                    | SystemFunctionKind::Size(..)
                    | SystemFunctionKind::Readmemh(_, _)
                    | SystemFunctionKind::Finish => Ok(()),
                }
            }
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
        },
        Expression::Unary(_, inner, _) => {
            collect_parent_output_address_sources(module, store, inner, arena, out)
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

pub(super) fn build_parent_effect_glue(
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
                        transparent_copy: false,
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

pub(super) fn subtract_composed_accesses(
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

pub(super) fn verify_glue_block(
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

pub(super) fn collect_glue_sources_with_window(
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
            if let SLTLoopBound::Expr(node) | SLTLoopBound::TypedExpr { node, .. } = start {
                collect_glue_sources_with_window(*node, None, arena, set);
            }
            if let SLTLoopBound::Expr(node) | SLTLoopBound::TypedExpr { node, .. } = end {
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

pub(super) fn carried_glue_states_cover_atom(
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
