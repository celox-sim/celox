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
    /// Whether `a` and `b` are equivalent types (IEEE 1800-2023 6.22.2).
    pub fn equivalent(&self, a: TyId, b: TyId) -> bool {
        if self.matches(a, b) {
            return true;
        }
        match (self.data(a), self.data(b)) {
            // Rule c: the same number of bits, states and signing; an
            // enumeration is not a packed array, but an array of one is.
            (TyData::Packed(a), TyData::Packed(b)) => {
                let scalar_enum = |packed: &Packed| {
                    matches!(packed.element, PackedElement::Enum { array: false, .. })
                };
                !scalar_enum(&a)
                    && !scalar_enum(&b)
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
                        self.equivalent(a, b)
                    }
                    _ => false,
                };
                kinds && self.equivalent(a_element, b_element)
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
        let assignable = match (&source_kind, &target_kind) {
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
        if !self.equivalent(source_element, target_element) {
            return Err(Mismatch::ElementType);
        }
        Ok(assignable)
    }
}
