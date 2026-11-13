//! Packed structures (IEEE 1800-2023 7.2.1).

use super::*;

mod layout;
mod selects;
mod tagged;
pub(in crate::ast) use tagged::{
    Construction, checked_typed_value, checked_value, construct, construction_member,
    member_access, member_guards,
};
#[cfg(test)]
mod tests;

pub(super) use layout::{AggregateKind, capture_member_type, native_type};
pub(super) use selects::{
    bind_interface_path, element_member, member_view, select_element_member, select_member,
    select_member_element_member, whole_member,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PackedMember {
    /// Nominal aggregate identity (IEEE 1800-2023 6.22.1), retained through aliases.
    owner: Arc<str>,
    name: String,
    offset: usize,
    tag: Option<TaggedMember>,
    is_void: bool,
    /// The member belongs to a packed union and shares its bits with the
    /// other members (IEEE 1800-2023 7.3.1).
    in_union: bool,
    r#type: Type,
}

impl generate::CallSites for PackedMember {
    fn call_sites(&self, visit: &mut dyn FnMut(usize)) {
        let PackedMember {
            owner: _,
            name: _,
            offset: _,
            tag: _,
            is_void: _,
            in_union: _,
            r#type,
        } = self;
        r#type.call_sites(visit);
    }
}

impl PackedMember {
    /// A bit window in the flat storage of an unpacked structure array.
    pub(in crate::ast) fn storage_member(
        owner: Arc<str>,
        name: String,
        offset: usize,
        r#type: Type,
    ) -> Self {
        Self {
            owner,
            name,
            offset,
            r#type,
            tag: None,
            is_void: false,
            in_union: false,
        }
    }

    pub(in crate::ast) fn is_tagged(&self) -> bool {
        self.tag.is_some()
    }
    /// The member belongs to a packed union, tagged or not.
    pub(in crate::ast) fn in_union(&self) -> bool {
        self.in_union
    }
    /// The declaration of the aggregate that has the member.
    pub(in crate::ast) fn owner(&self) -> &Arc<str> {
        &self.owner
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

/// The members of a packed structure that a constant member select can
/// name, with the members of their members: each with its path (`a.b`), the
/// offset of its least significant bit in the structure and its type
/// (IEEE 1800-2023 7.2.1). Void members and the members of tagged unions,
/// whose select depends on the tag, are left out.
pub(in crate::ast) fn constant_members(
    functions: &const_functions::ConstantFunctions,
    members: &[PackedMember],
    const_env: &ConstEnv,
) -> Vec<(String, usize, ExprType)> {
    let mut found = Vec::new();
    let mut pending = vec![(String::new(), 0usize, members)];
    while let Some((prefix, base, members)) = pending.pop() {
        for member in members {
            if member.is_void || member.tag.is_some() {
                continue;
            }
            let Some(offset) = base.checked_add(member.offset) else {
                continue;
            };
            let Some(r#type) = expr_type_from_type(functions, &member.r#type, const_env) else {
                continue;
            };
            let path = format!("{prefix}{}", member.name);
            if member.r#type.packed_ranges.len() == 1 && !member.r#type.members.is_empty() {
                pending.push((format!("{path}."), offset, &member.r#type.members));
            }
            found.push((path, offset, r#type));
        }
    }
    found
}

/// IEEE 1800-2023 7.3.2: tag bits precede a right-justified payload.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::ast) struct TaggedMember {
    index: usize,
    tag_width: usize,
    payload_width: usize,
}
