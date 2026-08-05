//! Source-language-independent design identities and semantic vocabulary.

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DomainKind {
    ClockPosedge,
    ClockNegedge,
    ResetAsyncHigh,
    ResetAsyncLow,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TriggerIdWithKind {
    pub kind: DomainKind,
    pub id: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortTypeKind {
    Clock,
    ResetAsyncHigh,
    ResetAsyncLow,
    ResetSyncHigh,
    ResetSyncLow,
    Logic,
    Bit,
    Other,
}

/// Source-independent metadata for one elaborated design variable.
///
/// Source IDs, source paths, and declaration syntax belong to the frontend and
/// deliberately are not part of this type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VariableMetadata {
    pub width: usize,
    pub is_4state: bool,
    pub kind: DomainKind,
    pub type_kind: PortTypeKind,
    /// Per-dimension sizes for array ports (for example, `[4]` for `logic<32>[4]`).
    /// Empty means scalar.
    pub array_dims: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TriggerSet<A> {
    pub clock: A,
    pub resets: Vec<A>,
}

#[derive(Clone, Copy, Debug)]
pub enum RuntimeEventKind {
    Display,
    AssertContinue,
    AssertFatal,
}

#[derive(Clone, Debug)]
pub struct RuntimeEventSite {
    pub kind: RuntimeEventKind,
    pub template: Option<String>,
    pub arg_widths: Vec<usize>,
    pub arg_signed: Vec<bool>,
    pub arg_is_string: Vec<bool>,
}

/// Runtime activation recipe for one combinational event site.
///
/// Expression trees used to emit the event have already been lowered into
/// SIR.  The runtime only retains the persistent-state ranges needed to detect
/// whether the corresponding combinational process must be observed again.
#[derive(Clone, Debug)]
pub struct RuntimeCombObserver<A> {
    pub site_id: u32,
    pub activation_group: u32,
    pub sensitivity: Vec<VarAtomBase<A>>,
    pub written_inputs: Vec<A>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialStateWriteRun {
    pub bit_offset: usize,
    pub bit_width: usize,
    pub value_bytes: Vec<u8>,
    pub mask_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitialStateData {
    Packed {
        value: BigUint,
        mask: BigUint,
        written_mask: BigUint,
    },
    Writes(Vec<InitialStateWriteRun>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialStateValue<A> {
    pub address: A,
    pub data: InitialStateData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeErrorInfo<A> {
    pub message: String,
    pub signals: Vec<A>,
}

/// Source-independent runtime diagnostics and observable event descriptions.
#[derive(Clone, Debug)]
pub struct RuntimeSchema<A> {
    pub runtime_errors: HashMap<i64, RuntimeErrorInfo<A>>,
    pub runtime_event_sites: Vec<RuntimeEventSite>,
    pub comb_observers: Vec<RuntimeCombObserver<A>>,
    /// Persistent state read directly by host-side testbench execution. These
    /// are optimization roots even when no SIR instruction loads them.
    pub testbench_read_roots: HashSet<A>,
    /// Bit ranges written by RTL execution units. External component outputs
    /// may not overlap these ranges because that would create multiple drivers.
    pub rtl_writes: HashSet<VarAtomBase<A>>,
}

impl<A> Default for RuntimeSchema<A> {
    fn default() -> Self {
        Self {
            runtime_errors: HashMap::default(),
            runtime_event_sites: Vec::new(),
            comb_observers: Vec::new(),
            testbench_read_roots: HashSet::default(),
            rtl_writes: HashSet::default(),
        }
    }
}

/// Source-independent event-domain topology after elaboration.
///
/// Addresses are already flattened. Source paths and frontend IDs used only
/// for diagnostics or lookup deliberately live outside this structure.
#[derive(Clone, Debug)]
pub struct EventTopology<A> {
    /// Alias event address to the canonical event-domain address.
    pub aliases: HashMap<A, A>,
    /// Canonical event domains in evaluation order.
    pub ordered_events: Vec<A>,
    /// Canonical clocks whose value may be changed by another event domain.
    pub cascaded_events: BTreeSet<A>,
    /// Canonical asynchronous/synchronous reset to its canonical clock.
    pub reset_clocks: HashMap<A, A>,
}

impl<A> Default for EventTopology<A> {
    fn default() -> Self {
        Self {
            aliases: HashMap::default(),
            ordered_events: Vec::new(),
            cascaded_events: BTreeSet::new(),
            reset_clocks: HashMap::default(),
        }
    }
}

impl<A: Copy + Eq + std::hash::Hash> EventTopology<A> {
    pub fn canonical(&self, address: A) -> A {
        self.aliases.get(&address).copied().unwrap_or(address)
    }

    pub fn len(&self) -> usize {
        self.ordered_events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ordered_events.is_empty()
    }
}

/// Backend-neutral semantic design data after hierarchy flattening.
///
/// This is intentionally not a frontend lookup table: every state object is
/// keyed by its flattened semantic address, and no source-language AST or path
/// type is retained.
#[derive(Clone, Debug)]
pub struct ElaboratedDesign<A> {
    pub state_objects: HashMap<A, VariableMetadata>,
    pub events: EventTopology<A>,
    pub initial_state: Vec<InitialStateValue<A>>,
}

impl<A> Default for ElaboratedDesign<A> {
    fn default() -> Self {
        Self {
            state_objects: HashMap::default(),
            events: EventTopology::default(),
            initial_state: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    DivU,
    DivS,
    RemU,
    RemS,
    And,
    Or,
    Xor,
    Shl, // Logical Shift Left (<<)
    Shr, // Logical Shift Right (>>)
    Sar, // Arithmetic Shift Right (>>>)
    Eq,
    Ne,
    LtU,
    LtS, // Less Than (Unsigned / Signed)
    LeU,
    LeS, // Less Equal
    GtU,
    GtS, // Greater Than
    GeU,
    GeS, // Greater Equal
    LogicAnd,
    LogicOr,
    EqWildcard,
    NeWildcard,
}

impl BinaryOp {
    /// Whether the operation is commutative (a op b == b op a).
    pub fn is_commutative(&self) -> bool {
        matches!(
            self,
            BinaryOp::Add
                | BinaryOp::Mul
                | BinaryOp::And
                | BinaryOp::Or
                | BinaryOp::Xor
                | BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
        )
    }
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self {
            BinaryOp::Add => "Add",
            BinaryOp::Sub => "Sub",
            BinaryOp::Mul => "Mul",
            BinaryOp::DivU => "DivU",
            BinaryOp::DivS => "DivS",
            BinaryOp::RemU => "RemU",
            BinaryOp::RemS => "RemS",
            BinaryOp::And => "And",
            BinaryOp::Or => "Or",
            BinaryOp::Xor => "Xor",
            BinaryOp::Shl => "Shl",
            BinaryOp::Shr => "Shr",
            BinaryOp::Sar => "Sar",
            BinaryOp::Eq => "Eq",
            BinaryOp::Ne => "Ne",
            BinaryOp::LtU => "LtU",
            BinaryOp::LtS => "LtS",
            BinaryOp::LeU => "LeU",
            BinaryOp::LeS => "LeS",
            BinaryOp::GtU => "GtU",
            BinaryOp::GtS => "GtS",
            BinaryOp::GeU => "GeU",
            BinaryOp::GeS => "GeS",
            BinaryOp::LogicAnd => "LogicAnd",
            BinaryOp::LogicOr => "LogicOr",
            BinaryOp::EqWildcard => "EqWildcard",
            BinaryOp::NeWildcard => "NeWildcard",
        };
        write!(f, "{}", op_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    Ident,
    /// Convert a four-state value to two-state form. Unknown bits become zero:
    /// `(value, mask) -> (value & !mask, 0)`.
    ToTwoState,
    Minus,
    BitNot,
    LogicNot,
    And,
    Or,
    Xor,
    PopCount,
    CountLeadingZeros,
    CountTrailingZeros,
}

impl UnaryOp {
    /// Return the canonical result width for an operand of `operand_width` bits.
    ///
    /// Bit-count operations return a value in `0..=operand_width`, which needs
    /// `ceil(log2(operand_width + 1))` bits.  Computing that as the bit length
    /// of `operand_width` avoids overflowing when the operand width is
    /// `usize::MAX`.
    pub fn result_width(self, operand_width: usize) -> usize {
        match self {
            UnaryOp::LogicNot | UnaryOp::And | UnaryOp::Or | UnaryOp::Xor => 1,
            UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Minus | UnaryOp::BitNot => {
                operand_width
            }
            UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                usize::BITS as usize - operand_width.leading_zeros() as usize
            }
        }
    }
}

impl fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_str = match self {
            UnaryOp::Ident => "Ident",
            UnaryOp::ToTwoState => "ToTwoState",
            UnaryOp::Minus => "Minus",
            UnaryOp::BitNot => "BitNot",
            UnaryOp::LogicNot => "LogicNot",
            UnaryOp::And => "And",
            UnaryOp::Or => "Or",
            UnaryOp::Xor => "Xor",
            UnaryOp::PopCount => "PopCount",
            UnaryOp::CountLeadingZeros => "CountLeadingZeros",
            UnaryOp::CountTrailingZeros => "CountTrailingZeros",
        };
        write!(f, "{}", op_str)
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct BitAccess {
    pub lsb: usize,
    pub msb: usize,
}
impl BitAccess {
    pub fn new(lsb: usize, msb: usize) -> Self {
        debug_assert!(lsb <= msb, "lsb must be less than or equal to msb");
        Self { lsb, msb }
    }
    pub fn overlaps(&self, other: &Self) -> bool {
        !(self.msb < other.lsb || other.msb < self.lsb)
    }

    /// Calculates the atomic bit ranges for a given access range and a set of boundaries.
    pub fn calculate_atoms(&self, bounds: &BTreeSet<usize>) -> Vec<Self> {
        use std::ops::Bound::*;
        let mut atoms = Vec::new();
        let mut current_lsb = self.lsb;

        // Iterate through the boundaries that are within the access range
        // Excluded(lsb) to Included(msb) handles lsb == msb case naturally (returns empty iterator)
        for &bound in bounds.range((Excluded(self.lsb), Included(self.msb))) {
            atoms.push(Self::new(current_lsb, bound - 1));
            current_lsb = bound;
        }

        // Add the last atom
        if current_lsb <= self.msb {
            atoms.push(Self::new(current_lsb, self.msb));
        }

        atoms
    }
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
pub struct VarAtomBase<A> {
    pub id: A,
    pub access: BitAccess,
}
impl<A> VarAtomBase<A> {
    pub fn new(id: A, lsb: usize, msb: usize) -> Self {
        Self {
            id,
            access: BitAccess { lsb, msb },
        }
    }
}
impl fmt::Display for BitAccess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.lsb == self.msb {
            write!(f, "[{}]", self.lsb)
        } else {
            write!(f, "[{}:{}]", self.msb, self.lsb)
        }
    }
}

impl<A> fmt::Display for VarAtomBase<A>
where
    A: fmt::Display + std::hash::Hash + Eq,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.id, self.access)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModuleId(pub usize);

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "mod{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceId(pub usize);

impl fmt::Display for InstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "inst{}", self.0)
    }
}

/// Dense source-independent identity of one flattened state object.
///
/// Frontends assign this identity during design projection. Source variable
/// IDs must not cross into SIR optimization, layout, or backend code.
#[derive(
    Debug, Clone, Copy, Default, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct StateObjectId(pub u32);

impl StateObjectId {
    pub const fn from_raw(value: u32) -> Self {
        Self(value)
    }
}

impl fmt::Display for StateObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "state{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AbsoluteAddrBase<V> {
    pub instance_id: InstanceId,
    pub var_id: V,
}

impl<V: fmt::Display> fmt::Display for AbsoluteAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AbsoluteAddr({}, {})", self.instance_id, self.var_id)
    }
}

pub const STABLE_REGION: u32 = 0;
pub const WORKING_REGION: u32 = 1;
pub const SPARSE_WORKING_REGION: u32 = 2;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegionedVarAddrBase<V> {
    pub region: u32,
    pub var_id: V,
}

impl<V: fmt::Display> fmt::Display for RegionedVarAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedVarAddr(region={}, {})",
            self.region, self.var_id
        )
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RegionedAbsoluteAddrBase<V> {
    pub region: u32,
    pub instance_id: InstanceId,
    pub var_id: V,
}

