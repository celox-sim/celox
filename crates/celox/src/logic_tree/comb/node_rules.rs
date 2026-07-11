//! Phase-neutral scalar rules shared by legacy and phase-typed SLT facts.
//!
//! This module deliberately knows neither [`super::node::NodeId`] nor
//! [`super::phase_arena::PhaseNodeId`].  Callers retain their ID namespace and
//! attach the rule failure to the appropriate owner.

use num_bigint::BigUint;

use crate::ir::{BinaryOp, BitAccess, UnaryOp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NodeRuleError {
    pub invariant: &'static str,
    pub message: String,
}

impl NodeRuleError {
    fn new(invariant: &'static str, message: impl Into<String>) -> Self {
        Self {
            invariant,
            message: message.into(),
        }
    }
}

pub(super) fn access_width(access: BitAccess, role: &str) -> Result<usize, NodeRuleError> {
    let Some(span) = access.msb.checked_sub(access.lsb) else {
        return Err(NodeRuleError::new(
            "WIDTH.ACCESS_ORDERED",
            format!(
                "{role} access has lsb {} greater than msb {}",
                access.lsb, access.msb
            ),
        ));
    };
    span.checked_add(1).ok_or_else(|| {
        NodeRuleError::new(
            "WIDTH.ACCESS_REPRESENTABLE",
            format!(
                "{role} access [{}:{}] has a width that overflows usize",
                access.msb, access.lsb
            ),
        )
    })
}

pub(super) fn constant_width(
    value: &BigUint,
    mask: &BigUint,
    width: usize,
) -> Result<usize, NodeRuleError> {
    let representable_bits = u64::try_from(width).unwrap_or(u64::MAX);
    if value.bits() > representable_bits {
        return Err(NodeRuleError::new(
            "CONSTANT.VALUE_FITS_WIDTH",
            format!(
                "constant payload needs {} bits but declares width {width}",
                value.bits()
            ),
        ));
    }
    if mask.bits() > representable_bits {
        return Err(NodeRuleError::new(
            "CONSTANT.MASK_FITS_WIDTH",
            format!(
                "constant X/Z mask needs {} bits but declares width {width}",
                mask.bits()
            ),
        ));
    }
    Ok(width)
}

pub(super) fn binary_width(
    op: BinaryOp,
    lhs_width: usize,
    rhs_width: usize,
) -> Result<usize, NodeRuleError> {
    match op {
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            if lhs_width != rhs_width {
                return Err(NodeRuleError::new(
                    "WIDTH.WILDCARD_OPERANDS_MATCH",
                    format!("wildcard comparison operands have widths {lhs_width} and {rhs_width}"),
                ));
            }
            Ok(1)
        }
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::LtU
        | BinaryOp::LtS
        | BinaryOp::LeU
        | BinaryOp::LeS
        | BinaryOp::GtU
        | BinaryOp::GtS
        | BinaryOp::GeU
        | BinaryOp::GeS
        | BinaryOp::LogicAnd
        | BinaryOp::LogicOr => Ok(1),
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => Ok(lhs_width),
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Rem
        | BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor => Ok(lhs_width.max(rhs_width)),
    }
}

pub(super) fn unary_width(op: UnaryOp, inner_width: usize) -> usize {
    match op {
        UnaryOp::LogicNot | UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => 1,
        UnaryOp::Ident | UnaryOp::Minus | UnaryOp::BitNot => inner_width,
    }
}

pub(super) fn binary_signed(op: BinaryOp, lhs_signed: bool, rhs_signed: bool) -> bool {
    match op {
        BinaryOp::Eq
        | BinaryOp::Ne
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
        | BinaryOp::EqWildcard
        | BinaryOp::NeWildcard => false,
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => lhs_signed,
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::Div
        | BinaryOp::Rem
        | BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor => lhs_signed && rhs_signed,
    }
}

pub(super) fn unary_signed(op: UnaryOp, inner_signed: bool) -> bool {
    match op {
        UnaryOp::Ident | UnaryOp::Minus | UnaryOp::BitNot => inner_signed,
        UnaryOp::LogicNot | UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => false,
    }
}

pub(super) fn mux_signed(then_signed: bool, else_signed: bool) -> bool {
    then_signed && else_signed
}

pub(super) fn mux_width(then_width: usize, else_width: usize) -> usize {
    then_width.max(else_width)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequiredCoercion {
    Identity,
    ZeroExtend,
    SignExtend,
    Truncate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoercionBasis {
    SourceSigned,
    TargetSigned,
    SourceAndTargetSigned,
}

/// Derive a coercion from verified source facts, never from the producer's
/// claimed coercion kind.
pub(super) fn required_coercion(
    source_width: usize,
    source_signed: bool,
    target_width: usize,
    target_signed: bool,
    basis: CoercionBasis,
) -> Result<RequiredCoercion, NodeRuleError> {
    if target_width == 0 {
        return Err(NodeRuleError::new(
            "COERCION.TARGET_NON_ZERO",
            "coercion target width is zero",
        ));
    }
    let sign_extend = match basis {
        CoercionBasis::SourceSigned => source_signed,
        CoercionBasis::TargetSigned => target_signed,
        CoercionBasis::SourceAndTargetSigned => source_signed && target_signed,
    };
    Ok(match source_width.cmp(&target_width) {
        std::cmp::Ordering::Equal => RequiredCoercion::Identity,
        std::cmp::Ordering::Less if sign_extend => RequiredCoercion::SignExtend,
        std::cmp::Ordering::Less => RequiredCoercion::ZeroExtend,
        std::cmp::Ordering::Greater => RequiredCoercion::Truncate,
    })
}

pub(super) fn concat_width(
    widths: impl IntoIterator<Item = usize>,
) -> Result<usize, NodeRuleError> {
    let mut total = 0usize;
    for part_width in widths {
        let Some(next) = total.checked_add(part_width) else {
            return Err(NodeRuleError::new(
                "WIDTH.CONCAT_REPRESENTABLE",
                format!(
                    "declared concat widths overflow usize while adding {part_width} to {total}"
                ),
            ));
        };
        total = next;
    }
    Ok(total)
}

pub(super) fn slice_width(
    access: BitAccess,
    expression_width: usize,
    expression_label: impl std::fmt::Display,
) -> Result<usize, NodeRuleError> {
    let width = access_width(access, "slice")?;
    if access.msb >= expression_width {
        return Err(NodeRuleError::new(
            "WIDTH.SLICE_IN_BOUNDS",
            format!(
                "slice [{msb}:{lsb}] exceeds child {expression_label} width {expression_width}",
                lsb = access.lsb,
                msb = access.msb
            ),
        ));
    }
    Ok(width)
}

pub(super) fn require_nonzero(
    width: usize,
    invariant: &'static str,
    message: impl FnOnce() -> String,
) -> Result<usize, NodeRuleError> {
    if width == 0 {
        Err(NodeRuleError::new(invariant, message()))
    } else {
        Ok(width)
    }
}

pub(super) fn direct_lowerable(width: usize, has_zero_concat_part: bool) -> bool {
    width != 0 && !has_zero_concat_part
}
