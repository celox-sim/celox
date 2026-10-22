//! Calls of extern functions through the platform C ABI.

use celox_sir::extern_abi::{ExternValue, SV_LOGIC_MASK_SHIFT};

use super::*;

/// Lower `ExternCall`: convert each argument to its C integer, call, and
/// convert the C result back.
///
/// [`ExternValue`] states how each register is passed; the callee reads only
/// the low bits of a narrower C type, so every argument is passed as a full
/// register.
pub(super) fn lower_extern_call(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: Option<RegisterId>,
    func: u32,
    args: &[RegisterId],
) {
    for (index, &arg) in args.iter().enumerate() {
        let src = extern_argument(ctx, block, arg);
        block.push(MInst::ExternArg {
            index: u8::try_from(index).expect("the SIR verifier bounds extern call arguments"),
            src,
        });
    }
    block.push(MInst::CallExtern {
        func,
        arg_count: u8::try_from(args.len()).expect("the SIR verifier bounds extern call arguments"),
    });
    let Some(dst) = dst else {
        return;
    };
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ExternResult { dst: result });
    define_extern_result(ctx, block, dst, result);
}

/// The 64-bit C integer passed for `arg`.
fn extern_argument(ctx: &mut ISelContext, block: &mut MBlock, arg: RegisterId) -> VReg {
    let value = ctx.reg_map.get(arg);
    let ty = ExternValue::of_verified(&ctx.register_types[&arg]);
    if let Some(width) = ty.sign_extended_width() {
        let shift = (64 - width) as u8;
        let shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShlImm {
            dst: shifted,
            src: value,
            imm: shift,
        });
        let extended = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: extended,
            src: shifted,
            imm: shift,
        });
        return extended;
    }
    if ty != ExternValue::Logic || !ctx.four_state {
        return value;
    }
    let mask = ctx.get_mask(arg, block);
    let shifted = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShlImm {
        dst: shifted,
        src: mask,
        imm: SV_LOGIC_MASK_SHIFT as u8,
    });
    let logic = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: logic,
        lhs: value,
        rhs: shifted,
    });
    logic
}

/// Define `dst` from the C integer `result` of an extern call.
fn define_extern_result(ctx: &mut ISelContext, block: &mut MBlock, dst: RegisterId, result: VReg) {
    let value = ctx.reg_map.get(dst);
    let ty = ExternValue::of(&ctx.register_types[&dst]).unwrap_or(ExternValue::Integer {
        width: ctx.sir_width(&dst),
        signed: false,
    });
    match ty.result_value_mask() {
        u64::MAX => block.push(MInst::Mov {
            dst: value,
            src: result,
        }),
        imm => block.push(MInst::AndImm {
            dst: value,
            src: result,
            imm,
        }),
    }
    if !ctx.four_state {
        return;
    }
    let mask = match ty {
        ExternValue::Logic => {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: result,
                imm: SV_LOGIC_MASK_SHIFT as u8,
            });
            let mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::AndImm {
                dst: mask,
                src: shifted,
                imm: 1,
            });
            mask
        }
        ExternValue::Integer { .. } => {
            let mask = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: mask,
                value: 0,
            });
            mask
        }
    };
    ctx.set_mask(dst, mask);
}

pub(super) fn lower_float(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    rhs: Option<RegisterId>,
    code: u64,
) {
    use celox_sir::{FloatBinaryOp as B, FloatPrecision as P, FloatUnaryOp as U};
    let precision = if code & 256 == 0 { P::F32 } else { P::F64 };
    let opcode = code & 255;
    let binary = match opcode {
        0 => Some(B::Add),
        1 => Some(B::Sub),
        2 => Some(B::Mul),
        3 => Some(B::Div),
        6 => Some(B::Eq),
        7 => Some(B::Ne),
        8 => Some(B::Lt),
        9 => Some(B::Le),
        10 => Some(B::Gt),
        11 => Some(B::Ge),
        _ => None,
    };
    let unary = match opcode {
        32 => Some(U::Neg),
        33 => Some(U::ToBool),
        38 => Some(U::Convert { to: P::F32 }),
        39 => Some(U::Convert { to: P::F64 }),
        _ => None,
    };
    if let Some(op) = binary {
        block.push(MInst::FloatBinary {
            dst: ctx.reg_map.get(dst),
            lhs: ctx.reg_map.get(lhs),
            rhs: ctx.reg_map.get(rhs.expect("binary FP SIR operand")),
            op,
            precision,
        });
    } else if let Some(op) = unary {
        block.push(MInst::FloatUnary {
            dst: ctx.reg_map.get(dst),
            src: ctx.reg_map.get(lhs),
            op,
            precision,
        });
    }
    if binary.is_some() || unary.is_some() {
        if ctx.four_state {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            ctx.set_mask(dst, zero);
        }
        return;
    }
    let a = ctx.reg_map.get(lhs);
    let a = if ctx.four_state && matches!(code & 255, 34 | 35) {
        let mask = ctx.get_mask(lhs, block);
        let known = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: known,
            src: mask,
        });
        let value = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: value,
            lhs: a,
            rhs: known,
        });
        value
    } else {
        a
    };

    let b = if let Some(rhs) = rhs {
        ctx.reg_map.get(rhs)
    } else {
        let v = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm { dst: v, value: 0 });
        v
    };
    let c = ctx.alloc_vreg(SpillDesc::remat(code));
    block.push(MInst::LoadImm {
        dst: c,
        value: code,
    });
    for (index, src) in [a, b, c].into_iter().enumerate() {
        block.push(MInst::ExternArg {
            index: index as u8,
            src,
        });
    }
    block.push(MInst::CallExtern {
        func: celox_sir::float::FLOAT_HELPER_ID,
        arg_count: 3,
    });
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ExternResult { dst: result });
    define_extern_result(ctx, block, dst, result);
}
