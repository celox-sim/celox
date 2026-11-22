//! Tabulate a dense state decode whose branches select constant values.
use super::*;
use num_bigint::BigUint;

fn boolean<A: Clone + Eq + Hash>(arena: &SLTNodeArena<A>, mut node: NodeId) -> NodeId {
    while let SLTNode::Unary(UnaryOp::ToTwoState | UnaryOp::Ident | UnaryOp::Or, child) =
        *arena.get(node)
    {
        if arena.node_width(child) != Some(1) {
            break;
        }
        node = child;
    }
    node
}

#[derive(Default)]
struct TableInputs {
    atoms: Vec<(NodeId, usize)>,
    // Destination, operand bit set, and whether all or any operands must hold.
    computed: Vec<(usize, u64, bool)>,
    slots: HashMap<NodeId, usize>,
}

fn conjunction(op: BinaryOp) -> Option<bool> {
    match op {
        BinaryOp::And | BinaryOp::LogicAnd => Some(true),
        BinaryOp::Or | BinaryOp::LogicOr => Some(false),
        _ => None,
    }
}

impl TableInputs {
    fn atom(&mut self, node: NodeId) -> usize {
        let slot = self.slots.len();
        *self.slots.entry(node).or_insert_with(|| {
            self.atoms.push((node, slot));
            slot
        })
    }

    // Pull known table inputs out of an associative gate. For example, once
    // enable is an input, global_a & (enable & global_b) needs just one
    // further input, global_a & global_b, shared by all the enabled rows.
    fn factor<A: Clone + Eq + Hash>(
        &self,
        arena: &SLTNodeArena<A>,
        control: NodeId,
    ) -> Option<(bool, u64, Vec<NodeId>)> {
        let SLTNode::Binary(left, op, right) = *arena.get(control) else {
            return None;
        };
        let and = conjunction(op)?;
        let mut pending = vec![left, right];
        let mut others = Vec::new();
        let mut known = 0;
        let mut visited = 0;
        while let Some(node) = pending.pop() {
            visited += 1;
            if visited > 32 || arena.node_width(node) != Some(1) {
                return None;
            }
            let node = boolean(arena, node);
            if let Some(slot) = self.slots.get(&node) {
                known |= 1u64 << slot;
            } else if let SLTNode::Binary(a, op, b) = *arena.get(node)
                && conjunction(op) == Some(and)
            {
                pending.extend([a, b]);
            } else {
                others.push(node);
            }
        }
        if known == 0 {
            return None;
        }
        others.sort_unstable();
        others.dedup();
        Some((and, known, others))
    }

    fn new<A: Clone + Eq + Hash>(
        arena: &mut SLTNodeArena<A>,
        controls: HashSet<NodeId>,
    ) -> Result<Option<Self>, SLTNodeFactsError> {
        let mut controls: Vec<_> = controls.into_iter().collect();
        controls.sort_unstable();
        let mut inputs = Self::default();
        for control in controls {
            if inputs.slots.contains_key(&control) {
                continue;
            }
            if let Some((and, mut known, rest)) = inputs.factor(arena, control) {
                if let Some((&first, tail)) = rest.split_first() {
                    let op = if and { BinaryOp::And } else { BinaryOp::Or };
                    let mut residual = first;
                    for &operand in tail {
                        residual = arena.alloc(SLTNode::Binary(residual, op, operand))?;
                    }
                    let slot = inputs.atom(residual);
                    if slot >= 64 {
                        return Ok(None);
                    }
                    known |= 1u64 << slot;
                }
                let slot = inputs.slots.len();
                inputs.slots.insert(control, slot);
                inputs.computed.push((slot, known, and));
            } else {
                inputs.atom(control);
            }
            if inputs.slots.len() > 64 {
                return Ok(None);
            }
        }
        inputs.atoms.sort_unstable();
        Ok(Some(inputs))
    }

    fn flags(&self, bits: usize) -> u64 {
        let mut flags = 0u64;
        for (i, (_, slot)) in self.atoms.iter().enumerate() {
            flags |= (((bits >> i) & 1) as u64) << slot;
        }
        for &(slot, mask, and) in &self.computed {
            let value = if and {
                flags & mask == mask
            } else {
                flags & mask != 0
            };
            flags |= u64::from(value) << slot;
        }
        flags
    }
}

pub(super) fn tabulate<A: Clone + Eq + Hash>(
    arena: &mut SLTNodeArena<A>,
    selector: NodeId,
    groups: &[(NodeId, Vec<(u64, NodeId)>)],
    default: NodeId,
    original: NodeId,
) -> Result<Option<NodeId>, SLTNodeFactsError> {
    let state_bits = crate::get_width(selector, arena);
    let width = crate::get_width(original, arena);
    if !(2..=4).contains(&state_bits)
        || !(2..=8).contains(&width)
        || groups.len() < 4
        || groups.len() * 2 < 1 << state_bits
    {
        return Ok(None);
    }
    let mut values: Vec<_> = groups.iter().map(|g| g.0).chain([default]).collect();
    let mut controls = HashSet::default();
    let mut decisions = HashSet::default();
    let mut constants = HashMap::default();
    while let Some(node) = values.pop() {
        match arena.get(node) {
            SLTNode::Constant(v, mask, w, _) if *w == width && mask.is_zero() => {
                let Some(value) = v.to_u8() else {
                    return Ok(None);
                };
                constants.insert(node, value);
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                if decisions.insert(node) {
                    if arena.node_width(*cond) != Some(1) || decisions.len() > 64 {
                        return Ok(None);
                    }
                    controls.insert(boolean(arena, *cond));
                    values.extend([*then_expr, *else_expr]);
                }
            }
            _ => return Ok(None),
        }
    }
    // Reserve table synthesis for dense decodes with substantial control logic.
    if decisions.len() < 2 * groups.len() || bound_signed(original, arena) {
        return Ok(None);
    }
    let Some(inputs) = TableInputs::new(arena, controls)? else {
        return Ok(None);
    };
    // One byte per row, at most 1 KiB of constant data per distinct table.
    if inputs.atoms.len() + state_bits > 10 {
        return Ok(None);
    }
    let mut states = vec![default; 1 << state_bits];
    for (value, labels) in groups {
        for &(label, _) in labels {
            states[label as usize] = *value;
        }
    }
    let mut bytes = Vec::with_capacity(states.len() << inputs.atoms.len());
    for bits in 0usize..1 << inputs.atoms.len() {
        let flags = inputs.flags(bits);
        for &state in &states {
            let mut node = state;
            while let SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } = *arena.get(node)
            {
                node = if flags & (1 << inputs.slots[&boolean(arena, cond)]) != 0 {
                    then_expr
                } else {
                    else_expr
                };
            }
            bytes.push(constants[&node]);
        }
    }
    let bit_width = bytes.len() * 8;
    let table = arena.alloc(SLTNode::Constant(
        BigUint::from_bytes_le(&bytes),
        0u8.into(),
        bit_width,
        false,
    ))?;
    let mut parts: Vec<_> = inputs
        .atoms
        .iter()
        .rev()
        .map(|(node, _)| (*node, 1))
        .collect();
    parts.push((selector, state_bits));
    let low = arena.alloc(SLTNode::Constant(0u8.into(), 0u8.into(), 3, false))?;
    parts.push((low, 3));
    let offset = arena.alloc(SLTNode::Concat(parts))?;
    let shifted = arena.alloc(SLTNode::Binary(table, BinaryOp::Shr, offset))?;
    arena
        .alloc(SLTNode::Slice {
            expr: shifted,
            access: BitAccess::new(0, width - 1),
        })
        .map(Some)
}
