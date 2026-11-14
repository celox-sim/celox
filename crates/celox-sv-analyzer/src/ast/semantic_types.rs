//! Semantic data types (IEEE 1800-2023 6.22): each type is a tree of
//! [`TyData`] interned in a [`TyTable`], so that a [`TyId`] names it and the
//! type compatibility relations compare ids instead of reconstructing types
//! from syntax.
//!
//! Ranges are evaluated integers. Named types (enumerations, structures,
//! unions, classes, interfaces) carry the identity of their declaration; a
//! simple typedef is not a type of its own (6.22.1 b), so it interns to the
//! type it renames.

use fxhash::FxHashMap as HashMap;
use std::sync::{Arc, Weak};

mod bindings;
mod conversion;
mod relations;
mod spelling;
mod streams;
#[cfg(test)]
mod tests;

pub(super) use bindings::ConstantType;
pub(super) use conversion::{bind_declared, bound_builtin, of_builtin, of_shape, of_type};

/// The bounds of the packed dimensions of `ty`, outermost first, as the
/// type table resolves them with the constants of `env` (an element's, for
/// a fixed-size unpacked array); none for a type without packed bits.
/// `None` when `ty` has no semantic type.
pub(super) fn packed_bounds(
    table: &mut TyTable,
    functions: &crate::ast::const_functions::ConstantFunctions,
    ty: &crate::ast::Type,
    env: &crate::ast::ConstEnv,
) -> Option<Vec<(i128, i128)>> {
    let mut id = of_type(table, functions, ty, env)?;
    loop {
        match table.data(id) {
            TyData::Packed(packed) => {
                return Some(
                    packed
                        .dims
                        .iter()
                        .map(|range| (range.left, range.right))
                        .collect(),
                );
            }
            TyData::Array {
                kind: ArrayKind::Fixed(_),
                element,
            } => id = *element,
            TyData::Array { .. } => return None,
            _ => return Some(Vec::new()),
        }
    }
}
pub(super) use relations::{Assignable, Castable, Mismatch};
pub(super) use streams::{StreamLayout, StreamRole};

/// An interned semantic type of a [`TyTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct TyId(u32);

/// The bounds of one dimension, `[left:right]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Range {
    pub left: i128,
    pub right: i128,
}

impl Range {
    /// How many elements or bits the dimension has.
    pub fn size(self) -> u128 {
        self.left.abs_diff(self.right) + 1
    }
}

/// A declaration of a named enumeration, structure or union type, as the
/// table interns it ([`TyTable::declare`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct DeclId(u32);

/// What tells a declaration apart: its typedef or its source position, and
/// the elaborated scope that declares it. Each instance of a module,
/// interface or program declares types of its own (IEEE 1800-2023 6.22);
/// instances with the same parameter values share one analysis and so one
/// scope here.
#[derive(Debug, Clone)]
pub(super) struct DeclData {
    pub name: Arc<str>,
    pub scope: Option<Arc<str>>,
    /// The members of an enumeration the declaration declares (6.19),
    /// once a type of it has been converted with them.
    pub domain: Option<Arc<super::enums::EnumDomain>>,
}

/// The identity of the declaration of a named type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Decl {
    /// An enumeration declared by a typedef.
    Named(DeclId),
    /// An enumeration whose declaration is not recorded, identified by its
    /// members.
    AnonymousEnum(Arc<super::enums::EnumDomain>),
    /// A structure or union declaration, identified by its typedef or its
    /// source position together with the types of its members as its scope
    /// gives them (7.2, 7.3): each specialization of a parameterized scope
    /// declares a type of its own.
    Aggregate {
        decl: DeclId,
        members: Arc<[(Arc<str>, TyId)]>,
    },
    /// A class, or a specialization of a parameterized class.
    Class(Arc<str>),
    /// An interface.
    Interface(Arc<str>),
}

