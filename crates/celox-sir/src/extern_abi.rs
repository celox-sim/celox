//! How an [`SIRInstruction::ExternCall`](crate::SIRInstruction::ExternCall)
//! passes registers through the C ABI.
//!
//! Every argument and result travels as a 64-bit integer; the callee reads
//! only the low bits of a narrower C type. Each backend emits these rules in
//! its own instructions, and [`ExternValue::encode`] and
//! [`ExternValue::decode`] state them on plain integers.

use crate::RegisterType;

/// The `func` of an extern call to the heap helper.
///
/// Its four arguments are an opcode word and three operands of a
/// `celox_heap::ops` operation. Unlike a user function, backends pass the
/// state pointer before them, and the helper finds the simulator's heap
/// through the state header. The helper's address sits two slots before the
/// user extern function table.
pub const HEAP_HELPER_ID: u32 = u32::MAX - 1;

/// Native-only call to another compiled function in the same image. The low
/// 30 bits index the extern address table. It takes the implicit state pointer,
/// no explicit arguments or result, and propagates a nonzero status to its
/// caller. The callee may read and write arbitrary simulation state.
pub const NATIVE_KERNEL_CALL_TAG: u32 = 0x8000_0000;

/// Decode a native kernel call, excluding the heap and floating-point helpers.
pub const fn native_kernel_index(function: u32) -> Option<u32> {
    if function & 0xc000_0000 == NATIVE_KERNEL_CALL_TAG {
        Some(function & 0x3fff_ffff)
    } else {
        None
    }
}

/// The number of arguments of an extern call to [`HEAP_HELPER_ID`].
pub const HEAP_HELPER_ARGUMENTS: usize = 4;

/// The bit of an `svLogic` that holds the mask: `value | mask << 1`.
pub const SV_LOGIC_MASK_SHIFT: u32 = 1;

/// The C type an extern call passes a register as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternValue {
    /// A `Bit` register of 1, 8, 16, 32 or 64 bits.
    Integer { width: usize, signed: bool },
    /// A one-bit `Logic` register, passed as an `svLogic`.
    Logic,
}

impl ExternValue {
    /// The C type for a register of type `ty`, or `None` when an extern call
    /// cannot pass it.
    pub fn of(ty: &RegisterType) -> Option<Self> {
        match *ty {
            RegisterType::Bit { width, signed } if matches!(width, 1 | 8 | 16 | 32 | 64) => {
                Some(Self::Integer { width, signed })
            }
            RegisterType::Logic { width: 1 } => Some(Self::Logic),
            _ => None,
        }
    }

    /// The C type for a register of a verified extern call.
    ///
    /// # Panics
    ///
    /// When `ty` is not a C integer, which the SIR verifier rejects.
    pub fn of_verified(ty: &RegisterType) -> Self {
        Self::of(ty).expect("the SIR verifier checks extern call register types")
    }

    /// The width an argument is sign-extended from to fill 64 bits, if any.
    ///
    /// A one-bit `svBit` is unsigned in C, so only wider signed integers
    /// narrower than 64 bits are extended; every other argument is passed as
    /// the zero-extended register value.
    pub fn sign_extended_width(self) -> Option<usize> {
        match self {
            Self::Integer {
                width,
                signed: true,
            } if width > 1 && width < 64 => Some(width),
            _ => None,
        }
    }

    /// The bits of a C result that the destination register keeps.
    pub fn result_value_mask(self) -> u64 {
        match self {
            Self::Integer { width: 64, .. } => u64::MAX,
            Self::Integer { width, .. } => (1 << width) - 1,
            Self::Logic => 1,
        }
    }

    /// The 64-bit argument passed for a register holding `value` and, for a
    /// four-state register, `mask`.
    pub fn encode(self, value: u64, mask: u64) -> u64 {
        match self {
            Self::Logic => (value & 1) | (mask & 1) << SV_LOGIC_MASK_SHIFT,
            _ => match self.sign_extended_width() {
                Some(width) => {
                    let shift = 64 - width;
                    (((value << shift) as i64) >> shift) as u64
                }
                None => value,
            },
        }
    }

    /// The value and mask of the destination register for a C result `raw`.
    /// Only an `svLogic` carries a mask; an integer result is always known.
    pub fn decode(self, raw: u64) -> (u64, u64) {
        let value = raw & self.result_value_mask();
        match self {
            Self::Logic => (value, (raw >> SV_LOGIC_MASK_SHIFT) & 1),
            Self::Integer { .. } => (value, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_c_integer_registers() {
        assert_eq!(
            ExternValue::of(&RegisterType::Bit {
                width: 16,
                signed: true
            }),
            Some(ExternValue::Integer {
                width: 16,
                signed: true
            })
        );
        assert_eq!(
            ExternValue::of(&RegisterType::Logic { width: 1 }),
            Some(ExternValue::Logic)
        );
        for ty in [
            RegisterType::Bit {
                width: 7,
                signed: false,
            },
            RegisterType::Logic { width: 8 },
        ] {
            assert_eq!(ExternValue::of(&ty), None);
        }
    }

    #[test]
    fn encodes_arguments() {
        let int = |width, signed| ExternValue::Integer { width, signed };
        assert_eq!(int(8, true).encode(0x80, 0), 0xffff_ffff_ffff_ff80);
        assert_eq!(int(8, false).encode(0x80, 0), 0x80);
        assert_eq!(int(1, true).encode(1, 0), 1);
        assert_eq!(int(64, true).encode(u64::MAX, 0), u64::MAX);
        assert_eq!(ExternValue::Logic.encode(1, 1), 0b11);
        assert_eq!(ExternValue::Logic.encode(0, 1), 0b10);
    }

    #[test]
    fn decodes_results() {
        let int = |width, signed| ExternValue::Integer { width, signed };
        assert_eq!(int(8, true).decode(0xffff_ffff_ffff_ff80), (0x80, 0));
        assert_eq!(int(64, false).decode(u64::MAX), (u64::MAX, 0));
        assert_eq!(int(1, false).decode(0b11), (1, 0));
        assert_eq!(ExternValue::Logic.decode(0b10), (0, 1));
        assert_eq!(ExternValue::Logic.decode(0b111), (1, 1));
    }
}
