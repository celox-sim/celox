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
    /// The values of the members, in declaration order.
    pub fn values(&self) -> Vec<i128> {
        self.members.iter().map(|member| member.value).collect()
    }
}
