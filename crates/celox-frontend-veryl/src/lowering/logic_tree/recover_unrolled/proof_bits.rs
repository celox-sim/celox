//! Canonical single-bit forms for comparing proof outputs.

use super::*;

#[derive(Default)]
pub(super) struct ProofBitCanonicalizer {
    pub(super) bit_cache: HashMap<(NodeId, usize), NodeId>,
    pub(super) opaque_bits: HashMap<OpaqueProofKey, NodeId>,
    pub(super) signed_cache: HashMap<NodeId, bool>,
    /// Dense low-to-high mapping for each Concat. Building this once avoids
    /// rescanning an increasingly long part list independently for every bit.
    pub(super) concat_layouts: HashMap<NodeId, Vec<(NodeId, usize)>>,
    #[cfg(test)]
    pub(super) bit_cache_hits: usize,
    #[cfg(test)]
    pub(super) bit_cache_misses: usize,
    #[cfg(test)]
    pub(super) concat_layout_builds: usize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum OpaqueProofKey {
    Binary {
        op: BinaryOp,
        bit: usize,
        width: usize,
        lhs: Vec<NodeId>,
        rhs: Vec<NodeId>,
    },
    Unary {
        op: UnaryOp,
        bit: usize,
        width: usize,
        input: Vec<NodeId>,
    },
}

impl ProofBitCanonicalizer {
    // Memoized (node, bit) pairs can otherwise grow as expression depth times
    // output width. Keep their payload proportional to the proof arena, rather
    // than retaining every bit column for the lifetime of the whole proof.
    // Only evict BETWEEN complete lhs/rhs comparisons: the postorder evaluator
    // requires its children's entries while reducing a single bit.
    pub(super) fn trim_bit_cache(&mut self, arena: &SLTNodeArena<VarId>) {
        let budget = arena
            .len()
            .saturating_mul(std::mem::size_of::<SLTNode<VarId>>());
        let bytes = self
            .bit_cache
            .len()
            .saturating_mul(std::mem::size_of::<((NodeId, usize), NodeId)>());
        if bytes > budget {
            self.bit_cache.clear();
        }
    }

    pub(super) fn concat_bit_source(
        &mut self,
        node: NodeId,
        parts: &[(NodeId, usize)],
        bit: usize,
    ) -> Option<(NodeId, usize)> {
        #[cfg(test)]
        let mut built = false;
        let source = match self.concat_layouts.entry(node) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                entry.into_mut().get(bit).copied()
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let total_width = parts.iter().try_fold(0usize, |width, (_, part_width)| {
                    width.checked_add(*part_width)
                })?;
                let mut layout = Vec::new();
                layout.try_reserve_exact(total_width).ok()?;
                for &(part, part_width) in parts.iter().rev() {
                    layout.extend((0..part_width).map(|part_bit| (part, part_bit)));
                }
                debug_assert_eq!(layout.len(), total_width);
                #[cfg(test)]
                {
                    built = true;
                }
                entry.insert(layout).get(bit).copied()
            }
        };
        #[cfg(test)]
        if built {
            self.concat_layout_builds += 1;
        }
        source
    }
}

