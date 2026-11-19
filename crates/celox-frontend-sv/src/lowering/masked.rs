//! Four-state masked writes preserve value and X/Z mask bits independently.
use super::procedural::slt_error;
use super::*;

pub(super) fn dynamic_four_state(
    arena: &mut SLTNodeArena<SourceVarId>,
    current: NodeId,
    value: NodeId,
    mask: NodeId,
    up: NodeId,
    down: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    let original_width = celox_slt::get_width(current, arena);
    let width = original_width.next_multiple_of(64);
    let current = coerce_node_width(arena, current, Some(width), false).map_err(slt_error)?;
    let value = coerce_node_width(arena, value, Some(width), false).map_err(slt_error)?;
    let mask = coerce_node_width(arena, mask, Some(width), false).map_err(slt_error)?;
    let current = arena
        .alloc(SLTNode::Unary(UnaryOp::ToPlanes, current))
        .map_err(slt_error)?;
    let value = arena
        .alloc(SLTNode::Unary(UnaryOp::ToPlanes, value))
        .map_err(slt_error)?;
    let keep = arena
        .alloc(SLTNode::Unary(UnaryOp::BitNot, mask))
        .map_err(slt_error)?;
    let mut planes = Vec::new();
    for lsb in [width, 0] {
        let access = BitAccess::new(lsb, lsb + width - 1);
        let old = arena
            .alloc(SLTNode::Slice {
                expr: current,
                access,
            })
            .map_err(slt_error)?;
        let new = arena
            .alloc(SLTNode::Slice {
                expr: value,
                access,
            })
            .map_err(slt_error)?;
        let raised = arena
            .alloc(SLTNode::Binary(new, BinaryOp::Shl, up))
            .map_err(slt_error)?;
        let placed = arena
            .alloc(SLTNode::Binary(raised, BinaryOp::Shr, down))
            .map_err(slt_error)?;
        let taken = arena
            .alloc(SLTNode::Binary(placed, BinaryOp::And, mask))
            .map_err(slt_error)?;
        let kept = arena
            .alloc(SLTNode::Binary(old, BinaryOp::And, keep))
            .map_err(slt_error)?;
        let merged = arena
            .alloc(SLTNode::Binary(kept, BinaryOp::Or, taken))
            .map_err(slt_error)?;
        planes.push((merged, width));
    }
    let planes = arena.alloc(SLTNode::Concat(planes)).map_err(slt_error)?;
    let merged = arena
        .alloc(SLTNode::Unary(UnaryOp::FromPlanes, planes))
        .map_err(slt_error)?;
    if original_width == width {
        Ok(merged)
    } else {
        arena
            .alloc(SLTNode::Slice {
                expr: merged,
                access: BitAccess::new(0, original_width - 1),
            })
            .map_err(slt_error)
    }
}

/// The bits of `forced` where the two-state `mask` is set and of `value`
/// elsewhere, all `width` bits wide. A four-state merge selects the value
/// and the X/Z planes apart, so a selected Z stays Z.
pub(super) fn select_masked<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    mask: NodeId,
    forced: NodeId,
    value: NodeId,
    width: usize,
    four_state: bool,
) -> Result<NodeId, celox_slt::SLTNodeFactsError> {
    let merge = |arena: &mut SLTNodeArena<A>, mask: NodeId, forced: NodeId, value: NodeId| {
        let keep = arena.alloc(SLTNode::Unary(UnaryOp::BitNot, mask))?;
        let taken = arena.alloc(SLTNode::Binary(forced, BinaryOp::And, mask))?;
        let kept = arena.alloc(SLTNode::Binary(value, BinaryOp::And, keep))?;
        arena.alloc(SLTNode::Binary(kept, BinaryOp::Or, taken))
    };
    if !four_state {
        return merge(arena, mask, forced, value);
    }
    let words = width.next_multiple_of(64);
    let pad = |arena: &mut SLTNodeArena<A>, node: NodeId| {
        if words == width {
            return Ok(node);
        }
        let zero = arena.alloc(SLTNode::Constant(
            BigUint::default(),
            BigUint::default(),
            words - width,
            false,
        ))?;
        arena.alloc(SLTNode::Concat(vec![(zero, words - width), (node, width)]))
    };
    let mask = pad(arena, mask)?;
    let forced = pad(arena, forced)?;
    let value = pad(arena, value)?;
    let forced = arena.alloc(SLTNode::Unary(UnaryOp::ToPlanes, forced))?;
    let value = arena.alloc(SLTNode::Unary(UnaryOp::ToPlanes, value))?;
    let mut planes = Vec::new();
    for lsb in [words, 0] {
        let access = BitAccess::new(lsb, lsb + words - 1);
        let new = arena.alloc(SLTNode::Slice {
            expr: forced,
            access,
        })?;
        let old = arena.alloc(SLTNode::Slice {
            expr: value,
            access,
        })?;
        planes.push((merge(arena, mask, new, old)?, words));
    }
    let planes = arena.alloc(SLTNode::Concat(planes))?;
    let merged = arena.alloc(SLTNode::Unary(UnaryOp::FromPlanes, planes))?;
    if words == width {
        return Ok(merged);
    }
    arena.alloc(SLTNode::Slice {
        expr: merged,
        access: BitAccess::new(0, width - 1),
    })
}
