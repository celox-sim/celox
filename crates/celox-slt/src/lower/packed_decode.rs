//! A chain of exact comparisons selecting one bit can fit in an immediate.
//! One data bit selects the upper half of the truth table.
use super::*;

fn bit_value<A: Clone + Eq + Hash>(
    arena: &SLTNodeArena<A>,
    mut node: NodeId,
    data: &mut Option<NodeId>,
) -> Option<[bool; 2]> {
    let mut invert = false;
    loop {
        if arena.node_width(node)? != 1 {
            return None;
        }
        match arena.get(node) {
            SLTNode::Unary(UnaryOp::ToTwoState | UnaryOp::Ident | UnaryOp::Or, child) => {
                node = *child
            }
            SLTNode::Unary(UnaryOp::LogicNot | UnaryOp::BitNot, child) => {
                invert = !invert;
                node = *child;
            }
            SLTNode::Constant(value, mask, _, _) if mask.is_zero() => {
                return Some([value.bit(0) ^ invert; 2]);
            }
            SLTNode::Input { index, .. }
                if index.is_empty() && *data.get_or_insert(node) == node =>
            {
                return Some([invert, !invert]);
            }
            _ => return None,
        }
    }
}

impl SLTToSIRLowerer {
    pub(super) fn lower_packed_decode<
        A: Clone + Eq + Hash + std::fmt::Debug + std::fmt::Display,
    >(
        &mut self,
        builder: &mut SIRBuilder<A>,
        root: NodeId,
        arena: &SLTNodeArena<A>,
        cache: &mut crate::HashMap<NodeId, RegisterId>,
        allow_cache: bool,
    ) -> Option<RegisterId> {
        let mut node = root;
        let mut selector = None;
        let mut data = None;
        let mut seen = 0u64;
        let mut rows = [0u64; 2];
        let mut comparisons = 0;
        while let SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } = *arena.get(node)
        {
            if comparisons == 64 || (node != root && cache.contains_key(&node)) {
                return None;
            }
            let (input, op, index) = crate::decode::exact_label(arena, cond)?;
            if op == BinaryOp::EqWildcard || arena.node_width(input)? > 5 {
                return None;
            }
            if *selector.get_or_insert(input) != input {
                return None;
            }
            let bit = 1u64 << index;
            let values = bit_value(arena, then_expr, &mut data)?;
            if seen & bit == 0 {
                for (row, set) in rows.iter_mut().zip(values) {
                    if set {
                        *row |= bit;
                    }
                }
                seen |= bit;
            }
            comparisons += 1;
            node = else_expr;
        }
        // A small mux or a single comparison already has a cheap lowering.
        if comparisons < 3 {
            return None;
        }
        let defaults = bit_value(arena, node, &mut data)?;
        let selector = selector?;
        let width = arena.node_width(selector)?;
        let entries = 1usize << width;
        // Constant-only chains already have a dedicated SIR lookup optimization.
        let data = data?;
        let table_width = entries * 2;
        let domain = u64::MAX >> (64 - entries);
        for (row, set) in rows.iter_mut().zip(defaults) {
            if set {
                *row |= domain & !seen;
            }
        }
        let bits = rows[0] | (rows[1] << entries);
        let low = self.lower_inner(builder, selector, arena, cache, None, allow_cache);
        let high = self.lower_inner(builder, data, arena, cache, None, allow_cache);
        let index = builder.alloc_bit(width + 1, false);
        builder.emit(SIRInstruction::Concat(index, vec![high, low]));
        let table = builder.alloc_bit(table_width, false);
        builder.emit(SIRInstruction::Imm(table, SIRValue::new(bits)));
        let shifted = builder.alloc_bit(table_width, false);
        builder.emit(SIRInstruction::Binary(shifted, table, BinaryOp::Shr, index));
        Some(self.slice_reg(builder, shifted, &BitAccess::new(0, 0)))
    }
}
