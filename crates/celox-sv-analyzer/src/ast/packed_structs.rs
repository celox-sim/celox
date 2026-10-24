//! Packed structures (IEEE 1800-2023 7.2.1).

use super::*;

mod layout;
mod selects;
mod tagged;
pub(in crate::ast) use tagged::{
    Construction, checked_value, construct, construction_member, member_access,
};
#[cfg(test)]
mod tests;

pub(super) use layout::{AggregateKind, native_type};
pub(super) use selects::{select_member, whole_member};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PackedMember {
    /// Nominal aggregate identity (IEEE 1800-2023 6.22.1), retained through aliases.
    owner: Arc<str>,
    name: String,
    offset: usize,
    tag: Option<TaggedMember>,
    is_void: bool,
    r#type: Type,
}

impl PackedMember {
    pub(in crate::ast) fn is_tagged(&self) -> bool {
        self.tag.is_some()
    }
    pub(in crate::ast) fn same_owner(&self, other: &Self) -> bool {
        self.owner == other.owner
    }
    /// This member with its type replaced by `map` of it.
    pub(in crate::ast) fn with_type(mut self, map: impl FnOnce(Type) -> Type) -> Self {
        self.r#type = map(self.r#type);
        self
    }

    pub(in crate::ast) fn offset(&self) -> usize {
        self.offset
    }

    pub(in crate::ast) fn name(&self) -> &str {
        &self.name
    }

    pub(in crate::ast) fn r#type(&self) -> &Type {
        &self.r#type
    }
}

/// IEEE 1800-2023 7.3.2: tag bits precede a right-justified payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::ast) struct TaggedMember {
    index: usize,
    tag_width: usize,
    payload_width: usize,
}
