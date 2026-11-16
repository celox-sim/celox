//! Floating-point operations.

use super::*;

pub(super) fn float_needs_helper(code: u64) -> bool {
    matches!(code & 255, 4 | 5 | 12 | 13 | 40..=57)
}

pub(super) fn compile_float(
    dst: RegisterId,
    lhs: RegisterId,
    rhs: Option<RegisterId>,
    code: u64,
    externs: &ExternImports,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let op = code & 255;
    let f64 = code & 256 != 0;
    let d = locals.reg_map[&dst].clone();
    instrs.push(Instruction::LocalGet(locals.reg_map[&lhs].value_idx));
    if matches!(code & 255, 34 | 35) {
        if let Some(mask) = locals.reg_map[&lhs].mask_idx {
            instrs.extend([
                Instruction::LocalGet(mask),
                Instruction::I64Const(-1),
                Instruction::I64Xor,
                Instruction::I64And,
            ]);
        }
    }

    if float_needs_helper(code) {
        if let Some(rhs) = rhs {
            instrs.push(Instruction::LocalGet(locals.reg_map[&rhs].value_idx));
        } else {
            instrs.push(Instruction::I64Const(0));
        }
        instrs.push(Instruction::I64Const(code as i64));
        instrs.push(Instruction::Call(
            externs.function_index(celox_sir::float::FLOAT_HELPER_ID),
        ));
    } else if matches!(op, 34 | 35) {
        let width = (code >> 16) as usize;
        if width < 64 {
            if op == 34 {
                instrs.extend([
                    Instruction::I64Const((64 - width) as i64),
                    Instruction::I64Shl,
                    Instruction::I64Const((64 - width) as i64),
                    Instruction::I64ShrS,
                ]);
            } else {
                instrs.extend([
                    Instruction::I64Const(((1u64 << width) - 1) as i64),
                    Instruction::I64And,
                ]);
            }
        }
        instrs.push(match (f64, op == 34) {
            (false, true) => Instruction::F32ConvertI64S,
            (false, false) => Instruction::F32ConvertI64U,
            (true, true) => Instruction::F64ConvertI64S,
            (true, false) => Instruction::F64ConvertI64U,
        });
        if f64 {
            instrs.push(Instruction::I64ReinterpretF64);
        } else {
            instrs.extend([Instruction::I32ReinterpretF32, Instruction::I64ExtendI32U]);
        }
    } else if matches!(op, 36 | 37) {
        compile_float_to_integer(lhs, code, locals, instrs);
    } else {
        if f64 {
            instrs.push(Instruction::F64ReinterpretI64);
        } else {
            instrs.extend([Instruction::I32WrapI64, Instruction::F32ReinterpretI32]);
        }
        if let Some(rhs) = rhs {
            instrs.push(Instruction::LocalGet(locals.reg_map[&rhs].value_idx));
            if f64 {
                instrs.push(Instruction::F64ReinterpretI64);
            } else {
                instrs.extend([Instruction::I32WrapI64, Instruction::F32ReinterpretI32]);
            }
        }
        let instruction = match (f64, op) {
            (false, 0) => Some(Instruction::F32Add),
            (false, 1) => Some(Instruction::F32Sub),
            (false, 2) => Some(Instruction::F32Mul),
            (false, 3) => Some(Instruction::F32Div),
            (true, 0) => Some(Instruction::F64Add),
            (true, 1) => Some(Instruction::F64Sub),
            (true, 2) => Some(Instruction::F64Mul),
            (true, 3) => Some(Instruction::F64Div),
            (false, 6) => Some(Instruction::F32Eq),
            (false, 7) => Some(Instruction::F32Ne),
            (false, 8) => Some(Instruction::F32Lt),
            (false, 9) => Some(Instruction::F32Le),
            (false, 10) => Some(Instruction::F32Gt),
            (false, 11) => Some(Instruction::F32Ge),
            (true, 6) => Some(Instruction::F64Eq),
            (true, 7) => Some(Instruction::F64Ne),
            (true, 8) => Some(Instruction::F64Lt),
            (true, 9) => Some(Instruction::F64Le),
            (true, 10) => Some(Instruction::F64Gt),
            (true, 11) => Some(Instruction::F64Ge),
            (false, 32) => Some(Instruction::F32Neg),
            (true, 32) => Some(Instruction::F64Neg),
            (false, 33) => {
                instrs.push(Instruction::F32Const(0.0.into()));
                Some(Instruction::F32Ne)
            }
            (true, 33) => {
                instrs.push(Instruction::F64Const(0.0.into()));
                Some(Instruction::F64Ne)
            }
            (true, 38) => Some(Instruction::F32DemoteF64),
            (false, 39) => Some(Instruction::F64PromoteF32),
            (false, 38) | (true, 39) => None,
            _ => unreachable!("known floating operation"),
        };
        if let Some(instruction) = instruction {
            instrs.push(instruction);
        }
        if matches!(op, 6..=11 | 33) {
            instrs.push(Instruction::I64ExtendI32U);
        } else if op == 38 || (!f64 && op != 39) {
            instrs.extend([Instruction::I32ReinterpretF32, Instruction::I64ExtendI32U]);
        } else {
            instrs.push(Instruction::I64ReinterpretF64);
        }
    }
    instrs.push(Instruction::LocalSet(d.value_idx));
    if let Some(mask) = d.mask_idx {
        instrs.push(Instruction::I64Const(0));
        instrs.push(Instruction::LocalSet(mask));
    }
}