/// What the bits of a packed type are made of.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum PackedElement {
    /// Single bits: a simple bit vector or an integer type.
    Bits,
    /// Elements of an enumerated type (6.19): the type itself, or a packed
    /// array of it if `array`.
    Enum { decl: Decl, array: bool },
    /// Elements of a packed structure or union type (7.2.1, 7.3.1).
    Aggregate(Decl),
}

/// A packed type (6.11, 7.4.1): its packed dimensions, outermost first,
/// including those of a named element type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Packed {
    pub element: PackedElement,
    pub four_state: bool,
    pub signed: bool,
    /// How many leading dimensions select an element of a signed named type,
    /// which is signed although the whole array is not (7.4.1).
    pub signed_element_depth: Option<usize>,
    pub dims: Box<[Range]>,
    /// The integer type with a predefined width (6.11) that declared the
    /// type, which `$typename` tells apart from a vector of its width
    /// (20.6.1) but type matching does not (6.22.1 e).
    pub keyword: Option<&'static str>,
}

impl Packed {
    /// How many bits the type has.
    pub fn bits(&self) -> u128 {
        self.dims.iter().map(|range| range.size()).product()
    }
}

/// The kind of the slowest-varying dimension of an unpacked array (7.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ArrayKind {
    Fixed(Range),
    Dynamic,
    /// A queue, bounded to `bound + 1` elements if `bound` is given (7.10).
    Queue {
        bound: Option<u32>,
    },
    Associative {
        key: TyId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum TyData {
    Packed(Packed),
    Real,
    Shortreal,
    String,
    /// The null literal, before conversion to a handle (6.11.1, 11.4.11).
    Null,
    /// A `chandle`, which only another chandle or `null` is assigned to
    /// (6.14).
    Chandle,
    /// A handle of a class object (8.4).
    Class(Decl),
    /// A virtual interface, with a modport if one is selected (25.9).
    VirtualInterface {
        interface: Decl,
        modport: Option<Arc<str>>,
    },
    /// An unpacked structure (7.2).
    UnpackedStruct(Decl),
    /// An unpacked array of `element` (7.4).
    Array {
        kind: ArrayKind,
        element: TyId,
    },
}

/// The interned types of an analysis.
#[derive(Clone, Debug, Default)]
pub(super) struct TyTable {
    /// Each type with the id of the type it matches in canonical form.
    types: Vec<(Arc<TyData>, TyId)>,
    ids: HashMap<Arc<TyData>, TyId>,
    /// The base class and the interface classes of each class whose
    /// declaration has been resolved (8.13, 8.26).
    supertypes: HashMap<TyId, Supertypes>,
    /// The type of each variable shape whose bounds are all literals, by
    /// the hash of the shape: such a shape has the same type in every
    /// context (see `VariableShape::is_closed`).
    closed_shapes: HashMap<u64, Vec<(crate::ast::variable_dimensions::VariableShape, TyId)>>,
    /// Whether a `chandle` type has been interned (6.14).
    chandle: bool,
    /// The declarations the types of the table name.
    decls: Vec<DeclData>,
    /// The declarations by name, then by scope (`""` for none, which no
    /// elaborated scope is called): a lookup allocates nothing, as types
    /// are converted at every reference.
    decl_ids: HashMap<Arc<str>, HashMap<Arc<str>, DeclId>>,
    /// How many times a reader resolved a type that was not bound where it
    /// was declared, which tests check producers against.
    #[cfg(test)]
    pub(super) unbound_reads: usize,
}

/// The classes a class extends and implements.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Supertypes {
    /// The base class (8.13).
    pub base: Option<TyId>,
    /// The interface classes the class implements, or that an interface
    /// class extends (8.26).
    pub interfaces: Arc<[TyId]>,
    /// Whether the class is an interface class (8.26).
    pub interface: bool,
}

