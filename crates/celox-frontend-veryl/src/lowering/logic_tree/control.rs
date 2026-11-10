//! Conditions of procedural control flow and merging of symbolic store versions.

use super::*;

pub(super) fn bool_node(
    arena: &mut SLTNodeArena<VarId>,
    value: bool,
) -> Result<NodeId, SLTNodeFactsError> {
    arena.alloc(SLTNode::Constant(
        BigUint::from(value as u8),
        BigUint::from(0u8),
        1,
        false,
    ))
}

pub(super) fn function_assigns_whole_var(assign: &AssignStatement, var_id: VarId) -> bool {
    assign.dst.len() == 1
        && assign.dst[0].id == var_id
        && assign.dst[0].index.0.is_empty()
        && assign.dst[0].select.0.is_empty()
        && assign.dst[0].select.1.is_none()
}

pub(super) fn constant_bool(arena: &SLTNodeArena<VarId>, node: NodeId) -> Option<bool> {
    match arena.get(node) {
        SLTNode::Constant(val, _, _, _) => Some(*val != BigUint::from(0u8)),
        _ => None,
    }
}

/// Convert an expression result to the boolean used by procedural control.
///
/// Unlike the conditional operator, an unknown condition in an `if`, `case`,
/// or loop does not merge both paths: only a definite one takes the true
/// path.  Keep that distinction in the SLT instead of relying on a backend's
/// treatment of a four-state mux.
pub(super) fn procedural_condition<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    condition: NodeId,
) -> Result<NodeId, SLTNodeFactsError> {
    if let SLTNode::Constant(value, unknown, width, _) = arena.get(condition) {
        let known_mask = if *width == 0 {
            BigUint::from(0u8)
        } else {
            let width_mask = (BigUint::from(1u8) << *width) - BigUint::from(1u8);
            &width_mask ^ (unknown & &width_mask)
        };
        return arena.alloc(SLTNode::Constant(
            BigUint::from(u8::from((value & known_mask) != BigUint::from(0u8))),
            BigUint::from(0u8),
            1,
            false,
        ));
    }
    let truth = arena.alloc(SLTNode::Unary(UnaryOp::Or, condition))?;
    arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, truth))
}

pub(super) fn combine_active_guard(
    arena: &mut SLTNodeArena<VarId>,
    outer: Option<&ActiveGuard>,
    condition: NodeId,
    condition_sources: &HashSet<VarAtomBase<VarId>>,
) -> Result<ActiveGuard, SLTNodeFactsError> {
    let Some((outer_expr, outer_sources)) = outer else {
        return Ok((condition, condition_sources.clone()));
    };
    let expr = arena.alloc(SLTNode::Binary(*outer_expr, BinaryOp::LogicAnd, condition))?;
    let mut sources = outer_sources.clone();
    sources.extend(condition_sources.iter().copied());
    Ok((expr, sources))
}

pub(super) fn invert_active_condition(
    arena: &mut SLTNodeArena<VarId>,
    condition: NodeId,
) -> Result<NodeId, SLTNodeFactsError> {
    arena.alloc(SLTNode::Unary(UnaryOp::LogicNot, condition))
}

