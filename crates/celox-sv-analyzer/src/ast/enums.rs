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

/// Resolve an enumerator from declaration bindings. Typedef member contents
/// and numeric values never participate in this lookup.
pub(super) fn constant_binding(
    tree: &super::AnalysisContext<'_>,
    session: &mut super::Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dims: &super::PackedDimensions,
) -> super::Converted<Option<super::semantic_types::ConstantType>> {
    use super::*;
    use celox_sv_syntax::ExprKind;
    let id = expression_binding::ungroup(id, native);
    if let Some(name) = expression_binding::callable_name(tree, id, native) {
        if dims.get(&name).is_some()
            || dims
                .const_env
                .contains_key(&parameters::unbounded_parameter_marker(&name))
        {
            return Ok(None);
        }
        if let Some(binding) = dims.constant_types.get(&name) {
            return Ok(Some(binding.clone()));
        }
        // A local enumerator may hide an outer typedef. If no visible
        // constant binding owns the name, a visible typedef hides imports.
        if dims.type_aliases.contains_key(&name) {
            return Ok(None);
        }
        if let Some(binding) = scope::imported(tree).enum_constants.declarations.get(&name) {
            return Ok(Some(binding.clone()));
        }
        if !name.contains("::")
            && let Some(class) = &dims.class_scope
        {
            return Ok(
                classes::specializations::enum_constants(tree, session, class)
                    .get(&name)
                    .cloned(),
            );
        }
    }
    if let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
        && separator == "::"
        && let Some(class) =
            expression_binding::objects::named_class(tree, session, *expression, native, dims)?
    {
        return Ok(
            classes::specializations::enum_constants(tree, session, &class)
                .get(&instances::normalize_identifier(&name.spelling))
                .cloned(),
        );
    }
    Ok(None)
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
