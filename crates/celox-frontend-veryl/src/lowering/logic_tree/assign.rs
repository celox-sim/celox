//! Assignments to statically selected destinations.

use super::*;

pub(super) fn checked_destination_width(
    module: &Module,
    destinations: &[veryl_analyzer::ir::AssignDestination],
    context: &'static str,
    token: Option<&TokenRange>,
) -> Result<usize, ParserError> {
    if destinations.is_empty() {
        return Err(ParserError::illegal_context(
            context,
            "assignment has no destination",
            token,
        ));
    }
    let mut total = 0usize;
    for destination in destinations {
        let width = crate::bitaccess::get_access_width(
            module,
            destination.id,
            &destination.index,
            &destination.select,
        )?;
        total = total.checked_add(width).ok_or_else(|| {
            ParserError::illegal_context(
                context,
                "concatenated destination width overflows usize",
                Some(&destination.token),
            )
        })?;
    }
    if total == 0 {
        return Err(ParserError::illegal_context(
            context,
            "assignment destination has zero width",
            token,
        ));
    }
    Ok(total)
}

pub(super) fn checked_assignment_slice(
    offset: usize,
    width: usize,
    rhs_width: usize,
    destination: &veryl_analyzer::ir::AssignDestination,
) -> Result<(BitAccess, usize), ParserError> {
    let end = offset.checked_add(width).ok_or_else(|| {
        ParserError::illegal_context(
            "concatenated assignment",
            "RHS slice end overflows usize",
            Some(&destination.token),
        )
    })?;
    if width == 0 || end > rhs_width {
        return Err(ParserError::illegal_context(
            "concatenated assignment",
            format!("RHS slice {offset}..{end} is outside width {rhs_width}"),
            Some(&destination.token),
        ));
    }
    Ok((BitAccess::new(offset, end - 1), end))
}

pub(super) fn record_assignment_boundary(
    boundaries: &mut BoundaryMap<VarId>,
    destination: &veryl_analyzer::ir::AssignDestination,
    access: BitAccess,
) -> Result<(), ParserError> {
    let end = access.msb.checked_add(1).ok_or_else(|| {
        ParserError::illegal_context(
            "assignment destination",
            "destination boundary overflows usize",
            Some(&destination.token),
        )
    })?;
    let entry = boundaries.entry(destination.id).or_default();
    entry.insert(access.lsb);
    entry.insert(end);
    Ok(())
}

pub(super) fn update_assignment_range(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    destination: &veryl_analyzer::ir::AssignDestination,
    access: BitAccess,
    mut value: (NodeId, HashSet<VarAtomBase<VarId>>),
    source_is_2state: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(), ParserError> {
    let variable = module.variables.get(&destination.id).ok_or_else(|| {
        ParserError::illegal_context(
            "assignment destination",
            "destination variable is absent from the semantic module",
            Some(&destination.token),
        )
    })?;
    let variable_width = resolve_total_width(module, variable)?;
    if variable_width == 0 || access.msb >= variable_width {
        return Err(ParserError::illegal_context(
            "assignment destination",
            format!(
                "destination access [{}:{}] is outside variable width {variable_width}",
                access.msb, access.lsb
            ),
            Some(&destination.token),
        ));
    }
    if variable.r#type.is_2state() && !source_is_2state {
        value.0 = arena.alloc(SLTNode::Unary(UnaryOp::ToTwoState, value.0))?;
    }
    let range_store = store
        .entry(destination.id)
        .or_insert_with(|| RangeStore::new(None, variable_width));
    range_store.update(access, Some(value)).map_err(|error| {
        range_store_error("assignment destination", error, Some(&destination.token))
    })?;
    Ok(())
}

