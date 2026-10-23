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
        B::Atan2 => 12,
        B::Hypot => 13,
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
        U::Ln => (40, 0, false),
        U::Log10 => (41, 0, false),
        U::Exp => (42, 0, false),
        U::Sqrt => (43, 0, false),
        U::Floor => (44, 0, false),
        U::Ceil => (45, 0, false),
        U::Sin => (46, 0, false),
        U::Cos => (47, 0, false),
        U::Tan => (48, 0, false),
        U::Asin => (49, 0, false),
        U::Acos => (50, 0, false),
        U::Atan => (51, 0, false),
        U::Sinh => (52, 0, false),
        U::Cosh => (53, 0, false),
        U::Tanh => (54, 0, false),
        U::Asinh => (55, 0, false),
        U::Acosh => (56, 0, false),
        U::Atanh => (57, 0, false),
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
        12 => B::Atan2,
        13 => B::Hypot,
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
                40 => U::Ln,
                41 => U::Log10,
                42 => U::Exp,
                43 => U::Sqrt,
                44 => U::Floor,
                45 => U::Ceil,
                46 => U::Sin,
                47 => U::Cos,
                48 => U::Tan,
                49 => U::Asin,
                50 => U::Acos,
                51 => U::Atan,
                52 => U::Sinh,
                53 => U::Cosh,
                54 => U::Tanh,
                55 => U::Asinh,
                56 => U::Acosh,
                57 => U::Atanh,

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
    fn real_math_helper_codes_roundtrip_and_reject_reserved_fields() {
        for p in [P::F32, P::F64] {
            let bits = |x: f64| {
                if p == P::F64 {
                    x.to_bits()
                } else {
                    (x as f32).to_bits() as u64
                }
            };
            for op in [
                U::Ln,
                U::Log10,
                U::Exp,
                U::Sqrt,
                U::Floor,
                U::Ceil,
                U::Sin,
                U::Cos,
                U::Tan,
                U::Asin,
                U::Acos,
                U::Atan,
                U::Sinh,
                U::Cosh,
                U::Tanh,
                U::Asinh,
                U::Acosh,
                U::Atanh,
            ] {
                let code = unary_code(op, p);
                let actual = evaluate(bits(0.5), 0, code);
                let expected = celox_design::eval_float_unary(op, p, bits(0.5));
                assert_eq!(actual, expected, "{op:?} {p:?}");
                assert_eq!(evaluate(bits(0.5), 0, code | 512), 0);
                assert_eq!(evaluate(bits(0.5), 0, code | (1 << 16)), 0);
            }
            for op in [B::Atan2, B::Hypot, B::Pow] {
                assert_eq!(
                    evaluate(bits(0.5), bits(2.0), binary_code(op, p)),
                    celox_design::eval_float_binary(op, p, bits(0.5), bits(2.0))
                );
            }
        }
        for invalid in [14, 31, 58, 63] {
            assert_eq!(evaluate(0, 0, invalid), 0);
        }
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
