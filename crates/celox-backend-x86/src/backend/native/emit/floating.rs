//! Scalar SSE2 floating point. GPR payloads are the storage ABI; XMM0/1
//! are ephemeral, with clobbers declared in x86_slp::clobbers_xmm.
use super::*;
use celox_sir::{FloatBinaryOp as B, FloatPrecision as P, FloatUnaryOp as U};

pub(super) fn emit_binary(
    asm: &mut EmitAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    lhs: VReg,
    rhs: VReg,
    op: B,
    precision: P,
) -> Result<(), IcedError> {
    let (lhs, rhs) = if matches!(op, B::Gt | B::Ge) {
        (rhs, lhs)
    } else {
        (lhs, rhs)
    };
    asm.movq(xmm0, preg_to_reg64(resolve(assignment, lhs)))?;
    asm.movq(xmm1, preg_to_reg64(resolve(assignment, rhs)))?;
    let predicate = match op {
        B::Eq => Some(0),
        B::Ne => Some(4),
        B::Lt | B::Gt => Some(1),
        B::Le | B::Ge => Some(2),
        _ => None,
    };
    if let Some(predicate) = predicate {
        match precision {
            P::F32 => asm.cmpss(xmm0, xmm1, predicate)?,
            P::F64 => asm.cmpsd_3(xmm0, xmm1, predicate)?,
        }
        let output = preg_to_reg32(resolve(assignment, dst));
        asm.movd(output, xmm0)?;
        asm.and(output, 1)?;
    } else {
        match (op, precision) {
            (B::Add, P::F32) => asm.addss(xmm0, xmm1)?,
            (B::Add, P::F64) => asm.addsd(xmm0, xmm1)?,
            (B::Sub, P::F32) => asm.subss(xmm0, xmm1)?,
            (B::Sub, P::F64) => asm.subsd(xmm0, xmm1)?,
            (B::Mul, P::F32) => asm.mulss(xmm0, xmm1)?,
            (B::Mul, P::F64) => asm.mulsd(xmm0, xmm1)?,
            (B::Div, P::F32) => asm.divss(xmm0, xmm1)?,
            (B::Div, P::F64) => asm.divsd(xmm0, xmm1)?,
            _ => unreachable!("unsupported native scalar FP binary operation"),
        }
        match precision {
            P::F32 => asm.movd(preg_to_reg32(resolve(assignment, dst)), xmm0)?,
            P::F64 => asm.movq(preg_to_reg64(resolve(assignment, dst)), xmm0)?,
        }
    }
    Ok(())
}

pub(super) fn emit_unary(
    asm: &mut EmitAssembler,
    assignment: &AssignmentMap,
    dst: VReg,
    src: VReg,
    op: U,
    precision: P,
) -> Result<(), IcedError> {
    let input = preg_to_reg64(resolve(assignment, src));
    let output = preg_to_reg64(resolve(assignment, dst));
    match op {
        U::Neg => {
            if precision == P::F32 {
                let output = preg_to_reg32(resolve(assignment, dst));
                asm.mov(output, preg_to_reg32(resolve(assignment, src)))?;
                asm.btc(output, 31)?;
            } else {
                asm.mov(output, input)?;
                asm.btc(output, 63)?;
            }
        }
        U::ToBool => {
            asm.movq(xmm0, input)?;
            asm.pxor(xmm1, xmm1)?;
            match precision {
                P::F32 => asm.cmpss(xmm0, xmm1, 4)?,
                P::F64 => asm.cmpsd_3(xmm0, xmm1, 4)?,
            }
            let output = preg_to_reg32(resolve(assignment, dst));
            asm.movd(output, xmm0)?;
            asm.and(output, 1)?;
        }
        U::Convert { to } => {
            asm.movq(xmm0, input)?;
            match (precision, to) {
                (P::F32, P::F64) => asm.cvtss2sd(xmm0, xmm0)?,
                (P::F64, P::F32) => asm.cvtsd2ss(xmm0, xmm0)?,
                _ => {}
            }
            match to {
                P::F32 => asm.movd(preg_to_reg32(resolve(assignment, dst)), xmm0)?,
                P::F64 => asm.movq(output, xmm0)?,
            }
        }
        _ => unreachable!("unsupported native scalar FP unary operation"),
    }
    Ok(())
}