pub(super) fn eval_assignment_rhs_effectful(
    module: &Module,
    store: &mut SymbolicStore<VarId>,
    stmt: &AssignStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<((NodeId, HashSet<VarAtomBase<VarId>>), BoundaryMap<VarId>), ParserError> {
    let rhs_expected_width = checked_destination_width(
        module,
        &stmt.dst,
        "assignment destination",
        Some(&stmt.expr.token_range()),
    )?;
    if let Expression::ArrayLiteral(items, _) = &stmt.expr {
        let ((node, sources), bounds) = eval_array_literal_expression_effectful(
            module,
            store,
            items,
            Some(rhs_expected_width),
            arena,
        )?;
        if get_width(node, arena) == 0 {
            return Err(ParserError::illegal_context(
                "assignment expression",
                "a zero-width array literal cannot be assigned",
                Some(&stmt.expr.token_range()),
            ));
        }
        Ok((
            (
                coerce_node_width(arena, node, Some(rhs_expected_width), false)?,
                sources,
            ),
            bounds,
        ))
    } else {
        eval_assignment_expression_effectful(
            module,
            store,
            &stmt
                .dst
                .first()
                .map(|dst| crate::float::converted(&stmt.expr, &dst.comptime.r#type))
                .unwrap_or(std::borrow::Cow::Borrowed(&stmt.expr)),
            arena,
            rhs_expected_width,
        )
    }
}

pub(super) fn eval_assign(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    boundaries: BoundaryMap<VarId>,
    stmt: &AssignStatement,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let rhs_is_2state = stmt.expr.comptime().r#type.is_2state();
    let rhs_expected_width = checked_destination_width(
        module,
        &stmt.dst,
        "assignment destination",
        Some(&stmt.expr.token_range()),
    )?;
    let ((rhs_expr, rhs_sources), rhs_bounds) =
        eval_assignment_rhs_effectful(module, &mut store, stmt, arena)?;
    let mut boundaries = merge_boundaries(boundaries, rhs_bounds);

    if stmt.dst.len() == 1 {
        // Single destination: store RHS directly
        let dst = &stmt.dst[0];

        if crate::bitaccess::is_static_access(&dst.index, &dst.select) {
            let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;

            record_assignment_boundary(&mut boundaries, dst, access)?;
            update_assignment_range(
                module,
                &mut store,
                dst,
                access,
                (rhs_expr, rhs_sources.clone()),
                rhs_is_2state,
                arena,
            )?;
        } else {
            let (s, b) = eval_dynamic_assign(
                module,
                store,
                boundaries,
                dst,
                rhs_expr,
                rhs_sources.clone(),
                rhs_is_2state,
                arena,
            )?;
            return Ok((s, b));
        }
    } else {
        // LHS concatenation: slice RHS for each destination
        // dst is ordered MSB-first (e.g., {a, b} means a=MSB, b=LSB),
        // so iterate in reverse to compute offsets from LSB.
        let mut current_offset = 0;
        for dst in stmt.dst.iter().rev() {
            let part_width =
                crate::bitaccess::get_access_width(module, dst.id, &dst.index, &dst.select)?;

            // Slice the RHS to extract the bits for this destination
            let (slice_access, next_offset) =
                checked_assignment_slice(current_offset, part_width, rhs_expected_width, dst)?;
            let slice_expr = arena.alloc(SLTNode::Slice {
                expr: rhs_expr,
                access: slice_access,
            })?;

            if crate::bitaccess::is_static_access(&dst.index, &dst.select) {
                let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;

                record_assignment_boundary(&mut boundaries, dst, access)?;
                update_assignment_range(
                    module,
                    &mut store,
                    dst,
                    access,
                    (slice_expr, rhs_sources.clone()),
                    rhs_is_2state,
                    arena,
                )?;
            } else {
                let (s, b) = eval_dynamic_assign(
                    module,
                    store,
                    boundaries,
                    dst,
                    slice_expr,
                    rhs_sources.clone(),
                    rhs_is_2state,
                    arena,
                )?;
                store = s;
                boundaries = b;
            }

            current_offset = next_offset;
        }
        if current_offset != rhs_expected_width {
            return Err(ParserError::illegal_context(
                "concatenated assignment",
                format!(
                    "destinations cover {current_offset} bits, but the RHS has width {rhs_expected_width}"
                ),
                Some(&stmt.expr.token_range()),
            ));
        }
    }
    Ok((store, boundaries))
}

pub(crate) fn apply_assignment_destination(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    destination: &veryl_analyzer::ir::AssignDestination,
    rhs_expr: NodeId,
    rhs_sources: HashSet<VarAtomBase<VarId>>,
    rhs_is_2state: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    if crate::bitaccess::is_static_access(&destination.index, &destination.select) {
        let access = eval_var_select(
            module,
            destination.id,
            &destination.index,
            &destination.select,
        )?;
        record_assignment_boundary(&mut boundaries, destination, access)?;
        update_assignment_range(
            module,
            &mut store,
            destination,
            access,
            (rhs_expr, rhs_sources),
            rhs_is_2state,
            arena,
        )?;
        Ok((store, boundaries))
    } else {
        eval_dynamic_assign(
            module,
            store,
            boundaries,
            destination,
            rhs_expr,
            rhs_sources,
            rhs_is_2state,
            arena,
        )
    }
}

pub(super) fn assign_node_to_dsts(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    dsts: &[veryl_analyzer::ir::AssignDestination],
    rhs_expr: NodeId,
    rhs_sources: HashSet<VarAtomBase<VarId>>,
    source_is_2state: bool,
    source_signed: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let destination_width = checked_destination_width(
        module,
        dsts,
        "function output destination",
        dsts.first().map(|destination| &destination.token),
    )?;
    let rhs_width = get_width(rhs_expr, arena);
    if rhs_width == 0 {
        return Err(ParserError::illegal_context(
            "function output destination",
            "function output value has zero width",
            dsts.first().map(|destination| &destination.token),
        ));
    }
    let rhs_expr = coerce_node_width(arena, rhs_expr, Some(destination_width), source_signed)?;

    if dsts.len() == 1 {
        let dst = &dsts[0];
        if crate::bitaccess::is_static_access(&dst.index, &dst.select) {
            let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;
            record_assignment_boundary(&mut boundaries, dst, access)?;
            update_assignment_range(
                module,
                &mut store,
                dst,
                access,
                (rhs_expr, rhs_sources),
                source_is_2state,
                arena,
            )?;

            return Ok((store, boundaries));
        }

        return eval_dynamic_assign(
            module,
            store,
            boundaries,
            dst,
            rhs_expr,
            rhs_sources,
            source_is_2state,
            arena,
        );
    }

    let mut current_offset = 0;
    for dst in dsts.iter().rev() {
        let part_width =
            crate::bitaccess::get_access_width(module, dst.id, &dst.index, &dst.select)?;
        let (slice_access, next_offset) =
            checked_assignment_slice(current_offset, part_width, destination_width, dst)?;
        let slice_expr = arena.alloc(SLTNode::Slice {
            expr: rhs_expr,
            access: slice_access,
        })?;

        if crate::bitaccess::is_static_access(&dst.index, &dst.select) {
            let access = eval_var_select(module, dst.id, &dst.index, &dst.select)?;

            record_assignment_boundary(&mut boundaries, dst, access)?;
            update_assignment_range(
                module,
                &mut store,
                dst,
                access,
                (slice_expr, rhs_sources.clone()),
                source_is_2state,
                arena,
            )?;
        } else {
            let (next_store, next_boundaries) = eval_dynamic_assign(
                module,
                store,
                boundaries,
                dst,
                slice_expr,
                rhs_sources.clone(),
                source_is_2state,
                arena,
            )?;
            store = next_store;
            boundaries = next_boundaries;
        }

        current_offset = next_offset;
    }

    if current_offset != destination_width {
        return Err(ParserError::illegal_context(
            "function output destination",
            format!(
                "destinations cover {current_offset} bits, but the output value has width {destination_width}"
            ),
            dsts.first().map(|destination| &destination.token),
        ));
    }

    Ok((store, boundaries))
}
