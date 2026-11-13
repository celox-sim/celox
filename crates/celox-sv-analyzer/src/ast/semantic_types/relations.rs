//! Type compatibility (IEEE 1800-2023 6.22) between interned types.
use super::*;

/// Why a value of one type cannot be assigned to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ast) enum Mismatch {
    /// The source or the target is not an unpacked array.
    NotArrays,
    /// Only an associative array is assigned to an associative array (7.9.9).
    Associative,
    /// The index types of two associative arrays differ (7.9.9).
    IndexType,
    /// The element types are not equivalent (7.6).
    ElementType,
    /// A fixed-size target has another number of elements than the source.
    Size { actual: u128, target: u128 },
    /// The source or the target is not a handle.
    NotHandles,
    /// The source's class neither is nor derives from or implements the
    /// target's (8.16, 8.26.5).
    ClassHandle,
    /// A virtual interface of another interface, or with a modport that the
    /// target's is not (25.9).
    VirtualInterface,
    /// A value of another type than an enumeration's is assigned to it
    /// without a cast (6.19.4).
    Enum,
    /// The types have no assignment between them (6.22.3).
    NotAssignable,
    /// The types have no cast between them (6.22.4, 6.24).
    NotCastable,
}

/// How a cast that the types allow converts its operand (IEEE 1800-2023
/// 6.24).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum Castable {
    /// The operand is assignment compatible: the cast assigns it (6.24.1).
    Assign(Assignable),
    /// To an enumeration from a value of another type, converted to its base
    /// type without checking that it is a member (6.19.4).
    Enum,
    /// Between bit-stream types (6.24.3), with the sizes in bits of the
    /// source and the target when they are fixed: a fixed size the other
    /// has only at run time is checked there.
    BitStream {
        source_bits: Option<u128>,
        target_bits: Option<u128>,
    },
    /// A class handle whose object `$cast` checks at run time (8.16): to a
    /// class derived from the source's, or from an interface class.
    Dynamic,
}

/// What an assignment that the types allow still checks when it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum Assignable {
    Always,
    /// The source is resized at run time; a fixed-size target of this many
    /// elements takes it only if it has as many (7.6).
    WhenSized(u128),
}

impl TyTable {
    /// Whether `id` is an integral type: a packed type, made of bits, of an
    /// enumeration or of a packed structure or union (IEEE 1800-2023 6.11.1).
    pub fn is_integral(&self, id: TyId) -> bool {
        matches!(self.data(id), TyData::Packed(_))
    }

    /// Whether `id` is an enumerated type, not a packed array of one (6.19).
    pub fn is_enum(&self, id: TyId) -> bool {
        matches!(self.data(id), TyData::Packed(packed) if scalar_enum(packed))
    }

    /// Whether `a` and `b` are equivalent types (IEEE 1800-2023 6.22.2).
    pub fn equivalent(&self, a: TyId, b: TyId) -> bool {
        if self.matches(a, b) {
            return true;
        }
        match (self.data(a), self.data(b)) {
            // Rule c: the same number of bits, states and signing; an
            // enumeration is not a packed array, but an array of one is.
            (TyData::Packed(a), TyData::Packed(b)) => {
                !scalar_enum(a)
                    && !scalar_enum(b)
                    && a.bits() == b.bits()
                    && a.four_state == b.four_state
                    && a.signed == b.signed
            }
            (
                TyData::Array {
                    kind: a_kind,
                    element: a_element,
                },
                TyData::Array {
                    kind: b_kind,
                    element: b_element,
                },
            ) => {
                let kinds = match (a_kind, b_kind) {
                    // Rule d: bounds may differ.
                    (ArrayKind::Fixed(a), ArrayKind::Fixed(b)) => a.size() == b.size(),
                    // Rule e: the same kind and equivalent index types.
                    (ArrayKind::Dynamic, ArrayKind::Dynamic)
                    | (ArrayKind::Queue { .. }, ArrayKind::Queue { .. }) => true,
                    (ArrayKind::Associative { key: a }, ArrayKind::Associative { key: b }) => {
                        self.equivalent(*a, *b)
                    }
                    _ => false,
                };
                kinds && self.equivalent(*a_element, *b_element)
            }
            _ => false,
        }
    }

