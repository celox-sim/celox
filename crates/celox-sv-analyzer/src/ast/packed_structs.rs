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

/// The declaration of a packed structure or union type (IEEE 1800-2023
/// 7.2.1, 7.3.1) that each of its members keeps: its nominal identity
/// (6.22.1), retained through aliases, and how `$typename` names it. Two
/// declarations are the same if their identities are.
#[derive(Debug, Clone)]
pub(in crate::ast) struct AggregateDeclaration {
    owner: Arc<str>,
    /// `struct` or `union` of an aggregate other than a soft or tagged
    /// union, which `$typename` writes (20.6.1).
    keyword: Option<&'static str>,
    naming: type_names::Naming,
}

impl AggregateDeclaration {
    /// The declaration `owner`, written with `keyword`, which elaborated
    /// scope `scope` declares.
    pub(in crate::ast) fn new(
        owner: Arc<str>,
        keyword: Option<&'static str>,
        scope: Option<Arc<str>>,
    ) -> Self {
        Self {
            owner,
            keyword,
            naming: type_names::Naming::declared_in(scope),
        }
    }

    /// `struct` or `union`, as `$typename` writes the aggregate.
    pub(in crate::ast) fn keyword(&self) -> Option<&'static str> {
        self.keyword
    }

    /// The typedef that names the aggregate and the scope that declares it.
    pub(in crate::ast) fn naming(&self) -> &type_names::Naming {
        &self.naming
    }

    /// The declaration named `naming`.
    pub(in crate::ast) fn named(&self, naming: type_names::Naming) -> Self {
        Self {
            naming,
            ..self.clone()
        }
    }
}

impl PartialEq for AggregateDeclaration {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
    }
}

impl Eq for AggregateDeclaration {}

impl std::hash::Hash for AggregateDeclaration {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.owner.hash(state);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct PackedMember {
    /// The declaration of the aggregate that has the member.
    owner: Arc<AggregateDeclaration>,
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
        owner: Arc<AggregateDeclaration>,
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

    pub(in crate::ast) fn is_void(&self) -> bool {
        self.is_void
    }

    /// The member's tag index, tag width and payload width in its union.
    pub(in crate::ast) fn tag_info(&self) -> Option<(usize, usize, usize)> {
        self.tag
            .as_ref()
            .map(|tag| (tag.index, tag.tag_width, tag.payload_width))
    }
    /// The member belongs to a packed union, tagged or not.
    pub(in crate::ast) fn in_union(&self) -> bool {
        self.in_union
    }
    /// The identity of the aggregate that has the member.
    pub(in crate::ast) fn owner(&self) -> &Arc<str> {
        &self.owner.owner
    }
    /// The declaration of the aggregate that has the member.
    pub(in crate::ast) fn declaration(&self) -> &Arc<AggregateDeclaration> {
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

/// Name the aggregate that has `members` as `naming` tells.
pub(in crate::ast) fn name_members(members: &mut [PackedMember], naming: &type_names::Naming) {
    let Some(first) = members.first() else {
        return;
    };
    let named = Arc::new(first.owner.named(naming.clone()));
    for member in members {
        debug_assert!(*member.owner == *named);
        member.owner = named.clone();
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