/// The semantic type a [`crate::ast::Type`] was bound to, kept until the
/// type changes: the one the scope that declares it tells (see
/// [`bind_declared`]). Whoever builds a type binds it; a copy of the type
/// keeps the binding, and any change of its shape forgets it.
#[derive(Debug, Clone, Default)]
pub(super) struct Binding(Option<RetainedType>);

impl Binding {
    /// Forget the semantic type, as the type changes.
    pub(super) fn forget(&mut self) {
        self.0 = None;
    }

    /// The semantic type in `table`, if bound to one of its types.
    pub(super) fn get(&self, table: &TyTable) -> Option<TyId> {
        self.0.as_ref()?.in_table(table)
    }

    /// Bind to type `id` of `table`.
    pub(super) fn set(&mut self, table: &TyTable, id: TyId) {
        self.0 = Some(table.retain(id));
    }
}

/// A declaration's ID together with its immutable interned node. A cloned
/// table shares its prefix; unrelated or independently appended nodes do not.
#[derive(Clone, Debug)]
pub(super) struct RetainedType {
    id: TyId,
    node: Weak<TyData>,
}
impl RetainedType {
    pub(super) fn same_node(&self, other: &Self) -> bool {
        self.id == other.id && Weak::ptr_eq(&self.node, &other.node)
    }

    pub(super) fn in_table(&self, table: &TyTable) -> Option<TyId> {
        table
            .types
            .get(self.id.0 as usize)
            .filter(|(node, _)| std::ptr::eq(Arc::as_ptr(node), self.node.as_ptr()))
            .map(|_| self.id)
    }
}

impl TyTable {
    /// The type of member `name` of a value of structure type `aggregate`,
    /// packed or unpacked (7.2): a packed structure only when it is one
    /// structure, not a packed array of them.
    pub fn member(&self, aggregate: TyId, name: &str) -> Option<TyId> {
        let members = match self.data(aggregate) {
            TyData::UnpackedStruct(Decl::Aggregate { members, .. }) => members,
            TyData::Packed(Packed {
                element: PackedElement::Aggregate(Decl::Aggregate { members, .. }),
                dims,
                ..
            }) if dims.len() == 1 => members,
            _ => return None,
        };
        members
            .iter()
            .find(|(member, _)| member.as_ref() == name)
            .map(|(_, ty)| *ty)
    }

    /// Records that class `class` extends or implements `supertypes`.
    pub fn record_supertypes(&mut self, class: TyId, supertypes: Supertypes) {
        self.supertypes.insert(class, supertypes);
    }

    /// The base class and interface classes recorded of class `class`.
    pub fn supertypes(&self, class: TyId) -> Option<&Supertypes> {
        self.supertypes.get(&class)
    }

    /// Class `class` and its base classes, nearest first, as far as their
    /// supertypes are recorded.
    pub fn base_classes(&self, class: TyId) -> Vec<TyId> {
        let mut classes = vec![class];
        while let Some(base) = self
            .supertypes(*classes.last().unwrap())
            .and_then(|supertypes| supertypes.base)
            && !classes.contains(&base)
        {
            classes.push(base);
        }
        classes
    }

    /// Class `class` and every class it derives from or implements as far as
    /// their supertypes are recorded.
    pub fn lineage(&self, class: TyId) -> Vec<TyId> {
        let mut lineage = vec![class];
        let mut next = 0;
        while let Some(&class) = lineage.get(next) {
            next += 1;
            let Some(supertypes) = self.supertypes(class) else {
                continue;
            };
            for &supertype in supertypes.base.iter().chain(supertypes.interfaces.iter()) {
                if !lineage.contains(&supertype) {
                    lineage.push(supertype);
                }
            }
        }
        lineage
    }

    pub(super) fn retain(&self, id: TyId) -> RetainedType {
        RetainedType {
            id,
            node: Arc::downgrade(&self.types[id.0 as usize].0),
        }
    }