    /// Whether an unpacked array of type `source` may be assigned to one of
    /// type `target` (IEEE 1800-2023 7.6, 7.9.9): the kinds of their
    /// slowest-varying dimensions may differ, but their elements are
    /// equivalent and a fixed-size target has as many elements.
    pub fn assignable_array(&self, source: TyId, target: TyId) -> Result<Assignable, Mismatch> {
        let (
            TyData::Array {
                kind: source_kind,
                element: source_element,
            },
            TyData::Array {
                kind: target_kind,
                element: target_element,
            },
        ) = (self.data(source), self.data(target))
        else {
            return Err(Mismatch::NotArrays);
        };
        let assignable = match (source_kind, target_kind) {
            (ArrayKind::Associative { key: source }, ArrayKind::Associative { key: target }) => {
                if !self.matches(*source, *target) {
                    return Err(Mismatch::IndexType);
                }
                Assignable::Always
            }
            (ArrayKind::Associative { .. }, _) | (_, ArrayKind::Associative { .. }) => {
                return Err(Mismatch::Associative);
            }
            (ArrayKind::Fixed(source), ArrayKind::Fixed(target)) => {
                if source.size() != target.size() {
                    return Err(Mismatch::Size {
                        actual: source.size(),
                        target: target.size(),
                    });
                }
                Assignable::Always
            }
            (_, ArrayKind::Fixed(target)) => Assignable::WhenSized(target.size()),
            _ => Assignable::Always,
        };
        if !self.equivalent(*source_element, *target_element) {
            return Err(Mismatch::ElementType);
        }
        Ok(assignable)
    }

    /// Whether a handle of type `source` may be assigned to a handle of type
    /// `target` (IEEE 1800-2023 6.22.3): `null` to any class, chandle or
    /// virtual interface handle (6.11.1, 6.14, 25.9), and a class to its own
    /// class, a class it derives from or an interface class it implements
    /// (8.16, 8.26.5), as far as the classes' supertypes are recorded; a
    /// virtual interface takes one of its interface with no modport or with
    /// its own modport (25.9).
    pub fn assignable_handle(&self, source: TyId, target: TyId) -> Result<(), Mismatch> {
        match (self.data(source), self.data(target)) {
            (
                TyData::VirtualInterface {
                    interface: source_interface,
                    modport: source_modport,
                },
                TyData::VirtualInterface {
                    interface: target_interface,
                    modport: target_modport,
                },
            ) => {
                if source_interface == target_interface
                    && (source_modport.is_none() || source_modport == target_modport)
                {
                    Ok(())
                } else {
                    Err(Mismatch::VirtualInterface)
                }
            }
            (
                TyData::Null,
                TyData::Class(_) | TyData::Chandle | TyData::VirtualInterface { .. },
            ) => Ok(()),
            (TyData::Class(_), TyData::Class(_)) => {
                if self.lineage(source).contains(&target) {
                    Ok(())
                } else {
                    Err(Mismatch::ClassHandle)
                }
            }
            _ => Err(Mismatch::NotHandles),
        }
    }

    /// Whether a value of type `source` may be assigned to a variable of
    /// type `target` (IEEE 1800-2023 6.22.3): numeric types convert into
    /// each other except into an enumeration, which takes only its own type
    /// (6.19.4); strings, structures and handles take their own kinds; and
    /// unpacked arrays follow [`Self::assignable_array`]. An integral value
    /// is assigned to a string only by a cast (6.16).
    pub fn assignable(&self, source: TyId, target: TyId) -> Result<Assignable, Mismatch> {
        if self.matches(source, target) {
            return Ok(Assignable::Always);
        }
        let numeric =
            |data: &TyData| matches!(data, TyData::Packed(_) | TyData::Real | TyData::Shortreal);
        match (self.data(source), self.data(target)) {
            (TyData::Array { .. }, TyData::Array { .. }) => self.assignable_array(source, target),
            (
                TyData::Null | TyData::Class(_) | TyData::Chandle | TyData::VirtualInterface { .. },
                _,
            )
            | (_, TyData::Class(_) | TyData::Chandle | TyData::VirtualInterface { .. }) => self
                .assignable_handle(source, target)
                .map(|()| Assignable::Always),
            (_, TyData::Packed(packed)) if scalar_enum(packed) => Err(Mismatch::Enum),
            (source, target) if numeric(source) && numeric(target) => Ok(Assignable::Always),
            (TyData::UnpackedStruct(_), TyData::UnpackedStruct(_))
                if self.equivalent(source, target) =>
            {
                Ok(Assignable::Always)
            }
            _ => Err(Mismatch::NotAssignable),
        }
    }

