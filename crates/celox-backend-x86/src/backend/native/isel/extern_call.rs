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
    let ty = ExternValue::of_verified(&ctx.register_types[&dst]);
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
