//! Native helper ABI for floating point register operations.
use crate::{FloatBinaryOp as B, FloatPrecision as P, FloatUnaryOp as U};
/// Private backend call target, outside the user extern function table.
pub const FLOAT_HELPER_ID: u32 = u32::MAX;
pub fn binary_code(op: B, p: P) -> u64 {
    let code = match op {
        B::Add => 0,
        B::Sub => 1,
        B::Mul => 2,
        B::Div => 3,
        B::Rem => 4,
        B::Pow => 5,
        B::Eq => 6,
        B::Ne => 7,
        B::Lt => 8,
        B::Le => 9,
        B::Gt => 10,
        B::Ge => 11,
    };
    code | if p == P::F64 { 256 } else { 0 }
}
pub fn unary_code(op: U, p: P) -> u64 {
    let (code, width, round) = match op {
        U::Neg => (32, 0, false),
        U::ToBool => (33, 0, false),
        U::FromSigned { width } => (34, width, false),
        U::FromUnsigned { width } => (35, width, false),
        U::ToSigned { width, round } => (36, width, round),
        U::ToUnsigned { width, round } => (37, width, round),
        U::Convert { to: P::F32 } => (38, 0, false),
        U::Convert { to: P::F64 } => (39, 0, false),
    };
    code | if p == P::F64 { 256 } else { 0 } | ((width as u64) << 16) | ((round as u64) << 9)
}
pub extern "C" fn evaluate(lhs: u64, rhs: u64, code: u64) -> u64 {
    let p = if code & 256 != 0 { P::F64 } else { P::F32 };
    let encoded_width = code >> 16;
    if encoded_width > 64 {
        return 0;
    }
    let width = encoded_width as usize;
    let round = code & 512 != 0;
    let opcode = code & 255;
    let conversion = matches!(opcode, 34..=37);
    if (conversion && !(1..=64).contains(&width))
        || (!conversion && width != 0)
        || (round && !matches!(opcode, 36 | 37))
        || code & 0xfc00 != 0
    {
        return 0;
    }
    let op = match opcode {
        0 => B::Add,
        1 => B::Sub,
        2 => B::Mul,
        3 => B::Div,
        4 => B::Rem,
        5 => B::Pow,
        6 => B::Eq,
        7 => B::Ne,
        8 => B::Lt,
        9 => B::Le,
        10 => B::Gt,
        11 => B::Ge,
        n => {
            let op = match n {
                32 => U::Neg,
                33 => U::ToBool,
                34 => U::FromSigned { width },
                35 => U::FromUnsigned { width },
                36 => U::ToSigned { width, round },
                37 => U::ToUnsigned { width, round },
                38 => U::Convert { to: P::F32 },
                39 => U::Convert { to: P::F64 },
                _ => return 0,
            };
            return celox_design::eval_float_unary(op, p, lhs);
        }
    };
    celox_design::eval_float_binary(op, p, lhs, rhs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_encoding_preserves_precision_rounding_and_width() {
        for p in [P::F32, P::F64] {
            let bits = if p == P::F32 {
                (-1.5f32).to_bits() as u64
            } else {
                (-1.5f64).to_bits()
            };
            for width in [8, 32, 64] {
                let op = U::ToUnsigned { width, round: true };
                assert_eq!(
                    evaluate(bits, 0, unary_code(op, p)),
                    celox_design::eval_float_unary(op, p, bits)
                );
            }
        }
        assert_eq!(
            evaluate(1f64.to_bits(), 2f64.to_bits(), binary_code(B::Add, P::F64)),
            3f64.to_bits()
        );
    }
    #[test]
    fn malformed_helper_encodings_return_zero() {
        for width in [0, 65, usize::MAX] {
            for op in [
                U::FromSigned { width },
                U::FromUnsigned { width },
                U::ToSigned { width, round: true },
                U::ToUnsigned {
                    width,
                    round: false,
                },
            ] {
                assert_eq!(evaluate(u64::MAX, u64::MAX, unary_code(op, P::F64)), 0);
            }
        }
        for code in [
            u64::MAX,
            255,
            0x400,
            32 | (1 << 16),
            32 | 512,
            ((1u64 << 32 | 1) << 16) | 34,
        ] {
            assert_eq!(evaluate(u64::MAX, u64::MAX, code), 0);
        }
    }
}
