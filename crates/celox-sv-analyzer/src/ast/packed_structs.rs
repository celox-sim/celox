//! Packed structures (IEEE 1800-2023 7.2.1).

use super::*;

mod layout;
mod selects;
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
    r#type: Type,
}

impl PackedMember {
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
