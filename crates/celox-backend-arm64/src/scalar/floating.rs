//! Scalar floating point. GPR payloads are the storage ABI; v0/v1 are
//! caller-clobbered scratch.

use super::*;

/// Emit `FloatBinary`: an arithmetic result, or a comparison as 0 or 1.
pub(super) fn emit_binary(
    ops: &mut VecAssembler<Aarch64Relocation>,
    assignment: &Assignment<VReg>,
    dst: &VReg,
    lhs: &VReg,
    rhs: &VReg,
    op: &celox_sir::FloatBinaryOp,
    precision: &celox_sir::FloatPrecision,
) -> Result<(), EmitError> {
    use celox_sir::{FloatBinaryOp as F, FloatPrecision as P};
    let (dst, lhs, rhs) = (
        resolve(assignment, *dst)?,
        resolve(assignment, *lhs)?,
        resolve(assignment, *rhs)?,
    );
    // v0/v1 are caller-clobbered scratch; persistent loop state uses v29/v31.
    match precision {
        P::F32 => {
            dynasm!(ops ; .arch aarch64 ; fmov s0, W(lhs) ; fmov s1, W(rhs));
            match op {
                F::Add => dynasm!(ops ; .arch aarch64 ; fadd s0, s0, s1),
                F::Sub => dynasm!(ops ; .arch aarch64 ; fsub s0, s0, s1),
                F::Mul => dynasm!(ops ; .arch aarch64 ; fmul s0, s0, s1),
                F::Div => dynasm!(ops ; .arch aarch64 ; fdiv s0, s0, s1),
                _ => dynasm!(ops ; .arch aarch64 ; fcmp s0, s1),
            }
        }
        P::F64 => {
            dynasm!(ops ; .arch aarch64 ; fmov d0, X(lhs) ; fmov d1, X(rhs));
            match op {
                F::Add => dynasm!(ops ; .arch aarch64 ; fadd d0, d0, d1),
                F::Sub => dynasm!(ops ; .arch aarch64 ; fsub d0, d0, d1),
                F::Mul => dynasm!(ops ; .arch aarch64 ; fmul d0, d0, d1),
                F::Div => dynasm!(ops ; .arch aarch64 ; fdiv d0, d0, d1),
                _ => dynasm!(ops ; .arch aarch64 ; fcmp d0, d1),
            }
        }
    }
    match op {
        F::Eq => dynasm!(ops ; .arch aarch64 ; cset X(dst), eq),
        F::Ne => dynasm!(ops ; .arch aarch64 ; cset X(dst), ne),
        // FCMP unordered has NZCV=0011: MI/LS/GT/GE all reject NaN.
        F::Lt => dynasm!(ops ; .arch aarch64 ; cset X(dst), mi),
        F::Le => dynasm!(ops ; .arch aarch64 ; cset X(dst), ls),
        F::Gt => dynasm!(ops ; .arch aarch64 ; cset X(dst), gt),
        F::Ge => dynasm!(ops ; .arch aarch64 ; cset X(dst), ge),
        F::Add | F::Sub | F::Mul | F::Div => match precision {
            P::F32 => dynasm!(ops ; .arch aarch64 ; fmov W(dst), s0),
            P::F64 => dynasm!(ops ; .arch aarch64 ; fmov X(dst), d0),
        },
        F::Rem | F::Pow | F::Atan2 | F::Hypot => {
            unreachable!("complex float operations use helpers")
        }
    }
    Ok(())
}

/// Emit `FloatUnary`: negation, the truth value, or a change of precision.
pub(super) fn emit_unary(
    ops: &mut VecAssembler<Aarch64Relocation>,
    assignment: &Assignment<VReg>,
    dst: &VReg,
    src: &VReg,
    op: &celox_sir::FloatUnaryOp,
    precision: &celox_sir::FloatPrecision,
) -> Result<(), EmitError> {
    use celox_sir::{FloatPrecision as P, FloatUnaryOp as F};
    let (dst, src) = (resolve(assignment, *dst)?, resolve(assignment, *src)?);
    match precision {
        P::F32 => dynasm!(ops ; .arch aarch64 ; fmov s0, W(src)),
        P::F64 => dynasm!(ops ; .arch aarch64 ; fmov d0, X(src)),
    }
    match (op, precision) {
        (F::Neg, P::F32) => dynasm!(ops ; .arch aarch64 ; fneg s0, s0),
        (F::Neg, P::F64) => dynasm!(ops ; .arch aarch64 ; fneg d0, d0),
        (F::ToBool, P::F32) => dynasm!(ops ; .arch aarch64 ; fcmp s0, #0.0),
        (F::ToBool, P::F64) => dynasm!(ops ; .arch aarch64 ; fcmp d0, #0.0),
        (F::Convert { to: P::F64 }, P::F32) => dynasm!(ops ; .arch aarch64 ; fcvt d0, s0),
        (F::Convert { to: P::F32 }, P::F64) => dynasm!(ops ; .arch aarch64 ; fcvt s0, d0),
        (F::Convert { .. }, _) => {}
        _ => unreachable!("integer float conversions use helpers"),
    }
    match op {
        F::ToBool => dynasm!(ops ; .arch aarch64 ; cset X(dst), ne),
        _ => match match op {
            F::Convert { to } => to,
            _ => precision,
        } {
            P::F32 => dynasm!(ops ; .arch aarch64 ; fmov W(dst), s0),
            P::F64 => dynasm!(ops ; .arch aarch64 ; fmov X(dst), d0),
        },
    }
    Ok(())
}
