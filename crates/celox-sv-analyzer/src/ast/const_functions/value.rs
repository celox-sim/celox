//! Values owned by a constant-function frame. Keep ordinary counters inline.

use super::*;

#[derive(Clone)]
pub(super) enum Value {
    Small {
        value: i128,
        width: usize,
        signed: bool,
    },
    Wide {
        literal: Box<typecheck::IntegralLiteral>,
        // Bounds may consume a numerically small value of a wide type. Keep
        // that projection separate from the literal used for typed arithmetic.
        number: Option<i128>,
    },
}

impl Value {
    pub(super) fn integer(value: i128) -> Self {
        Self::Small {
            value,
            width: 128,
            signed: true,
        }
    }

    pub(super) fn from_literal(literal: typecheck::IntegralLiteral) -> Self {
        match typecheck::integral_literal_as_i128(&literal, literal.signed) {
            Some(value) => Self::Small {
                value,
                width: literal.width,
                signed: literal.signed,
            },
            None => {
                let number = if literal.mask != BigUint::default() {
                    None
                } else if literal.signed
                    && literal.width > 0
                    && literal.value.bit((literal.width - 1) as u64)
                {
                    let magnitude = (BigUint::from(1u8) << literal.width) - &literal.value;
                    u128::try_from(magnitude)
                        .ok()
                        .filter(|value| *value <= (1u128 << 127))
                        .map(|value| (value as i128).wrapping_neg())
                } else {
                    i128::try_from(literal.value.clone()).ok()
                };
                Self::Wide {
                    literal: Box::new(literal),
                    number,
                }
            }
        }
    }

    pub(super) fn number(&self) -> Option<&i128> {
        match self {
            Self::Small { value, .. } => Some(value),
            Self::Wide { number, .. } => number.as_ref(),
        }
    }

    pub(super) fn wide(&self) -> Option<&typecheck::IntegralLiteral> {
        match self {
            Self::Wide { literal, .. } => Some(literal),
            Self::Small { .. } => None,
        }
    }

    pub(super) fn truth(&self) -> Option<bool> {
        match self {
            Self::Small { value, .. } => Some(*value != 0),
            Self::Wide { literal, .. } => typecheck::integral_literal_truth(literal),
        }
    }

    pub(super) fn width(&self) -> usize {
        match self {
            Self::Small { width, .. } => *width,
            Self::Wide { literal, .. } => literal.width,
        }
    }

    pub(super) fn r#type(&self) -> (usize, bool) {
        match self {
            Self::Small { width, signed, .. } => (*width, *signed),
            Self::Wide { literal, .. } => (literal.width, literal.signed),
        }
    }

    pub(super) fn into_literal(self) -> typecheck::IntegralLiteral {
        match self {
            Self::Wide { literal, .. } => *literal,
            Self::Small {
                value,
                width,
                signed,
            } => {
                let bits = if width == 128 {
                    value as u128
                } else {
                    (value as u128) & ((1u128 << width) - 1)
                };
                typecheck::IntegralLiteral {
                    width,
                    signed,
                    value: BigUint::from(bits),
                    mask: BigUint::default(),
                }
            }
        }
    }

    pub(super) fn resize(self, width: usize, signed: bool) -> Option<Self> {
        if width > constant_folding::MAX_CONSTANT_CONCAT_BITS {
            return None;
        }
        if let Self::Small { value, .. } = self
            && width <= 128
        {
            return Some(Self::Small {
                value: fit(value, width, signed),
                width,
                signed,
            });
        }
        Some(Self::from_literal(
            typecheck::resize_const_integral_literal(self.into_literal(), width, signed),
        ))
    }

    pub(super) fn into_two_state(self) -> Self {
        match self {
            Self::Small { .. } => self,
            Self::Wide { literal, .. } => {
                let mut literal = *literal;
                let known =
                    ((BigUint::from(1u8) << literal.width) - BigUint::from(1u8)) ^ &literal.mask;
                literal.value &= known;
                literal.mask = BigUint::default();
                Self::from_literal(literal)
            }
        }
    }

    /// Write the in-range bits of a part-select, retaining the destination's type.
    pub(super) fn write(self, high: i128, low: i128, value: Self) -> Option<Self> {
        let width = self.width();
        if high < 0 || low >= width as i128 {
            return Some(self);
        }
        let selected_width = usize::try_from(high.checked_sub(low)?.checked_add(1)?).ok()?;
        let value = value.resize(selected_width, false)?;
        if let (
            Self::Small {
                value: old, signed, ..
            },
            Self::Small { value, .. },
        ) = (&self, &value)
            && low >= 0
            && high < width as i128
        {
            let mask = if selected_width == 128 {
                u128::MAX
            } else {
                (1u128 << selected_width) - 1
            } << low as u32;
            let bits = ((*old as u128) & !mask) | (((*value as u128) << low as u32) & mask);
            return Some(Self::Small {
                value: fit(bits as i128, width, *signed),
                width,
                signed: *signed,
            });
        }
        let value = value.into_literal();
        let start = usize::try_from(low.max(0)).ok()?;
        let end = usize::try_from(high.min(width as i128 - 1)).ok()?;
        let offset = usize::try_from(low.saturating_neg().max(0)).ok()?;
        let selected = ((BigUint::from(1u8) << (end - start + 1)) - BigUint::from(1u8)) << start;
        let keep = ((BigUint::from(1u8) << width) - BigUint::from(1u8)) ^ &selected;
        let mut result = self.into_literal();
        result.value = (result.value & &keep) | (((value.value >> offset) << start) & &selected);
        result.mask = (result.mask & keep) | (((value.mask >> offset) << start) & selected);
        Some(Self::from_literal(result))
    }
}
