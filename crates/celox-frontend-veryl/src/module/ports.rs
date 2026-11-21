//! Source ranges and effects of instance port expressions.

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
                            .indices
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
                for address in index.indices.iter().chain(select.0.iter()) {
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
            Factor::Value(_)
            | Factor::ResetValue(..)
            | Factor::Anonymous(_)
            | Factor::Unknown(_) => Ok(()),
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

pub(super) fn parent_effect_paths(
    initial_store: &SymbolicStore<VarId>,
    store: &SymbolicStore<VarId>,
    written_accesses: &HashMap<VarId, Vec<BitAccess>>,
    output_address_sources: &HashMap<VarId, HashSet<VarAtomBase<VarId>>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<Vec<LogicPath<VarId>>, ParserError> {
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
                let mapped = *expr;
                let mapped = if relative_lsb == 0 && target_width == get_width(mapped, arena) {
                    mapped
                } else {
                    arena.alloc(SLTNode::Slice {
                        expr: mapped,
                        access: BitAccess::new(relative_lsb, relative_msb),
                    })?
                };
                let sources = collect_port_sources(mapped, arena);
                let target = VarAtomBase::new(*id, target_access.lsb, target_access.msb);
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
                    .copied()
                    .filter(|address| {
                        sources.iter().any(|source| {
                            source.id == address.id && source.access.overlaps(&address.access)
                        })
                    })
                    .collect();

                paths.push(LogicPath {
                    transparent_copy: true,
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
                });
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

pub(super) fn collect_port_sources(
    expr: NodeId,
    arena: &SLTNodeArena<VarId>,
) -> HashSet<VarAtomBase<VarId>> {
    let mut set = HashSet::default();
    crate::symbolic::flattening::collect_inputs(expr, arena, &mut set);
    set
}