pub type StateAddr = AbsoluteAddrBase<StateObjectId>;
pub type RegionedStateAddr = RegionedAbsoluteAddrBase<StateObjectId>;

impl<V: Copy> RegionedAbsoluteAddrBase<V> {
    pub fn from_absolute_addr(region: u32, addr: AbsoluteAddrBase<V>) -> Self {
        Self {
            region,
            instance_id: addr.instance_id,
            var_id: addr.var_id,
        }
    }

    pub fn absolute_addr(&self) -> AbsoluteAddrBase<V> {
        AbsoluteAddrBase {
            instance_id: self.instance_id,
            var_id: self.var_id,
        }
    }
}

impl<V: fmt::Display> fmt::Display for RegionedAbsoluteAddrBase<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RegionedAbsoluteAddr(region={}, {}, {})",
            self.region, self.instance_id, self.var_id
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_access_splits_only_at_internal_boundaries() {
        let access = BitAccess::new(4, 11);
        let bounds = [0, 4, 7, 12, 20].into_iter().collect();

        assert_eq!(
            access.calculate_atoms(&bounds),
            vec![BitAccess::new(4, 6), BitAccess::new(7, 11)]
        );
    }

    #[test]
    fn design_ids_and_addresses_have_stable_display() {
        let address = AbsoluteAddrBase {
            instance_id: InstanceId(42),
            var_id: 7,
        };

        assert_eq!(ModuleId(3).to_string(), "mod3");
        assert_eq!(InstanceId(42).to_string(), "inst42");
        assert_eq!(StateObjectId(7).to_string(), "state7");
        assert_eq!(address.to_string(), "AbsoluteAddr(inst42, 7)");
    }

    #[test]
    fn regioned_address_round_trips_semantic_identity() {
        let address = AbsoluteAddrBase {
            instance_id: InstanceId(2),
            var_id: 9,
        };
        let regioned = RegionedAbsoluteAddrBase::from_absolute_addr(WORKING_REGION, address);

        assert_eq!(regioned.absolute_addr(), address);
        assert_eq!(regioned.region, WORKING_REGION);
    }

    #[test]
    fn semantic_operator_contracts_are_source_independent() {
        assert!(BinaryOp::Add.is_commutative());
        assert!(!BinaryOp::Sub.is_commutative());
        assert_eq!(UnaryOp::LogicNot.result_width(128), 1);
        assert_eq!(UnaryOp::PopCount.result_width(128), 8);
    }

    #[test]
    fn initial_state_and_runtime_error_schemas_accept_design_owned_ids() {
        let initial = InitialStateValue {
            address: AbsoluteAddrBase {
                instance_id: InstanceId(1),
                var_id: 7u32,
            },
            data: InitialStateData::Writes(vec![InitialStateWriteRun {
                bit_offset: 3,
                bit_width: 5,
                value_bytes: vec![0x15],
                mask_bytes: vec![0],
            }]),
        };
        let error = RuntimeErrorInfo {
            message: "failed".to_string(),
            signals: vec![initial.address],
        };
        let mut runtime = RuntimeSchema::default();
        runtime.runtime_errors.insert(1, error.clone());
        runtime.runtime_event_sites.push(RuntimeEventSite {
            kind: RuntimeEventKind::AssertFatal,
            template: Some("failed".to_string()),
            arg_widths: Vec::new(),
            arg_signed: Vec::new(),
            arg_is_string: Vec::new(),
        });
        runtime.comb_observers.push(RuntimeCombObserver {
            site_id: 0,
            activation_group: 0,
            sensitivity: vec![VarAtomBase {
                id: initial.address,
                access: BitAccess { lsb: 3, msb: 7 },
            }],
            written_inputs: vec![initial.address],
        });
        runtime.testbench_read_roots.insert(initial.address);

        assert_eq!(error.signals, vec![initial.address]);
        assert!(matches!(initial.data, InitialStateData::Writes(_)));
        assert_eq!(runtime.runtime_errors[&1], error);
        assert_eq!(runtime.runtime_event_sites.len(), 1);
        assert_eq!(runtime.comb_observers[0].sensitivity[0].id, initial.address);
        assert!(runtime.testbench_read_roots.contains(&initial.address));
    }

    #[test]
    fn variable_metadata_preserves_elaborated_shape_and_domain() {
        let metadata = VariableMetadata {
            width: 32,
            is_4state: true,
            kind: DomainKind::Other,
            type_kind: PortTypeKind::Logic,
            array_dims: vec![4],
        };

        assert_eq!(metadata.width, 32);
        assert_eq!(metadata.array_dims, vec![4]);
    }

    #[test]
    fn elaborated_design_uses_flat_addresses_and_canonical_event_topology() {
        let mut design = ElaboratedDesign::<u32>::default();
        design.state_objects.insert(
            10,
            VariableMetadata {
                width: 1,
                is_4state: false,
                kind: DomainKind::ClockPosedge,
                type_kind: PortTypeKind::Clock,
                array_dims: Vec::new(),
            },
        );
        design.events.aliases.insert(11, 10);
        design.events.ordered_events.push(10);

        assert_eq!(design.events.canonical(11), 10);
        assert_eq!(design.events.canonical(12), 12);
        assert_eq!(design.events.len(), 1);
        assert!(!design.events.is_empty());
        assert_eq!(design.state_objects[&10].width, 1);
    }
}