/// Reduce a rounded floating value modulo 2^64 before converting it. Scaling
/// by a power of two keeps the remainder exact for representable IEEE values;
/// saturating conversion maps the NaN remainder of infinity/NaN to known zero.
pub(super) fn compile_float_to_integer(
    lhs: RegisterId,
    code: u64,
    locals: &mut LocalAllocator,
    instrs: &mut Vec<Instruction<'static>>,
) {
    let abs = locals.alloc(1);
    let truncated = locals.alloc(1);
    let rounded = locals.alloc(1);
    let magnitude = locals.alloc(1);
    let f64 = code & 256 != 0;
    if f64 {
        instrs.push(Instruction::F64ReinterpretI64);
    } else {
        instrs.extend([
            Instruction::I32WrapI64,
            Instruction::F32ReinterpretI32,
            Instruction::F64PromoteF32,
        ]);
    }
    instrs.extend([
        Instruction::F64Abs,
        Instruction::I64ReinterpretF64,
        Instruction::LocalSet(abs),
        Instruction::LocalGet(abs),
        Instruction::F64ReinterpretI64,
        Instruction::F64Trunc,
        Instruction::I64ReinterpretF64,
        Instruction::LocalSet(truncated),
        Instruction::LocalGet(truncated),
        Instruction::F64ReinterpretI64,
    ]);
    if code & 512 != 0 {
        instrs.extend([
            Instruction::LocalGet(abs),
            Instruction::F64ReinterpretI64,
            Instruction::LocalGet(truncated),
            Instruction::F64ReinterpretI64,
            Instruction::F64Sub,
            Instruction::F64Const(0.5.into()),
            Instruction::F64Ge,
            Instruction::F64ConvertI32U,
            Instruction::F64Add,
        ]);
    }
    instrs.extend([
        Instruction::I64ReinterpretF64,
        Instruction::LocalSet(rounded),
        Instruction::LocalGet(rounded),
        Instruction::F64ReinterpretI64,
        Instruction::LocalGet(rounded),
        Instruction::F64ReinterpretI64,
        Instruction::F64Const(18_446_744_073_709_551_616.0.into()),
        Instruction::F64Div,
        Instruction::F64Floor,
        Instruction::F64Const(18_446_744_073_709_551_616.0.into()),
        Instruction::F64Mul,
        Instruction::F64Sub,
        Instruction::I64TruncSatF64U,
        Instruction::LocalSet(magnitude),
        Instruction::I64Const(0),
        Instruction::LocalGet(magnitude),
        Instruction::I64Sub,
        Instruction::LocalGet(magnitude),
        Instruction::LocalGet(locals.reg_map[&lhs].value_idx),
        Instruction::I64Const((1u64 << if f64 { 63 } else { 31 }) as i64),
        Instruction::I64And,
        Instruction::I64Const(0),
        Instruction::I64Ne,
        Instruction::Select,
    ]);
    let width = (code >> 16) as usize;
    if width < 64 {
        instrs.extend([
            Instruction::I64Const(((1u64 << width) - 1) as i64),
            Instruction::I64And,
        ]);
    }
}