pub(super) fn proof_outputs_match(
    lhs: &[NodeId],
    rhs: &[NodeId],
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<bool> {
    // A constant-index LHS is represented as RangeStore slices/concats, while
    // its dynamic template is a full-width mask/shift read-modify-write. Keep
    // the proof exact by comparing their per-bit canonical forms. Operations
    // that are not safely bit-decomposable remain opaque slices.
    if lhs == rhs {
        return Some(true);
    }
    if lhs.len() != rhs.len() {
        return Some(false);
    }

    for (&lhs, &rhs) in lhs.iter().zip(rhs) {
        let width = get_width(lhs, arena);
        if width != get_width(rhs, arena) {
            return Some(false);
        }
        for bit in 0..width {
            let lhs = canonicalize_proof_bit(lhs, bit, arena, canonicalizer)?;
            let rhs = canonicalize_proof_bit(rhs, bit, arena, canonicalizer)?;
            let matches = lhs == rhs;
            canonicalizer.trim_bit_cache(arena);
            if !matches {
                return Some(false);
            }
        }
    }
    Some(true)
}

pub(super) fn canonicalize_proof_bit(
    node: NodeId,
    bit: usize,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<NodeId> {
    if canonicalizer.bit_cache.contains_key(&(node, bit)) {
        return canonicalize_proof_bit_ready(node, bit, arena, canonicalizer);
    }
    // Demand-driven postorder traversal. The reducer below only visits cached
    // children, so deeply unrolled expressions consume heap worklist space,
    // not one native stack frame per expression/bit.
    let mut pending = vec![(node, bit, false)];
    while let Some((current, current_bit, ready)) = pending.pop() {
        if canonicalizer
            .bit_cache
            .contains_key(&(current, current_bit))
        {
            continue;
        }
        if current_bit >= get_width(current, arena) {
            return None;
        }
        if ready {
            canonicalize_proof_bit_ready(current, current_bit, arena, canonicalizer)?;
            continue;
        }
        let mut children = Vec::new();
        match arena.get(current) {
            SLTNode::Binary(lhs, BinaryOp::And | BinaryOp::Or | BinaryOp::Xor, rhs)
                if current_bit < get_width(*lhs, arena) && current_bit < get_width(*rhs, arena) =>
            {
                children.extend([(*lhs, current_bit), (*rhs, current_bit)]);
            }
            SLTNode::Binary(lhs, op @ (BinaryOp::Shl | BinaryOp::Shr), rhs) => {
                if let Some(shift) = specialized_constant(*rhs, arena)
                    .filter(|constant| constant.mask.is_zero())
                    .and_then(|constant| constant.value.to_usize())
                {
                    let source = if *op == BinaryOp::Shl {
                        current_bit.checked_sub(shift)
                    } else {
                        current_bit.checked_add(shift)
                    };
                    if let Some(source) = source.filter(|&bit| bit < get_width(*lhs, arena)) {
                        children.push((*lhs, source));
                    }
                }
            }
            SLTNode::Unary(UnaryOp::BitNot, inner) if current_bit < get_width(*inner, arena) => {
                children.push((*inner, current_bit));
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } if get_width(*cond, arena) == 1
                && current_bit < get_width(*then_expr, arena)
                && current_bit < get_width(*else_expr, arena) =>
            {
                children.extend([
                    (*cond, 0),
                    (*then_expr, current_bit),
                    (*else_expr, current_bit),
                ]);
            }
            SLTNode::Concat(parts) => {
                children.push(canonicalizer.concat_bit_source(current, parts, current_bit)?);
            }
            SLTNode::Slice { expr, access } => {
                children.push((*expr, access.lsb.checked_add(current_bit)?));
            }
            SLTNode::Binary(lhs, _, rhs) => {
                children.extend((0..get_width(*lhs, arena)).map(|bit| (*lhs, bit)));
                children.extend((0..get_width(*rhs, arena)).map(|bit| (*rhs, bit)));
            }
            SLTNode::Unary(_, inner) => {
                children.extend((0..get_width(*inner, arena)).map(|bit| (*inner, bit)));
            }
            _ => {}
        }
        pending.push((current, current_bit, true));
        pending.extend(
            children
                .into_iter()
                .rev()
                .map(|(child, bit)| (child, bit, false)),
        );
    }
    canonicalizer.bit_cache.get(&(node, bit)).copied()
}

pub(super) fn canonicalize_proof_bit_ready(
    node: NodeId,
    bit: usize,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<NodeId> {
    if let Some(&canonical) = canonicalizer.bit_cache.get(&(node, bit)) {
        #[cfg(test)]
        {
            canonicalizer.bit_cache_hits += 1;
        }
        return Some(canonical);
    }
    #[cfg(test)]
    {
        canonicalizer.bit_cache_misses += 1;
    }
    let width = get_width(node, arena);
    if bit >= width {
        return None;
    }

    let canonical = match arena.get(node).clone() {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty() => arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index,
                access: BitAccess::new(access.lsb.checked_add(bit)?, access.lsb.checked_add(bit)?),
            })
            .ok()?,
        SLTNode::Constant(value, mask, _, _) => arena
            .alloc(SLTNode::Constant(
                (&value >> bit) & BigUint::from(1u8),
                (&mask >> bit) & BigUint::from(1u8),
                1,
                false,
            ))
            .ok()?,
        SLTNode::Binary(lhs, op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), rhs)
            if bit < get_width(lhs, arena) && bit < get_width(rhs, arena) =>
        {
            let lhs = canonicalize_proof_bit(lhs, bit, arena, canonicalizer)?;
            let rhs = canonicalize_proof_bit(rhs, bit, arena, canonicalizer)?;
            alloc_proof_bit_binary(lhs, op, rhs, arena)?
        }
        SLTNode::Binary(lhs, op @ (BinaryOp::Shl | BinaryOp::Shr), rhs) => {
            let Some(shift) = specialized_constant(rhs, arena)
                .filter(|constant| constant.mask.is_zero())
                .and_then(|constant| constant.value.to_usize())
            else {
                return alloc_opaque_proof_bit(node, bit, arena, canonicalizer);
            };
            let source_bit = match op {
                BinaryOp::Shl => bit.checked_sub(shift),
                BinaryOp::Shr => bit.checked_add(shift),
                _ => unreachable!(),
            };
            match source_bit.filter(|&source_bit| source_bit < get_width(lhs, arena)) {
                Some(source_bit) => canonicalize_proof_bit(lhs, source_bit, arena, canonicalizer)?,
                None => alloc_proof_bit_constant(false, arena)?,
            }
        }
        SLTNode::Unary(UnaryOp::BitNot, inner) if bit < get_width(inner, arena) => {
            let inner = canonicalize_proof_bit(inner, bit, arena, canonicalizer)?;
            alloc_proof_bit_not(inner, arena)?
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } if get_width(cond, arena) == 1
            && bit < get_width(then_expr, arena)
            && bit < get_width(else_expr, arena) =>
        {
            let cond = canonicalize_proof_bit(cond, 0, arena, canonicalizer)?;
            let then_expr = canonicalize_proof_bit(then_expr, bit, arena, canonicalizer)?;
            let else_expr = canonicalize_proof_bit(else_expr, bit, arena, canonicalizer)?;
            if then_expr == else_expr {
                then_expr
            } else if let Some(value) = known_proof_bit(cond, arena) {
                if value { then_expr } else { else_expr }
            } else {
                arena
                    .alloc(SLTNode::Mux {
                        cond,
                        then_expr,
                        else_expr,
                    })
                    .ok()?
            }
        }
        SLTNode::Concat(parts) => {
            let (part, part_bit) = canonicalizer.concat_bit_source(node, &parts, bit)?;
            canonicalize_proof_bit(part, part_bit, arena, canonicalizer)?
        }
        SLTNode::Slice { expr, access } => {
            canonicalize_proof_bit(expr, access.lsb.checked_add(bit)?, arena, canonicalizer)?
        }
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        _ => return canonicalize_opaque_proof_bit(node, bit, arena, canonicalizer),
    };
    canonicalizer.bit_cache.insert((node, bit), canonical);
    Some(canonical)
}

