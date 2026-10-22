//! IEEE floating point operations on raw two-state register payloads.
use crate::{FloatBinaryOp as B, FloatPrecision as P, FloatUnaryOp as U};

pub fn eval_float_binary(op: B, precision: P, lhs: u64, rhs: u64) -> u64 {
    macro_rules! eval {
        ($ty:ty, $lhs:expr, $rhs:expr) => {{
            let a = $lhs;
            let b = $rhs;
            match op {
                B::Add => (a + b).to_bits() as u64,
                B::Sub => (a - b).to_bits() as u64,
                B::Mul => (a * b).to_bits() as u64,
                B::Div => (a / b).to_bits() as u64,
                B::Rem => (a % b).to_bits() as u64,
                B::Pow => a.powf(b).to_bits() as u64,
                B::Atan2 => a.atan2(b).to_bits() as u64,
                B::Hypot => a.hypot(b).to_bits() as u64,
                B::Eq => (a == b) as u64,
                B::Ne => (a != b) as u64,
                B::Lt => (a < b) as u64,
                B::Le => (a <= b) as u64,
                B::Gt => (a > b) as u64,
                B::Ge => (a >= b) as u64,
            }
        }};
    }
    match precision {
        P::F32 => eval!(f32, f32::from_bits(lhs as u32), f32::from_bits(rhs as u32)),
        P::F64 => eval!(f64, f64::from_bits(lhs), f64::from_bits(rhs)),
    }
}

pub fn eval_float_unary(op: U, precision: P, value: u64) -> u64 {
    let x = match precision {
        P::F32 => f32::from_bits(value as u32) as f64,
        P::F64 => f64::from_bits(value),
    };
    let mask = |width: usize| {
        if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        }
    };
    match op {
        U::Neg => value ^ (1u64 << (precision.width() - 1)),
        U::ToBool => (x != 0.0) as u64,
        U::FromSigned { width } => {
            let signed = if width == 0 {
                0
            } else if width >= 64 {
                value as i64
            } else {
                ((value << (64 - width)) as i64) >> (64 - width)
            };
            // Convert directly to f32 to avoid double rounding through f64.
            match precision {
                P::F32 => (signed as f32).to_bits() as u64,
                P::F64 => (signed as f64).to_bits(),
            }
        }
        U::FromUnsigned { width } => {
            let v = value & mask(width);
            match precision {
                P::F32 => (v as f32).to_bits() as u64,
                P::F64 => (v as f64).to_bits(),
            }
        }
        U::ToSigned { width, round } | U::ToUnsigned { width, round } => {
            // Integer destinations keep the low bits, independently of signedness.
            // Non-finite values have no integer representation; use known zero.
            let rounded = if round { x.round() } else { x.trunc() };
            let magnitude = (rounded.abs() % 18_446_744_073_709_551_616.0) as u64;
            let bits = if rounded.is_sign_negative() {
                magnitude.wrapping_neg()
            } else {
                magnitude
            };
            bits & mask(width)
        }
        U::Convert { to: P::F32 } => (x as f32).to_bits() as u64,
        U::Convert { to: P::F64 } => x.to_bits(),
        U::Ln => {
            let value = x.ln();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Log10 => {
            let value = x.log10();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Exp => {
            let value = x.exp();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Sqrt => {
            let value = x.sqrt();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Floor => {
            let value = x.floor();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Ceil => {
            let value = x.ceil();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Sin => {
            let value = x.sin();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Cos => {
            let value = x.cos();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Tan => {
            let value = x.tan();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Asin => {
            let value = x.asin();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Acos => {
            let value = x.acos();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Atan => {
            let value = x.atan();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Sinh => {
            let value = x.sinh();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Cosh => {
            let value = x.cosh();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Tanh => {
            let value = x.tanh();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Asinh => {
            let value = x.asinh();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Acosh => {
            let value = x.acosh();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
        U::Atanh => {
            let value = x.atanh();
            match precision {
                P::F32 => (value as f32).to_bits() as u64,
                P::F64 => value.to_bits(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ieee_arithmetic_and_comparisons() {
        for p in [P::F32, P::F64] {
            let bits = |x: f64| {
                if p == P::F32 {
                    (x as f32).to_bits() as u64
                } else {
                    x.to_bits()
                }
            };
            assert_eq!(
                eval_float_binary(B::Add, p, bits(1.25), bits(2.5)),
                bits(3.75)
            );
            assert_eq!(
                eval_float_binary(B::Div, p, bits(1.0), bits(0.0)),
                bits(f64::INFINITY)
            );
            assert_eq!(eval_float_binary(B::Eq, p, bits(0.0), bits(-0.0)), 1);
            assert_eq!(
                eval_float_binary(B::Eq, p, bits(f64::NAN), bits(f64::NAN)),
                0
            );
            assert_eq!(eval_float_binary(B::Ne, p, bits(f64::NAN), bits(1.0)), 1);
            assert_eq!(eval_float_binary(B::Lt, p, bits(f64::NAN), bits(1.0)), 0);
            assert_eq!(eval_float_unary(U::Neg, p, bits(0.0)), bits(-0.0));
            assert_eq!(eval_float_unary(U::ToBool, p, bits(-0.0)), 0);
        }
    }
    #[test]
    fn real_integer_rounding_and_bit_truncation() {
        for (x, expected) in [
            (1.5f64, 2u64),
            (-1.5, 0xffff_fffe),
            (35.2, 35),
            (35.7, 36),
            (-0.5, 0xffff_ffff),
        ] {
            for signed in [true, false] {
                let op = if signed {
                    U::ToSigned {
                        width: 32,
                        round: true,
                    }
                } else {
                    U::ToUnsigned {
                        width: 32,
                        round: true,
                    }
                };
                assert_eq!(eval_float_unary(op, P::F64, x.to_bits()), expected);
            }
        }
        assert_eq!(
            eval_float_unary(
                U::ToUnsigned {
                    width: 64,
                    round: true
                },
                P::F64,
                9_223_372_036_854_775_808f64.to_bits()
            ),
            1u64 << 63
        );
        assert_eq!(
            eval_float_unary(
                U::ToSigned {
                    width: 8,
                    round: false
                },
                P::F64,
                (-1.75f64).to_bits()
            ),
            255
        );
        assert_eq!(
            eval_float_unary(U::FromSigned { width: 8 }, P::F32, 255),
            (-1f32).to_bits() as u64
        );
    }
}
