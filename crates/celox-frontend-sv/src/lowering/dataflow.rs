//! Optimize the combinational definitions and clocked sinks of a SystemVerilog module.
use super::*;
use celox_slt::canon::Reinterner;

/// Canonicalize combinational definitions and clocked consumers together.
/// Other paths can contain effects and keep their original nodes. Signal reads
/// mark sampling boundaries. A wire driven exclusively with zero has that
/// value even before the first two-state settle and can cross the boundary.
pub(super) fn optimize(
    module: &mut SimModule,
    wires: &HashSet<SourceVarId>,
) -> Result<HashMap<NodeId, NodeId>, String> {
    let roots: Vec<_> = module
        .next_state
        .forms
        .iter()
        .flat_map(|(_, form)| form.nodes())
        .collect();
    let mut definitions = HashMap::default();
    let mut multiple = HashSet::default();
    for path in &module.comb_blocks {
        if let Some(target) = path.target.var()
            && definitions.insert(target.id, path).is_some()
        {
            multiple.insert(target.id);
        }
    }
    let initialized: HashSet<_> = module
        .initial_memory_values
        .iter()
        .chain(&module.declaration_initial_memory_values)
        .chain(&module.folded_initial_memory_values)
        .filter(|init| !matches!(&init.data, InitialStateData::Packed {value, ..} if value == &Default::default()))
        .map(|init| init.address)
        .collect();
    let mut settled = HashSet::default();
    let mut fixed = HashMap::default();
    for (&id, path) in &definitions {
        let LogicPathTarget::Var(target) = &path.target else {
            continue;
        };
        if !wires.contains(&id) || multiple.contains(&id) || initialized.contains(&id) {
            continue;
        }
        if target.access.lsb == 0
            && target.access.msb + 1 == module.variables[&id].metadata.width
            && path.previous_sources.is_empty()
            && path.pre_lower_nodes.is_empty()
            && path.local_inputs.is_empty()
            && !path.sources.iter().any(|source| source.id == id)
        {
            settled.insert(id);
        }
        let mut zero_ranges = Vec::new();
        let mut pending = vec![(path.expr, target.access.lsb)];
        while let Some((node, lsb)) = pending.pop() {
            match module.arena.get(node) {
                SLTNode::Constant(value, mask, width, _)
                    if value == &Default::default() && mask == &Default::default() =>
                {
                    if lsb <= target.access.msb {
                        zero_ranges.push(BitAccess::new(
                            lsb,
                            (lsb + width - 1).min(target.access.msb),
                        ));
                    }
                }
                SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, child) => {
                    pending.push((*child, lsb))
                }
                SLTNode::Concat(parts) => {
                    let mut offset = lsb;
                    for &(part, width) in parts.iter().rev() {
                        if module.arena.node_width(part) == Some(width) {
                            pending.push((part, offset));
                        }
                        offset += width;
                    }
                }
                _ => {}
            }
        }
        if !zero_ranges.is_empty() {
            fixed.insert(id, zero_ranges);
        }
    }
    let inputs = celox_slt::canon::reachable(&module.arena, roots.iter().copied());
    let mut shared = SLTNodeArena::new();
    let mut copier = Reinterner::new(&module.arena);
    let mut producer_reads = 0usize;
    let mut constants = 0usize;
    for &node in &inputs {
        if let SLTNode::Input {
            variable,
            access,
            signed,
            index,
        } = module.arena.get(node)
        {
            producer_reads += usize::from(definitions.contains_key(variable));
            if index.is_empty()
                && fixed.get(variable).is_some_and(|ranges| {
                    ranges
                        .iter()
                        .any(|range| range.lsb <= access.lsb && access.msb <= range.msb)
                })
            {
                let zero = shared
                    .alloc(SLTNode::Constant(
                        Default::default(),
                        Default::default(),
                        access.msb - access.lsb + 1,
                        *signed,
                    ))
                    .map_err(|e| e.to_string())?;
                copier.substitute(node, zero);
                constants += 1;
            }
        }
    }
    let mut comb = Vec::new();
    for (index, path) in module.comb_blocks.iter().enumerate() {
        if matches!(path.target, LogicPathTarget::Var(_))
            && path.pre_lower_nodes.is_empty()
            && path.local_inputs.is_empty()
            && let Ok(value) = copier.copy(&mut shared, path.expr)
        {
            comb.push((index, value));
        }
    }
    let mut values = HashMap::default();
    for root in roots {
        let canonical = copier.copy(&mut shared, root).map_err(|e| e.to_string())?;
        values.insert(root, canonical);
    }
    let edge_nodes: HashSet<_> = celox_slt::canon::reachable(&shared, values.values().copied())
        .into_iter()
        .collect();
    // A register may use the settled copy of a common calculation if every
    // input and the result live in internal wires written solely by the
    // combinational schedule. Active and NBA procedures cannot invalidate it.
    // Before the first settle all these wires have the two-state zero value.
    let mut sampled = HashMap::default();
    let mut restore = HashMap::default();
    for (index, canonical) in comb {
        if !edge_nodes.contains(&canonical) {
            continue;
        }
        let path = &mut module.comb_blocks[index];
        let LogicPathTarget::Var(target) = &path.target else {
            continue;
        };
        let value = shared.get(canonical);
        if matches!(value, SLTNode::Input { .. } | SLTNode::Constant(..)) {
            continue;
        }
        path.expr = value
            .map_addr(canonical, &shared, &mut module.arena, &mut restore, &|id| {
                *id
            })
            .map_err(|err| err.to_string())?;
        if settled.contains(&target.id)
            && shared.node_width(canonical) == Some(target.access.msb + 1)
            && zero_preserving_wire_expression(&shared, canonical, &settled)
        {
            let load = module
                .arena
                .alloc(SLTNode::Input {
                    variable: target.id,
                    signed: false,
                    index: Vec::new(),
                    access: target.access,
                })
                .map_err(|err| err.to_string())?;
            sampled.entry(canonical).or_insert(load);
        }
    }
    let shared_wires = sampled.len();
    for value in values.values_mut() {
        *value = shared
            .get(*value)
            .map_addr(*value, &shared, &mut module.arena, &mut sampled, &|id| *id)
            .map_err(|err| err.to_string())?;
    }
    for (_, form) in &mut module.next_state.forms {
        for root in &mut form.roots {
            root.value = values[&root.value];
        }
    }
    tracing::debug!(target: "celox::dataflow", module = %module.name, producer_reads, constants, shared_wires, "scheduled comb and clocked roots");
    Ok(values)
}

fn zero_preserving_wire_expression(
    arena: &SLTNodeArena<SourceVarId>,
    root: NodeId,
    wires: &HashSet<SourceVarId>,
) -> bool {
    celox_slt::canon::reachable(arena, [root])
        .into_iter()
        .all(|node| match arena.get(node) {
            SLTNode::Input {
                variable,
                signed: false,
                index,
                ..
            } => index.is_empty() && wires.contains(variable),
            SLTNode::Constant(value, mask, _, false) => {
                value == &Default::default() && mask == &Default::default()
            }
            SLTNode::Binary(
                _,
                BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul,
                _,
            )
            | SLTNode::Unary(UnaryOp::Ident | UnaryOp::ToTwoState, _)
            | SLTNode::Slice { .. }
            | SLTNode::Concat(_)
            | SLTNode::Mux { .. } => true,
            _ => false,
        })
}