pub(super) fn merge_control_expr(
    cond_expr: NodeId,
    then_expr: NodeId,
    else_expr: NodeId,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<NodeId, SLTNodeFactsError> {
    if then_expr == else_expr {
        Ok(then_expr)
    } else {
        arena.alloc(SLTNode::Mux {
            cond: cond_expr,
            then_expr,
            else_expr,
        })
    }
}

pub(super) fn materialize_aligned_symbolic_part(
    id: VarId,
    absolute: BitAccess,
    value: &Option<(NodeId, HashSet<VarAtomBase<VarId>>)>,
    relative: BitAccess,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(NodeId, HashSet<VarAtomBase<VarId>>), SLTNodeFactsError> {
    let Some((expr, sources)) = value else {
        let input = arena.alloc(SLTNode::Input {
            variable: id,
            signed: false,
            index: Vec::new(),
            access: absolute,
        })?;
        let mut sources = HashSet::default();
        sources.insert(VarAtomBase::new(id, absolute.lsb, absolute.msb));
        return Ok((input, sources));
    };

    let width = get_width(*expr, arena);
    let expr = if width == 0 || (relative.lsb == 0 && relative.msb == width - 1) {
        *expr
    } else {
        arena.alloc(SLTNode::Slice {
            expr: *expr,
            access: relative,
        })?
    };
    Ok((expr, sources.clone()))
}

pub(super) fn merge_symbolic_versions(
    module: &Module,
    then_store: &SymbolicStore<VarId>,
    else_store: &SymbolicStore<VarId>,
    cond_expr: NodeId,
    cond_sources: &HashSet<VarAtomBase<VarId>>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<SymbolicStore<VarId>, ParserError> {
    // A branch clone is a symbolic version. Start with one version and only
    // visit definitions whose copy-on-write entries diverged; shared pages and
    // shared entries cannot need a phi.
    let mut merged_store = then_store.fork();
    for id in then_store.differing_keys(else_store) {
        // Function-local state can be introduced while evaluating only one
        // branch. It is scoped to that branch and must not escape the merge.
        let Some(t_range_store) = then_store.get(id) else {
            continue;
        };
        let Some(e_range_store) = else_store.get(id) else {
            continue;
        };

        if t_range_store == e_range_store {
            continue;
        }

        let var = &module.variables[id];
        let var_width = resolve_total_width(module, var)?;
        let aligned_parts = t_range_store
            .aligned_parts(e_range_store)
            .map_err(|error| range_store_error("conditional merge", error, None))?;
        let store_width = aligned_parts
            .last()
            .map_or(0, |(access, _, _)| access.msb + 1);
        if store_width != var_width {
            return Err(ParserError::illegal_context(
                "conditional merge",
                format!(
                    "symbolic range width {store_width} differs from declared width {var_width}"
                ),
                None,
            ));
        }

        let mut merged_range_store = RangeStore {
            ranges: std::collections::BTreeMap::new(),
        };
        for (access, (then_value, then_access), (else_value, else_access)) in aligned_parts {
            let t_modified = then_value.is_some();
            let e_modified = else_value.is_some();
            let (t_expr, t_sources) =
                materialize_aligned_symbolic_part(*id, access, then_value, then_access, arena)?;
            let (e_expr, e_sources) =
                materialize_aligned_symbolic_part(*id, access, else_value, else_access, arena)?;

            let result_val = if !t_modified && !e_modified {
                None
            } else if t_expr == e_expr {
                let mut sources = t_sources;
                sources.extend(e_sources);
                Some((t_expr, sources))
            } else {
                let mut sources = cond_sources.clone();
                sources.extend(t_sources);
                sources.extend(e_sources);

                Some((
                    arena.alloc(SLTNode::Mux {
                        cond: cond_expr,
                        then_expr: t_expr,
                        else_expr: e_expr,
                    })?,
                    sources,
                ))
            };

            merged_range_store.ranges.insert(
                access.lsb,
                (result_val, access.msb - access.lsb + 1, access.lsb),
            );
        }

        merged_store.insert(*id, merged_range_store);
    }

    Ok(merged_store)
}

pub(crate) fn combine_parts_with_default<'a, A, I>(
    var_id: A,
    start_lsb: usize,
    parts: I,
    arena: &mut SLTNodeArena<A>,
) -> Result<(NodeId, HashSet<VarAtomBase<A>>), SLTNodeFactsError>
where
    A: Clone + PartialEq + Eq + Hash + 'a,
    I: IntoIterator<Item = (&'a Option<(NodeId, HashSet<VarAtomBase<A>>)>, BitAccess)>,
{
    let mut fixed_parts = Vec::new();
    let mut total_sources = HashSet::default();
    let mut current_lsb = start_lsb;
    for (val_opt, access) in parts {
        let width = access.msb - access.lsb + 1;
        match val_opt {
            Some((expr, s)) => {
                total_sources.extend(s.iter().cloned());
                fixed_parts.push((*expr, access));
            }
            None => {
                let input_node = arena.alloc(SLTNode::Input {
                    variable: var_id.clone(),
                    signed: false,
                    index: vec![],
                    access: BitAccess::new(current_lsb, current_lsb + width - 1),
                })?;
                total_sources.insert(VarAtomBase::new(
                    var_id.clone(),
                    current_lsb,
                    current_lsb + width - 1,
                ));
                fixed_parts.push((input_node, BitAccess::new(0, width - 1)));
            }
        }
        current_lsb += width;
    }
    if fixed_parts.is_empty() {
        return Ok((
            arena.alloc(SLTNode::Constant(
                BigUint::from(0u32),
                BigUint::from(0u32),
                0,
                false,
            ))?,
            HashSet::default(),
        ));
    }
    if fixed_parts.len() == 1 {
        let (expr, access) = fixed_parts
            .into_iter()
            .next()
            .expect("the single symbolic part exists");
        let w = get_width(expr, arena);
        if w == 0 {
            return Ok((expr, total_sources));
        }
        if access.lsb == 0 && access.msb == w - 1 {
            return Ok((expr, total_sources));
        } else {
            return Ok((arena.alloc(SLTNode::Slice { expr, access })?, total_sources));
        }
    }

    let mut concat_parts = Vec::new();
    for (expr, access) in fixed_parts {
        let w = access.msb - access.lsb + 1;
        let slice = arena.alloc(SLTNode::Slice { expr, access })?;
        concat_parts.push((slice, w));
    }
    concat_parts.reverse();
    Ok((arena.alloc(SLTNode::Concat(concat_parts))?, total_sources))
}
