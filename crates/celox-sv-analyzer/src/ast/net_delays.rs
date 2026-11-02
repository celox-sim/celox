//! Separate the resolved value of a net from its delayed visible value (10.3.3).
use super::*;

pub(super) fn lower(
    tree: &AnalysisContext<'_>,
    dimensions: &mut PackedDimensions,
    ports: &[Port],
    instances: &[Instance],
    signals: &mut Vec<Signal>,
    processes: &mut Vec<CombProcess>,
) -> Converted<()> {
    if !processes.iter().any(|process| process.net_delay) {
        return Ok(());
    }
    let (delays, mut drivers): (Vec<_>, Vec<_>) = std::mem::take(processes)
        .into_iter()
        .partition(|process| process.net_delay);
    for (serial, delay) in delays.into_iter().enumerate() {
        let name = delay.assignments[0].lhs.name();
        let target = ports.iter().find(|port| port.name() == name);
        if target.is_some_and(|port| port.direction != PortDirection::Output) {
            return Err(unsupported(
                "delayed continuous assignment: net delay on an input or inout port",
            ));
        }
        let mut raw = format!("%net_delay{serial}_raw");
        while dimensions.get(&raw).is_some() || signals.iter().any(|signal| signal.name() == raw) {
            raw.push('_');
        }
        // Connection directions are resolved by the hierarchy linker later.
        // Reject this boundary rather than bypassing or delaying an input read.
        let names = HashMap::from_iter([(name.to_string(), raw.clone())]);
        let rename = scope::Renamer { names: &names };
        if instances
            .iter()
            .flat_map(|instance| &instance.port_connections)
            .any(|connection| {
                let mut renamed = connection.clone();
                renamed.map_expressions(|expr| rename.expr(expr));
                // `actual` can fall back to the formal name for empty and
                // non-identifier connections. Only bound values refer to nets.
                renamed != *connection
            })
        {
            return Err(unsupported(
                "delayed continuous assignment: independent net delay in an instance connection",
            ));
        }
        let ty = signals
            .iter()
            .find(|signal| signal.name() == name)
            .map(|signal| signal.r#type.clone())
            .or_else(|| target.map(|port| port.r#type.clone()))
            .ok_or_else(|| {
                unsupported("delayed continuous assignment: independent net delay target")
            })?;
        let shape = dimensions.get(name).cloned().ok_or_else(|| {
            unsupported("delayed continuous assignment: independent net delay shape")
        })?;
        if shape.string
            || shape.real
            || shape.class.is_some()
            || shape.chandle
            || shape.container.is_some()
            || !shape.unpacked.is_empty()
            || shape.unpacked_struct.is_some()
        {
            return Err(unsupported(
                "delayed continuous assignment: nonintegral or unpacked net delay",
            ));
        }
        let width = lvalue_expr_type(&tree.functions, &LValue::Ident(name.into()), dimensions)
            .ok_or_else(|| {
                unsupported("delayed continuous assignment: independent net delay width")
            })?
            .width;
        let mut ranges = Vec::new();
        let retarget = |lhs: &mut LValue| {
            let (LValue::Ident(base) | LValue::Select { name: base, .. }) = lhs else {
                lowered_object()
            };
            if base == name {
                *base = raw.clone();
                true
            } else {
                false
            }
        };
        let mut drive = |lhs: &mut LValue| -> Converted<()> {
            if lhs.name() != name {
                return Ok(());
            }
            ranges.push(match lhs {
                LValue::Object { .. } => lowered_object(),
                LValue::Ident(_) => None,
                LValue::Select { msb, lsb, .. } => {
                    let msb = eval_ast_const_expr(&tree.functions, msb, &dimensions.const_env);
                    let lsb = eval_ast_const_expr(&tree.functions, lsb, &dimensions.const_env);
                    let (Some(msb), Some(lsb)) = (msb, lsb) else {
                        return Err(unsupported("delayed continuous assignment: dynamic driver of an independent delayed net"));
                    };
                    Some((msb.min(lsb), msb.max(lsb)))
                }
            });
            retarget(lhs);
            Ok(())
        };
        for process in drivers
            .iter_mut()
            .filter(|process| process.kind == CombProcessKind::ContinuousAssign)
        {
            for assignment in &mut process.assignments {
                retarget(&mut assignment.lhs);
            }
            for stmt in &mut process.body {
                match stmt {
                    Stmt::Assign { lhs, .. } => drive(lhs)?,
                    Stmt::AssignConcat { parts, .. } => {
                        for lhs in parts {
                            drive(lhs)?;
                        }
                    }
                    _ => {}
                }
            }
        }
        // With no driver, a delayed net remains Z from time zero (6.7.1).
        if ranges.is_empty() {
            continue;
        }
        let unresolved = signals
            .iter()
            .any(|signal| signal.name() == name && signal.unresolved_net)
            || target.is_some_and(|port| port.unresolved_net);
        // The frontend addresses a one-dimensional packed vector with its
        // declared indices; multidimensional packed values use flat offsets.
        let range = match ty.packed_ranges.as_slice() {
            [range] => Some((
                eval_ast_const_expr(&tree.functions, range.left(), &dimensions.const_env),
                eval_ast_const_expr(&tree.functions, range.right(), &dimensions.const_env),
            )),
            _ => None,
        };
        let range = match range {
            Some((Some(left), Some(right))) => Some((left, right)),
            Some(_) => {
                return Err(unsupported(
                    "delayed continuous assignment: independent net delay bounds",
                ));
            }
            None => None,
        };
        signals.push(Signal::new_net(raw.clone(), ty).with_unresolved_net(unresolved));
        dimensions.insert(raw.clone(), shape.clone());
        dimensions
            .expression_signedness
            .insert(raw.clone(), shape.signed);
        // Net delays act on individual bits. A vector assignment delay instead
        // compares and reschedules the entire destination-sized vector.
        for bit in 0..width {
            let bit = match range {
                Some((left, right)) if left >= right => right + bit as i128,
                Some((_, right)) => right - bit as i128,
                None => bit as i128,
            };
            // Statically undriven bits remain Z immediately, even when other
            // bits of the same vector have delayed drivers.
            if !ranges.iter().any(|range| match range {
                None => true,
                Some((low, high)) => (*low..=*high).contains(&bit),
            }) {
                continue;
            }
            let select = |name: String| LValue::Select {
                name,
                msb: ConstExpr::Literal(bit.to_string()),
                lsb: ConstExpr::Literal(bit.to_string()),
                signed: false,
                array_slice_width: None,
                array_slice_reversed: false,
                is_2state: shape.is_2state,
            };
            let lhs = select(name.into());
            let rhs = expr_from_lvalue(tree, &select(raw.clone()), dimensions);
            let mut process = CombProcess::new(
                CombProcessKind::ContinuousAssign,
                delay.condition.clone(),
                vec![Assignment::new(lhs, rhs)],
            );
            process.delay = delay.delay;
            process.creation_order = delay.creation_order;
            process.site = delay.site.clone();
            process.process_site = delay.process_site.clone();
            drivers.push(process);
        }
    }
    *processes = drivers;
    Ok(())
}