    /// The type of the closed shape `shape` of hash `key`, once it is known.
    pub(super) fn closed_shape(
        &self,
        key: u64,
        shape: &crate::ast::variable_dimensions::VariableShape,
    ) -> Option<TyId> {
        self.closed_shapes
            .get(&key)?
            .iter()
            .find(|(known, _)| known.same_closed(shape))
            .map(|(_, id)| *id)
    }

    /// Remember the type `id` of the closed shape `shape` of hash `key`.
    pub(super) fn insert_closed_shape(
        &mut self,
        key: u64,
        shape: crate::ast::variable_dimensions::VariableShape,
        id: TyId,
    ) {
        self.closed_shapes.entry(key).or_default().push((shape, id));
    }

    /// The declaration `name` of elaborated scope `scope`, or of a package or
    /// compilation unit when `scope` is `None`.
    pub fn declare(&mut self, name: &str, scope: Option<&str>) -> DeclId {
        if let Some(&id) = self
            .decl_ids
            .get(name)
            .and_then(|scopes| scopes.get(scope.unwrap_or("")))
        {
            return id;
        }
        let id = DeclId(self.decls.len() as u32);
        let data = DeclData {
            name: name.into(),
            scope: scope.map(Into::into),
            domain: None,
        };
        self.decl_ids
            .entry(data.name.clone())
            .or_default()
            .insert(data.scope.clone().unwrap_or_else(|| "".into()), id);
        self.decls.push(data);
        id
    }

    /// Declaration `id`.
    pub fn decl(&self, id: DeclId) -> &DeclData {
        &self.decls[id.0 as usize]
    }

    /// The id of type `data`.
    pub fn intern(&mut self, data: TyData) -> TyId {
        if let Some(&id) = self.ids.get(&data) {
            return id;
        }
        let canonical = match self.canonical_data(&data) {
            Some(canonical) => self.intern(canonical),
            None => TyId(self.types.len() as u32),
        };
        let id = TyId(self.types.len() as u32);
        self.chandle |= matches!(data, TyData::Chandle);
        let data = Arc::new(data);
        self.types.push((data.clone(), canonical));
        self.ids.insert(data, id);
        id
    }

    /// Whether some type of the analysis is a `chandle` (6.14): binding the
    /// `chandle` type, a DPI formal of it or a type parameter bound to one
    /// interns it ([`Self::declare_chandle`]), as typing any value of it does.
    pub fn has_chandle(&self) -> bool {
        self.chandle
    }

    /// Record that the analysis has a `chandle` type: a declaration, a
    /// formal or a type argument names one.
    pub fn declare_chandle(&mut self) {
        self.intern(TyData::Chandle);
    }

    /// The type `id` names.
    pub fn data(&self, id: TyId) -> &TyData {
        &self.types[id.0 as usize].0
    }

    /// Whether `a` and `b` are matching types (IEEE 1800-2023 6.22.1).
    pub fn matches(&self, a: TyId, b: TyId) -> bool {
        self.canonical(a) == self.canonical(b)
    }

    /// The type that every type matching `id` shares.
    pub fn canonical(&self, id: TyId) -> TyId {
        self.types[id.0 as usize].1
    }

    /// How many types the table holds.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.types.len()
    }

    /// The canonical form of `data` if it is not `data` itself: the type
    /// without what tells matching types apart.
    fn canonical_data(&self, data: &TyData) -> Option<TyData> {
        let canonical = match data {
            TyData::Packed(packed) => TyData::Packed(Packed {
                keyword: None,
                ..packed.clone()
            }),
            TyData::Array { kind, element } => TyData::Array {
                kind: match kind {
                    ArrayKind::Associative { key } => ArrayKind::Associative {
                        key: self.canonical(*key),
                    },
                    kind => kind.clone(),
                },
                element: self.canonical(*element),
            },
            _ => return None,
        };
        (canonical != *data).then_some(canonical)
    }
}
