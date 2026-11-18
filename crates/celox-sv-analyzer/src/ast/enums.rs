//! Enumerated types (IEEE 1800-2023 6.19) and their methods (6.19.5).

/// One named member of an enumerated type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EnumMember {
    pub name: String,
    pub value: i128,
}

/// The members of an enumerated type in declaration order, with the shape
/// of its base type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct EnumDomain {
    pub members: Vec<EnumMember>,
    pub width: usize,
    pub signed: bool,
    /// Whether the base type is 4-state, whose default initial value is
    /// `'x` rather than `'0` (Table 6-7).
    pub four_state: bool,
}

impl EnumDomain {
    /// The values of the members, in declaration order, as their base type
    /// interprets them.
    pub fn values(&self) -> Vec<MemberValue> {
        self.members
            .iter()
            .map(|member| self.value_of(member))
            .collect()
    }

    /// The value of `member` as the base type interprets its bits (6.19):
    /// its `width` bits, sign-extended when the base type is signed and
    /// zero-extended otherwise. A member's stored `value` holds those bits
    /// in two's complement, so an unsigned 128-bit member with its top bit
    /// set is stored as a negative `i128`; this is its value.
    pub fn value_of(&self, member: &EnumMember) -> MemberValue {
        let width = self.width.clamp(1, 128);
        let mask = if width == 128 {
            u128::MAX
        } else {
            (1u128 << width) - 1
        };
        let bits = (member.value as u128) & mask;
        if self.signed && (bits >> (width - 1)) & 1 == 1 {
            // Sign-extend the base width's bits.
            MemberValue::Negative((bits | !mask) as i128)
        } else {
            MemberValue::NonNegative(bits)
        }
    }
}

/// The value of an enum member as its base type interprets it: negative only
/// for a signed base type, and up to `2^128 - 1` for an unsigned one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum MemberValue {
    Negative(i128),
    NonNegative(u128),
}

impl MemberValue {
    /// The value's bits in two's complement, as many as a `u128` holds.
    pub fn bits(self) -> u128 {
        match self {
            Self::Negative(value) => value as u128,
            Self::NonNegative(value) => value,
        }
    }

    /// Whether an integral type of `width` bits and signing `signed`
    /// represents this value.
    pub fn fits(self, width: usize, signed: bool) -> bool {
        match (self, signed) {
            (Self::Negative(_), false) => false,
            (Self::Negative(value), true) => width >= 128 || value >= -(1i128 << (width - 1)),
            (Self::NonNegative(value), false) => width >= 128 || value < 1u128 << width,
            (Self::NonNegative(value), true) => width > 128 || value < 1u128 << (width - 1),
        }
    }

    /// A literal of `width` bits and signing `signed` with this value: its
    /// two's complement at that width.
    pub fn literal(self, width: usize, signed: bool) -> String {
        let signing = if signed { "s" } else { "" };
        match self {
            Self::NonNegative(value) => {
                // Truncated to the width, as an assignment to it would.
                let value = if width < 128 {
                    value & ((1u128 << width) - 1)
                } else {
                    value
                };
                format!("{width}'{signing}h{value:x}")
            }
            Self::Negative(value) => {
                super::parameters::format_typed_parameter_literal(value, width, signed)
            }
        }
    }
}

