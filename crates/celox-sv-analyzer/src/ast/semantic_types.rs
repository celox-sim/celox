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
use std::cell::RefCell;
use std::sync::Arc;

mod conversion;
mod relations;
mod spelling;
#[cfg(test)]
mod tests;

pub(super) use conversion::of_shape;

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

/// The identity of the declaration of a named type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Decl {
    /// A declaration identified by its typedef or its source position.
    Named(Arc<str>),
    /// An enumeration whose declaration is not recorded, identified by its
    /// members.
    AnonymousEnum(Arc<super::enums::EnumDomain>),
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
#[derive(Debug, Default)]
pub(super) struct TyTable(RefCell<Interned>);

#[derive(Debug, Default)]
struct Interned {
    /// Each type with the id of the type it matches in canonical form.
    types: Vec<(TyData, TyId)>,
    ids: HashMap<TyData, TyId>,
}

impl TyTable {
    /// The id of type `data`.
    pub fn intern(&self, data: TyData) -> TyId {
        if let Some(&id) = self.0.borrow().ids.get(&data) {
            return id;
        }
        let canonical = match self.canonical_data(&data) {
            Some(canonical) => self.intern(canonical),
            None => TyId(self.0.borrow().types.len() as u32),
        };
        let mut interned = self.0.borrow_mut();
        let id = TyId(interned.types.len() as u32);
        interned.types.push((data.clone(), canonical));
        interned.ids.insert(data, id);
        id
    }

    /// The type `id` names.
    pub fn data(&self, id: TyId) -> TyData {
        self.0.borrow().types[id.0 as usize].0.clone()
    }

    /// Whether `a` and `b` are matching types (IEEE 1800-2023 6.22.1).
    pub fn matches(&self, a: TyId, b: TyId) -> bool {
        self.canonical(a) == self.canonical(b)
    }

    /// The type that every type matching `id` shares.
    pub fn canonical(&self, id: TyId) -> TyId {
        self.0.borrow().types[id.0 as usize].1
    }

    /// How many types the table holds.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.0.borrow().types.len()
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
