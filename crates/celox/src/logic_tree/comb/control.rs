//! Checked identifiers and forward-definition storage for control provenance.
//!
//! This is the first, producer-independent part of the control-provenance
//! implementation. The records that use these IDs are added separately.

#![allow(dead_code)]

#[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
compile_error!("Celox control IDs require usize to represent every u32 value");

use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// Failure while constructing a checked control-provenance artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ControlBuildError {
    /// A dense ID cannot represent the next vector position.
    IdExhausted {
        kind: &'static str,
        attempted_length: usize,
    },
    /// Dense storage for the next record could not be reserved.
    StorageUnavailable { kind: &'static str, count: usize },
    /// A reserved slot was never defined, or an unreserved slot was named.
    UndefinedSlot { kind: &'static str, slot: usize },
    /// A slot was defined more than once.
    DoubleDefine { kind: &'static str, slot: usize },
}

impl fmt::Display for ControlBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IdExhausted {
                kind,
                attempted_length,
            } => write!(
                formatter,
                "{kind} ID space is exhausted at vector length {attempted_length}"
            ),
            Self::StorageUnavailable { kind, count } => {
                write!(
                    formatter,
                    "cannot reserve storage for {count} {kind} records"
                )
            }
            Self::UndefinedSlot { kind, slot } => {
                write!(formatter, "{kind} slot {slot} is undefined")
            }
            Self::DoubleDefine { kind, slot } => {
                write!(formatter, "{kind} slot {slot} was defined more than once")
            }
        }
    }
}

impl std::error::Error for ControlBuildError {}

/// Common operations required by [`CheckedSlots`].
pub(crate) trait CheckedControlId: Copy {
    const KIND: &'static str;

    fn checked_from_len(length: usize) -> Result<Self, ControlBuildError>;
    fn index(self) -> usize;
}

macro_rules! define_control_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        pub(crate) struct $name(u32);

        impl $name {
            pub(crate) const KIND: &'static str = stringify!($name);

            /// Construct the ID for the next position in a dense vector.
            pub(crate) fn checked_from_len(length: usize) -> Result<Self, ControlBuildError> {
                <Self as CheckedControlId>::checked_from_len(length)
            }

            /// Return the vector index represented by this ID.
            pub(crate) fn index(self) -> usize {
                <Self as CheckedControlId>::index(self)
            }

            /// Return the serialized integer representation.
            pub(crate) const fn raw(self) -> u32 {
                self.0
            }
        }

        impl CheckedControlId for $name {
            const KIND: &'static str = stringify!($name);

            fn checked_from_len(length: usize) -> Result<Self, ControlBuildError> {
                let raw = u32::try_from(length).map_err(|_| ControlBuildError::IdExhausted {
                    kind: Self::KIND,
                    attempted_length: length,
                })?;
                Ok(Self(raw))
            }

            fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

define_control_id!(SourceRootId);
define_control_id!(SourceControlUnitId);
define_control_id!(SourcePredicateRegionId);
define_control_id!(SourceControlPointId);
define_control_id!(SourceControlEdgeId);
define_control_id!(SourceGateId);
define_control_id!(SourceDecisionId);
define_control_id!(SourceValueOccurrenceId);
define_control_id!(ValueOccurrenceId);
define_control_id!(RootExpansionId);
define_control_id!(ControlUnitId);
define_control_id!(ExternalRootId);
define_control_id!(ObserverId);
define_control_id!(ObserverOccurrenceId);
define_control_id!(ControlActionId);
define_control_id!(GateId);
define_control_id!(DecisionId);
define_control_id!(GatedMuxId);
define_control_id!(DecisionResultMergeId);
define_control_id!(PredicateRegionId);
define_control_id!(ControlPointId);
define_control_id!(ControlEdgeId);
define_control_id!(GlobalControlPointId);
define_control_id!(GlobalControlEdgeId);
define_control_id!(InstValueId);
define_control_id!(DynamicAddressPlanId);
define_control_id!(MemoryTokenId);
define_control_id!(EnvironmentTokenId);
define_control_id!(EffectTokenId);
define_control_id!(ForFoldTemplateId);
define_control_id!(WriteDomainId);
define_control_id!(BindingId);
define_control_id!(EffectStreamId);
define_control_id!(SLTMemoryDependencyId);
define_control_id!(SLTEnvDependencyId);

/// A source-level position before, between, or after ordered source actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct SourceControlSite {
    pub(crate) point: SourceControlPointId,
    pub(crate) slot: usize,
}

