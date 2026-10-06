//! Calls of extern functions through the platform C ABI.

use super::*;

/// Lower `ExternCall`: convert each argument to its C integer, call, and
/// convert the C result back.
///
/// A `Bit` register passes its value, sign-extended to 64 bits when signed;
/// a one-bit `Logic` register passes an `svLogic` (`value | mask << 1`). The
/// callee reads only the low bits of a narrower C type, so every argument is
/// passed as a full register.
pub(super) fn lower_extern_call(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: Option<RegisterId>,
    func: u32,
    args: &[RegisterId],
) {
    let args = args
        .iter()
        .map(|&arg| extern_argument(ctx, block, arg))
        .collect();
    let result = dst.map(|_| ctx.alloc_vreg(SpillDesc::transient()));
    block.push(MInst::CallExtern {
        dst: result,
        func,
        args,
    });
    if let (Some(dst), Some(result)) = (dst, result) {
        define_extern_result(ctx, block, dst, result);
    }
}

/// The 64-bit C integer passed for `arg`.
fn extern_argument(ctx: &mut ISelContext, block: &mut MBlock, arg: RegisterId) -> VReg {
    let value = ctx.reg_map.get(arg);
    match ctx.register_types[&arg] {
        RegisterType::Bit {
            width,
            signed: true,
        } if width > 1 && width < 64 => {
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
            extended
        }
        RegisterType::Bit { .. } => value,
        RegisterType::Logic { .. } if ctx.four_state => {
            let mask = ctx.get_mask(arg, block);
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted,
                src: mask,
                imm: 1,
            });
            let logic = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: logic,
                lhs: value,
                rhs: shifted,
            });
            logic
        }
        RegisterType::Logic { .. } => value,
    }
}

/// Define `dst` from the C integer `result` of an extern call.
fn define_extern_result(ctx: &mut ISelContext, block: &mut MBlock, dst: RegisterId, result: VReg) {
    let value = ctx.reg_map.get(dst);
    match ctx.register_types[&dst] {
        RegisterType::Bit { width, .. } => {
            if width < 64 {
                block.push(MInst::AndImm {
                    dst: value,
                    src: result,
                    imm: (1u64 << width) - 1,
                });
            } else {
                block.push(MInst::Mov {
                    dst: value,
                    src: result,
                });
            }
            if ctx.four_state {
                let mask = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: mask,
                    value: 0,
                });
                ctx.set_mask(dst, mask);
            }
        }
        RegisterType::Logic { .. } => {
            block.push(MInst::AndImm {
                dst: value,
                src: result,
                imm: 1,
            });
            if ctx.four_state {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: result,
                    imm: 1,
                });
                let mask = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::AndImm {
                    dst: mask,
                    src: shifted,
                    imm: 1,
                });
                ctx.set_mask(dst, mask);
            }
        }
    }
}