pub(super) fn canonicalize_opaque_proof_bit(
    node: NodeId,
    bit: usize,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<NodeId> {
    let key = match arena.get(node).clone() {
        SLTNode::Binary(lhs, op, rhs) => {
            let (lhs_bits, rhs_bits) =
                canonicalize_opaque_binary_operands(lhs, op, rhs, arena, canonicalizer)?;
            OpaqueProofKey::Binary {
                op,
                bit,
                width: get_width(node, arena),
                lhs: lhs_bits,
                rhs: rhs_bits,
            }
        }
        SLTNode::Unary(op, input) => OpaqueProofKey::Unary {
            op,
            bit,
            width: get_width(node, arena),
            input: (0..get_width(input, arena))
                .map(|child_bit| canonicalize_proof_bit(input, child_bit, arena, canonicalizer))
                .collect::<Option<Vec<_>>>()?,
        },
        _ => return alloc_opaque_proof_bit(node, bit, arena, canonicalizer),
    };
    if let Some(&canonical) = canonicalizer.opaque_bits.get(&key) {
        canonicalizer.bit_cache.insert((node, bit), canonical);
        return Some(canonical);
    }
    let canonical = alloc_opaque_proof_bit(node, bit, arena, canonicalizer)?;
    canonicalizer.opaque_bits.insert(key, canonical);
    Some(canonical)
}

pub(super) fn canonicalize_opaque_binary_operands(
    lhs: NodeId,
    op: BinaryOp,
    rhs: NodeId,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<(Vec<NodeId>, Vec<NodeId>)> {
    let lhs_width = get_width(lhs, arena);
    let rhs_width = get_width(rhs, arena);
    let comparison = matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::EqWildcard
            | BinaryOp::NeWildcard
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
    );
    let operand_width = if comparison {
        lhs_width.max(rhs_width)
    } else {
        0
    };
    let sign_extend = matches!(
        op,
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS
    ) || matches!(
        op,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard
    ) && proof_node_signed(lhs, arena, &mut canonicalizer.signed_cache)
        && proof_node_signed(rhs, arena, &mut canonicalizer.signed_cache);

    let canonicalize_operand = |operand: NodeId,
                                width: usize,
                                canonicalizer: &mut ProofBitCanonicalizer,
                                arena: &mut SLTNodeArena<VarId>|
     -> Option<Vec<NodeId>> {
        let target_width = if comparison { operand_width } else { width };
        let mut bits = (0..width)
            .map(|child_bit| canonicalize_proof_bit(operand, child_bit, arena, canonicalizer))
            .collect::<Option<Vec<_>>>()?;
        let extension = if sign_extend {
            *bits.last()?
        } else {
            alloc_proof_bit_constant(false, arena)?
        };
        bits.resize(target_width, extension);
        Some(bits)
    };

    Some((
        canonicalize_operand(lhs, lhs_width, canonicalizer, arena)?,
        canonicalize_operand(rhs, rhs_width, canonicalizer, arena)?,
    ))
}

pub(super) fn proof_node_signed(
    root: NodeId,
    arena: &SLTNodeArena<VarId>,
    cache: &mut HashMap<NodeId, bool>,
) -> bool {
    enum Rule {
        Known(bool),
        Copy(NodeId),
        Both(NodeId, NodeId),
    }
    let mut pending = vec![(root, false)];
    while let Some((node, ready)) = pending.pop() {
        if cache.contains_key(&node) {
            continue;
        }
        let rule = match arena.get(node) {
            SLTNode::Input { signed, .. } | SLTNode::Constant(_, _, _, signed) => {
                Rule::Known(*signed)
            }
            SLTNode::Binary(lhs, op, rhs) => match op {
                BinaryOp::Float { .. } => Rule::Known(false),
                BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqCase
                | BinaryOp::NeCase
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
                | BinaryOp::DivU
                | BinaryOp::RemU => Rule::Known(false),
                BinaryOp::DivS | BinaryOp::RemS => Rule::Known(true),
                BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => Rule::Copy(*lhs),
                BinaryOp::Add
                | BinaryOp::Sub
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor => Rule::Both(*lhs, *rhs),
            },
            SLTNode::Unary(UnaryOp::Float { op, .. }, _) => {
                Rule::Known(matches!(op, celox_design::FloatUnaryOp::ToSigned { .. }))
            }
            SLTNode::Unary(
                UnaryOp::LogicNot
                | UnaryOp::And
                | UnaryOp::Or
                | UnaryOp::Xor
                | UnaryOp::PopCount
                | UnaryOp::CountLeadingZeros
                | UnaryOp::CountTrailingZeros
                | UnaryOp::ToPlanes
                | UnaryOp::FromPlanes,
                _,
            ) => Rule::Known(false),
            SLTNode::Unary(
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot,
                inner,
            ) => Rule::Copy(*inner),
            SLTNode::Capture { expr, .. } => Rule::Copy(*expr),
            SLTNode::Mux {
                then_expr,
                else_expr,
                ..
            } => Rule::Both(*then_expr, *else_expr),
            SLTNode::ForFold { loop_signed, .. } => Rule::Known(*loop_signed),
            SLTNode::ForFoldGroup { .. } | SLTNode::Concat(_) | SLTNode::Slice { .. } => {
                Rule::Known(false)
            }
        };
        let value = match rule {
            Rule::Known(value) => value,
            Rule::Copy(child) if ready => cache[&child],
            Rule::Both(lhs, rhs) if ready => cache[&lhs] && cache[&rhs],
            Rule::Copy(child) => {
                pending.extend([(node, true), (child, false)]);
                continue;
            }
            Rule::Both(lhs, rhs) => {
                pending.extend([(node, true), (rhs, false), (lhs, false)]);
                continue;
            }
        };
        cache.insert(node, value);
    }
    cache[&root]
}

pub(super) fn alloc_opaque_proof_bit(
    node: NodeId,
    bit: usize,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<NodeId> {
    let canonical = if get_width(node, arena) == 1 {
        node
    } else {
        arena
            .alloc(SLTNode::Slice {
                expr: node,
                access: BitAccess::new(bit, bit),
            })
            .ok()?
    };
    canonicalizer.bit_cache.insert((node, bit), canonical);
    Some(canonical)
}

pub(super) fn alloc_proof_bit_constant(
    value: bool,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    arena
        .alloc(SLTNode::Constant(
            BigUint::from(u8::from(value)),
            BigUint::from(0u8),
            1,
            false,
        ))
        .ok()
}

pub(super) fn known_proof_bit(node: NodeId, arena: &SLTNodeArena<VarId>) -> Option<bool> {
    let constant = specialized_constant(node, arena)?;
    (constant.width == 1 && constant.mask.is_zero()).then(|| !constant.value.is_zero())
}

pub(super) fn alloc_proof_bit_binary(
    lhs: NodeId,
    op: BinaryOp,
    rhs: NodeId,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    let lhs_known = known_proof_bit(lhs, arena);
    let rhs_known = known_proof_bit(rhs, arena);
    match op {
        BinaryOp::And => {
            if lhs_known == Some(false) || rhs_known == Some(false) {
                return alloc_proof_bit_constant(false, arena);
            }
            if lhs_known == Some(true) {
                return Some(rhs);
            }
            if rhs_known == Some(true) {
                return Some(lhs);
            }
        }
        BinaryOp::Or => {
            if lhs_known == Some(true) || rhs_known == Some(true) {
                return alloc_proof_bit_constant(true, arena);
            }
            if lhs_known == Some(false) {
                return Some(rhs);
            }
            if rhs_known == Some(false) {
                return Some(lhs);
            }
        }
        BinaryOp::Xor => {}
        _ => return None,
    }
    arena.alloc(SLTNode::Binary(lhs, op, rhs)).ok()
}

pub(super) fn alloc_proof_bit_not(
    inner: NodeId,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    if let Some(value) = known_proof_bit(inner, arena) {
        alloc_proof_bit_constant(!value, arena)
    } else {
        arena.alloc(SLTNode::Unary(UnaryOp::BitNot, inner)).ok()
    }
}