impl SourceControlSite {
    pub(crate) const fn new(point: SourceControlPointId, slot: usize) -> Self {
        Self { point, slot }
    }
}

/// A source operand use either at an action slot or on one exact CFG edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum SourceControlUseSite {
    Slot(SourceControlSite),
    Edge(SourceControlEdgeId),
}

/// A position before, between, or after the ordered actions in a control point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) struct ControlSite {
    pub(crate) point: ControlPointId,
    pub(crate) slot: usize,
}

impl ControlSite {
    pub(crate) const fn new(point: ControlPointId, slot: usize) -> Self {
        Self { point, slot }
    }
}

/// A flattened operand use either at an action slot or on one exact CFG edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum ControlUseSite {
    Slot(ControlSite),
    Edge(ControlEdgeId),
}

/// Dense storage that supports reserving IDs before their records are known.
///
/// A definition error poisons the builder as well as being returned from
/// [`define`](Self::define). Consequently, ignoring that immediate error cannot
/// allow [`finish`](Self::finish) to expose an invalid artifact.
#[derive(Debug)]
pub(crate) struct CheckedSlots<Id, T> {
    slots: Vec<Option<T>>,
    definition_error: Option<ControlBuildError>,
    id: PhantomData<fn() -> Id>,
}

impl<Id, T> Default for CheckedSlots<Id, T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            definition_error: None,
            id: PhantomData,
        }
    }
}

