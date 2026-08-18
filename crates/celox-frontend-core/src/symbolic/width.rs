use std::hash::Hash;

use celox_design::BitAccess;
use celox_slt::{SLTNode, SLTNodeArena, SLTNodeFactsError, get_width};
use num_bigint::BigUint;

/// Resize a symbolic expression without depending on a source-language AST.
pub fn coerce_node_width<A: Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    expression: celox_slt::NodeId,
    target_width: Option<usize>,
    sign_extend: bool,
) -> Result<celox_slt::NodeId, SLTNodeFactsError> {
    let Some(target_width) = target_width else {
        return Ok(expression);
    };
    let expression_width = get_width(expression, arena);
    if expression_width == 0 && target_width != 0 {
        return Err(SLTNodeFactsError::new(
            "WIDTH.COERCE_SOURCE_NON_ZERO",
            expression,
            format!(
                "cannot coerce zero-width n{} to width {target_width}",
                expression.0
            ),
        ));
    }
    if target_width == 0 && expression_width != 0 {
        return Err(SLTNodeFactsError::new(
            "WIDTH.COERCE_TARGET_NON_ZERO",
            expression,
            format!(
                "cannot coerce width-{expression_width} n{} to zero width",
                expression.0
            ),
        ));
    }
    if expression_width < target_width {
        let pad_width = target_width - expression_width;
        let pad = if sign_extend {
            let msb = arena.alloc(SLTNode::Slice {
                expr: expression,
                access: BitAccess::new(expression_width - 1, expression_width - 1),
            })?;
            let pad = if pad_width == 1 {
                msb
            } else {
                arena.alloc(SLTNode::Concat(
                    std::iter::repeat_n((msb, 1), pad_width).collect(),
                ))?
            };
            (pad, pad_width)
        } else {
            let zero = arena.alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u32),
                pad_width,
                false,
            ))?;
            (zero, pad_width)
        };
        arena.alloc(SLTNode::Concat(vec![pad, (expression, expression_width)]))
    } else if expression_width > target_width {
        arena.alloc(SLTNode::Slice {
            expr: expression,
            access: BitAccess::new(0, target_width - 1),
        })
    } else {
        Ok(expression)
    }
}