    /// Whether a value of type `source` may be cast to type `target` (IEEE
    /// 1800-2023 6.22.4): assignment compatible types, an enumeration from a
    /// numeric value (6.19.4), class handles that `$cast` checks at run time
    /// (8.16), and bit-stream types (6.24.3) other than into a class or a
    /// type holding an associative array.
    pub fn castable(&self, source: TyId, target: TyId) -> Result<Castable, Mismatch> {
        if let Ok(assignable) = self.assignable(source, target) {
            return Ok(Castable::Assign(assignable));
        }
        match (self.data(source), self.data(target)) {
            (TyData::Packed(_) | TyData::Real | TyData::Shortreal, TyData::Packed(packed))
                if scalar_enum(packed) =>
            {
                return Ok(Castable::Enum);
            }
            (TyData::Class(_), TyData::Class(_)) => {
                let interface = |class| {
                    self.supertypes(class)
                        .is_some_and(|supertypes| supertypes.interface)
                };
                return if self.lineage(target).contains(&source) || interface(source) {
                    Ok(Castable::Dynamic)
                } else {
                    Err(Mismatch::ClassHandle)
                };
            }
            _ => {}
        }
        // An associative array, or a type holding one, has no indices a
        // stream could give (6.24.3).
        if self.holds_associative(target) {
            return Err(Mismatch::NotCastable);
        }
        match (self.bit_stream(source), self.bit_stream(target)) {
            (Some(Some(actual)), Some(Some(target))) if actual != target => {
                Err(Mismatch::Size { actual, target })
            }
            (Some(source_bits), Some(target_bits)) => Ok(Castable::BitStream {
                source_bits,
                target_bits,
            }),
            _ => Err(Mismatch::NotCastable),
        }
    }

    /// Whether type `id` is an associative array or holds one as an element
    /// or a member.
    fn holds_associative(&self, id: TyId) -> bool {
        match self.data(id) {
            TyData::Array {
                kind: ArrayKind::Associative { .. },
                ..
            } => true,
            TyData::Array { element, .. } => self.holds_associative(*element),
            TyData::UnpackedStruct(Decl::Aggregate { members, .. }) => members
                .iter()
                .any(|(_, member)| self.holds_associative(*member)),
            _ => false,
        }
    }

    /// The size in bits of a value of bit-stream type `id` (6.24.3): `None`
    /// if `id` is not one, `Some(None)` if its size is known only at run time.
    /// A class is treated as no bit-stream type: the types of its properties
    /// are not in the table.
    fn bit_stream(&self, id: TyId) -> Option<Option<u128>> {
        match self.data(id) {
            TyData::Packed(packed) => Some(Some(packed.bits())),
            // A string is a dynamic array of bytes here (6.24.3).
            TyData::String => Some(None),
            TyData::UnpackedStruct(Decl::Aggregate { members, .. }) => {
                members.iter().try_fold(Some(0u128), |bits, (_, member)| {
                    let member = self.bit_stream(*member)?;
                    Some(bits.zip(member).map(|(bits, member)| bits + member))
                })
            }
            TyData::Array { kind, element } => {
                let element = self.bit_stream(*element)?;
                Some(match kind {
                    ArrayKind::Fixed(range) => element.map(|bits| bits * range.size()),
                    _ => None,
                })
            }
            _ => None,
        }
    }
}

/// Whether `packed` is an enumeration, not a packed array of one.
fn scalar_enum(packed: &Packed) -> bool {
    matches!(packed.element, PackedElement::Enum { array: false, .. })
}