impl<Id, T> CheckedSlots<Id, T>
where
    Id: CheckedControlId,
{
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Reserve one undefined slot and return its checked dense ID.
    pub(crate) fn reserve(&mut self) -> Result<Id, ControlBuildError> {
        let id = Id::checked_from_len(self.slots.len())?;
        let count = self.slots.len().saturating_add(1);
        self.slots
            .try_reserve(1)
            .map_err(|_| ControlBuildError::StorageUnavailable {
                kind: Id::KIND,
                count,
            })?;
        self.slots.push(None);
        Ok(id)
    }

    /// Define a previously reserved slot exactly once.
    pub(crate) fn define(&mut self, id: Id, value: T) -> Result<(), ControlBuildError> {
        let slot = id.index();
        let Some(entry) = self.slots.get_mut(slot) else {
            let error = ControlBuildError::UndefinedSlot {
                kind: Id::KIND,
                slot,
            };
            self.record_definition_error(&error);
            return Err(error);
        };
        if entry.is_some() {
            let error = ControlBuildError::DoubleDefine {
                kind: Id::KIND,
                slot,
            };
            self.record_definition_error(&error);
            return Err(error);
        }
        *entry = Some(value);
        Ok(())
    }

    /// Consume the builder, rejecting any earlier definition error or hole.
    pub(crate) fn finish(self) -> Result<Vec<T>, ControlBuildError> {
        if let Some(error) = self.definition_error {
            return Err(error);
        }

        let mut values = Vec::new();
        values.try_reserve_exact(self.slots.len()).map_err(|_| {
            ControlBuildError::StorageUnavailable {
                kind: Id::KIND,
                count: self.slots.len(),
            }
        })?;
        for (slot, value) in self.slots.into_iter().enumerate() {
            let Some(value) = value else {
                return Err(ControlBuildError::UndefinedSlot {
                    kind: Id::KIND,
                    slot,
                });
            };
            values.push(value);
        }
        Ok(values)
    }

    fn record_definition_error(&mut self, error: &ControlBuildError) {
        if self.definition_error.is_none() {
            self.definition_error = Some(error.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_ids_are_dense_and_expose_safe_accessors() {
        let mut slots = CheckedSlots::<ControlUnitId, &'static str>::new();
        assert!(slots.is_empty());

        let first = slots.reserve().expect("first ID must fit");
        let second = slots.reserve().expect("second ID must fit");

        assert_eq!(first.raw(), 0);
        assert_eq!(first.index(), 0);
        assert_eq!(second.raw(), 1);
        assert_eq!(second.index(), 1);
        assert_eq!(slots.len(), 2);

        slots.define(second, "second").expect("slot is reserved");
        slots.define(first, "first").expect("slot is reserved");
        assert_eq!(slots.finish(), Ok(vec!["first", "second"]));
    }

    #[test]
    fn every_id_type_uses_its_own_checked_kind() {
        macro_rules! check_id {
            ($id:ident) => {{
                let id = $id::checked_from_len(7).expect("small ID must fit");
                assert_eq!(id.raw(), 7);
                assert_eq!(id.index(), 7);
                assert_eq!($id::KIND, stringify!($id));
            }};
        }

        check_id!(SourceRootId);
        check_id!(SourceControlUnitId);
        check_id!(SourcePredicateRegionId);
        check_id!(SourceControlPointId);
        check_id!(SourceControlEdgeId);
        check_id!(SourceGateId);
        check_id!(SourceDecisionId);
        check_id!(SourceValueOccurrenceId);
        check_id!(ValueOccurrenceId);
        check_id!(RootExpansionId);
        check_id!(ControlUnitId);
        check_id!(ExternalRootId);
        check_id!(ObserverId);
        check_id!(ObserverOccurrenceId);
        check_id!(ControlActionId);
        check_id!(GateId);
        check_id!(DecisionId);
        check_id!(GatedMuxId);
        check_id!(DecisionResultMergeId);
        check_id!(PredicateRegionId);
        check_id!(ControlPointId);
        check_id!(ControlEdgeId);
        check_id!(GlobalControlPointId);
        check_id!(GlobalControlEdgeId);
        check_id!(InstValueId);
        check_id!(DynamicAddressPlanId);
        check_id!(MemoryTokenId);
        check_id!(EnvironmentTokenId);
        check_id!(EffectTokenId);
        check_id!(ForFoldTemplateId);
        check_id!(WriteDomainId);
        check_id!(BindingId);
        check_id!(EffectStreamId);
        check_id!(SLTMemoryDependencyId);
        check_id!(SLTEnvDependencyId);
    }

    #[test]
    fn ids_round_trip_through_serde_without_exposing_construction() {
        let id = DecisionResultMergeId::checked_from_len(23).expect("small ID must fit");
        let encoded = serde_json::to_string(&id).expect("ID must serialize");
        let decoded: DecisionResultMergeId =
            serde_json::from_str(&encoded).expect("ID must deserialize");
        assert_eq!(decoded, id);

        let site = ControlSite::new(
            ControlPointId::checked_from_len(5).expect("small ID must fit"),
            11,
        );
        let encoded = serde_json::to_string(&site).expect("site must serialize");
        let decoded: ControlSite = serde_json::from_str(&encoded).expect("site must deserialize");
        assert_eq!(decoded, site);

        let edge_use =
            ControlUseSite::Edge(ControlEdgeId::checked_from_len(9).expect("small ID must fit"));
        let encoded = serde_json::to_string(&edge_use).expect("edge use must serialize");
        let decoded: ControlUseSite =
            serde_json::from_str(&encoded).expect("edge use must deserialize");
        assert_eq!(decoded, edge_use);

        let source_slot = SourceControlUseSite::Slot(SourceControlSite::new(
            SourceControlPointId::checked_from_len(4).expect("small ID must fit"),
            3,
        ));
        let encoded = serde_json::to_string(&source_slot).expect("source use must serialize");
        let decoded: SourceControlUseSite =
            serde_json::from_str(&encoded).expect("source use must deserialize");
        assert_eq!(decoded, source_slot);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn checked_id_reports_u32_exhaustion() {
        let attempted_length =
            usize::try_from(u64::from(u32::MAX) + 1).expect("64-bit usize must fit u32::MAX + 1");
        assert_eq!(
            GateId::checked_from_len(attempted_length),
            Err(ControlBuildError::IdExhausted {
                kind: GateId::KIND,
                attempted_length,
            })
        );
    }

    #[test]
    fn finish_rejects_an_undefined_reserved_slot() {
        let mut slots = CheckedSlots::<PredicateRegionId, u8>::new();
        let defined = slots.reserve().expect("small ID must fit");
        let _undefined = slots.reserve().expect("small ID must fit");
        slots.define(defined, 9).expect("slot is reserved");

        assert_eq!(
            slots.finish(),
            Err(ControlBuildError::UndefinedSlot {
                kind: PredicateRegionId::KIND,
                slot: 1,
            })
        );
    }

    #[test]
    fn double_definition_is_immediate_and_poisons_finish() {
        let mut slots = CheckedSlots::<DecisionId, &'static str>::new();
        let id = slots.reserve().expect("small ID must fit");
        slots.define(id, "first").expect("slot is reserved");

        let expected = ControlBuildError::DoubleDefine {
            kind: DecisionId::KIND,
            slot: 0,
        };
        assert_eq!(slots.define(id, "second"), Err(expected.clone()));
        assert_eq!(slots.finish(), Err(expected));
    }
}