impl std::fmt::Display for MemberValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Negative(value) => write!(formatter, "{value}"),
            Self::NonNegative(value) => write!(formatter, "{value}"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn domain(width: usize, signed: bool, values: &[i128]) -> EnumDomain {
        EnumDomain {
            members: values
                .iter()
                .enumerate()
                .map(|(index, &value)| EnumMember {
                    name: format!("M{index}"),
                    value,
                })
                .collect(),
            width,
            signed,
            four_state: true,
        }
    }

    #[test]
    fn members_have_the_value_their_base_type_gives_their_bits() {
        // 6.19: an unsigned 128-bit member with its top bit set, stored as a
        // negative i128, is a large nonnegative value; a signed one is
        // negative; a narrower signed base sign-extends from its width.
        let top = i128::MIN;
        let unsigned = domain(128, false, &[0, top]);
        assert_eq!(
            unsigned.value_of(&unsigned.members[1]),
            MemberValue::NonNegative(1u128 << 127)
        );
        let signed = domain(128, true, &[top]);
        assert_eq!(
            signed.value_of(&signed.members[0]),
            MemberValue::Negative(top)
        );
        let narrow = domain(4, true, &[0b1110]);
        assert_eq!(
            narrow.value_of(&narrow.members[0]),
            MemberValue::Negative(-2)
        );
        let narrow = domain(4, false, &[-2]);
        assert_eq!(
            narrow.value_of(&narrow.members[0]),
            MemberValue::NonNegative(0b1110)
        );
    }

    #[test]
    fn member_values_fit_and_format_by_the_comparing_type() {
        let high = MemberValue::NonNegative(1u128 << 127);
        assert!(high.fits(128, false));
        assert!(!high.fits(128, true));
        assert!(high.fits(129, true));
        assert!(!high.fits(127, false));
        let minus = MemberValue::Negative(-2);
        assert!(!minus.fits(64, false));
        assert!(minus.fits(2, true));
        assert!(!minus.fits(1, true));
        assert_eq!(
            high.literal(128, false),
            "128'h80000000000000000000000000000000"
        );
        assert_eq!(MemberValue::NonNegative(0x1ff).literal(8, false), "8'hff");
        assert_eq!(minus.literal(8, true), "8'sd254");
        assert_eq!(high.to_string(), "170141183460469231731687303715884105728");
        assert_eq!(minus.bits(), u128::MAX - 1);
    }
}

/// What a type is of an enumeration (IEEE 1800-2023 6.19), as its semantic
/// type tells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Enumeration {
    /// The type is an enumerated type or a packed array of one.
    pub(super) enumeration: bool,
    /// The type is a packed array of an enumerated type, whose elements'
    /// methods are not supported.
    pub(super) packed: bool,
    /// The members of an enumerated type that is no array, if resolved.
    pub(super) domain: Option<std::sync::Arc<EnumDomain>>,
}

impl Enumeration {
    /// The type is an enumeration whose members are not resolved, or a
    /// packed array of one, whose values are not supported where its
    /// members are needed.
    pub(super) fn unresolved(&self) -> bool {
        self.enumeration && self.domain.is_none()
    }

    /// The enumeration of a type that has no semantic type yet but keeps
    /// the enumerated type `declaration` declares, with `packed` packed
    /// dimensions, and whose conversion input fields tell `fields`.
    pub(super) fn kept(
        declaration: &super::semantic_types::EnumDeclaration,
        packed: usize,
        fields: Self,
    ) -> Self {
        let array = packed > declaration.dims;
        let facts = Self {
            enumeration: true,
            packed: array,
            domain: declaration.domain.clone().filter(|_| !array),
        };
        debug_assert_eq!(facts, fields, "the enumeration a type keeps");
        facts
    }

    /// The enumeration of a type, or of the elements of a fixed-size
    /// unpacked array of it, whose semantic type in `table` is `id`, and
    /// whose conversion input fields tell `fields`.
    pub(super) fn of(
        id: Option<super::semantic_types::TyId>,
        table: &super::semantic_types::TyTable,
        fields: Self,
    ) -> Self {
        let Some(id) = id else {
            debug_assert_eq!(
                fields,
                Self::default(),
                "an enumeration type is told by its semantic type"
            );
            return Self::default();
        };
        let id = table.fixed_element(id);
        match table.enumeration(id) {
            Some(array) => {
                let facts = Self {
                    enumeration: true,
                    packed: array,
                    domain: table.enum_domain(id).filter(|_| !array).cloned(),
                };
                debug_assert_eq!(facts, fields, "the semantic type of an enumeration type");
                facts
            }
            // An anonymous enumeration whose members are not resolved, or a
            // packed array of one, converts to bits: nothing identifies it
            // until anonymous enumerations are told by their declarations.
            None if fields.enumeration && table.is_integral(id) => {
                debug_assert!(fields.domain.is_none());
                fields
            }
            None => {
                debug_assert_eq!(
                    fields,
                    Self::default(),
                    "the semantic type of a type that is no enumeration"
                );
                Self::default()
            }
        }
    }
}
