//! Event IR (EIR): the semantic boundary between AIR/SLT and executable SIR.
//!
//! EIR keeps the event-entry snapshot, combinational definitions,
//! process-local values, staged FF updates, effects, and commit barriers
//! distinct. It contains no physical simulator offsets and does not recover
//! value flow from SIR Store/Load pairs.

mod comb;
mod comb_value_graph;
mod lower;
mod verify;

use num_bigint::BigUint;
use std::sync::Arc;

pub use crate::ir::{BinaryOp, BitAccess, UnaryOp};
pub use comb::{
    CombCaptureRecipe, CombConvergenceRegion, CombDependency, CombGraph, CombImportError,
    CombImportInvariant, CombLocalInput, CombRecipe, CombRecipeTarget, CombSnapshotInput,
    CombSnapshotKind,
};
pub use lower::{EventProjectionError, lower_event_projection};
pub use verify::{EventIrError, EventIrInvariant};

use crate::ir::AbsoluteAddr;

macro_rules! entity_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
        pub struct $name(pub usize);

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, concat!($prefix, "{}"), self.0)
            }
        }
    };
}

entity_id!(ValueId, "v");
entity_id!(RegionId, "region");
entity_id!(ProcessId, "process");
entity_id!(ControlBlockId, "block");
entity_id!(EffectId, "effect");
entity_id!(CombDefinitionId, "comb");
entity_id!(CombRecipeId, "recipe");
entity_id!(CombRecipeNodeId, "slt");
entity_id!(CombConvergenceId, "convergence");

/// The logical event for which one canonical EIR graph is constructed.
///
/// Fused/evaluate/apply are projections of a clock graph, not separate
/// semantic domains. Keeping the stages and commit in one graph prevents the
/// split execution interface from changing HDL visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventDomain {
    Combinational,
    Clock {
        clock: AbsoluteAddr,
        resets: Vec<AbsoluteAddr>,
    },
}

impl EventDomain {
    pub fn is_clock(&self) -> bool {
        matches!(self, Self::Clock { .. })
    }
}

/// Executable view selected while lowering one already-verified EIR graph.
///
/// A projection never owns semantic values or effects. In particular,
/// `EvaluateClock` and `ApplyClock` refer to the same `EventDomain::Clock`
/// graph as `FusedClock`.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum EventProjection {
    Combinational,
    FusedClock,
    EvaluateClock,
    /// Evaluate and commit FF AIR against combinational state which has
    /// already been settled by the shared combinational schedule.
    FusedSettledClock,
    /// Evaluate FF AIR without commit against already-settled combinational
    /// state.
    EvaluateSettledClock,
    ApplyClock,
}

impl EventProjection {
    pub fn is_valid_for(self, domain: &EventDomain) -> bool {
        matches!(
            (self, domain),
            (Self::Combinational, EventDomain::Combinational)
                | (
                    Self::FusedClock
                        | Self::EvaluateClock
                        | Self::FusedSettledClock
                        | Self::EvaluateSettledClock
                        | Self::ApplyClock,
                    EventDomain::Clock { .. }
                )
        )
    }
}

/// Logical value type. Width is an RTL width, not a machine register class.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub struct ValueType {
    pub width: usize,
    pub signed: bool,
    pub four_state: bool,
}

impl ValueType {
    pub fn bit(width: usize, signed: bool) -> Self {
        Self {
            width,
            signed,
            four_state: false,
        }
    }

    pub fn logic(width: usize, signed: bool) -> Self {
        Self {
            width,
            signed,
            four_state: true,
        }
    }
}

/// One exact logical range of an elaborated RTL object.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjectRange {
    pub object: AbsoluteAddr,
    pub access: BitAccess,
}

impl ObjectRange {
    pub fn new(object: AbsoluteAddr, access: BitAccess) -> Self {
        Self { object, access }
    }

    pub fn width(self) -> Option<usize> {
        self.access.msb.checked_sub(self.access.lsb)?.checked_add(1)
    }
}

/// Logical bit offset retained before simulator-memory layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueOffset {
    Static(usize),
    Dynamic(ValueId),
    Element {
        index: ValueId,
        element_width: usize,
        bit_offset: usize,
        dynamic_bit_offset: Option<ValueId>,
    },
}

impl ValueOffset {
    pub fn visit_value_operands(&self, mut visit: impl FnMut(ValueId)) {
        match self {
            Self::Static(_) => {}
            Self::Dynamic(value) => visit(*value),
            Self::Element {
                index,
                dynamic_bit_offset,
                ..
            } => {
                visit(*index);
                if let Some(offset) = dynamic_bit_offset {
                    visit(*offset);
                }
            }
        }
    }
}

/// One static or dynamic logical access to an elaborated RTL object.
///
/// `alias` is the conservative range touched by a dynamic access; for a
/// static access it is the exact range. It is semantic bit-range metadata,
/// not a byte-layout approximation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectAccess {
    pub object: AbsoluteAddr,
    pub offset: ValueOffset,
    pub width: usize,
    pub alias: BitAccess,
}

impl From<ObjectRange> for ObjectAccess {
    fn from(range: ObjectRange) -> Self {
        Self {
            object: range.object,
            offset: ValueOffset::Static(range.access.lsb),
            width: range
                .width()
                .expect("ObjectRange converted to ObjectAccess has a valid width"),
            alias: range.access,
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub enum ValueScope {
    Event,
    Process(ProcessId),
}

/// Structured AIR control regions retained before SIR CFG construction.
/// Combinational convergence regions live in the shared [`CombGraph`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub parent: Option<RegionId>,
    pub kind: RegionKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegionKind {
    EventRoot,
    FfProcess(ProcessId),
    ControlBlock {
        process: ProcessId,
        block: ControlBlockId,
    },
}

/// One AIR process. Every process has an independent local value namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub region: RegionId,
    pub source_order: usize,
    /// Asynchronous reset events which activate this process in addition to
    /// the primary clock named by the enclosing event domain.
    pub resets: Vec<AbsoluteAddr>,
    pub entry: ControlBlockId,
    pub blocks: Vec<ControlBlockId>,
}

/// One process-local control block.
///
/// Block parameters are EIR's explicit phi representation. Incoming edge
/// arguments, rather than a value-node backedge, represent loop-carried
/// values. This keeps the value arena topological even when the process CFG
/// is cyclic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlBlock {
    pub process: ProcessId,
    pub region: RegionId,
    pub parameters: Vec<ValueId>,
    pub terminator: Option<ControlTerminator>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlTerminator {
    Jump {
        target: ControlBlockId,
        arguments: Vec<ValueId>,
    },
    Branch {
        condition: ValueId,
        true_target: ControlBlockId,
        true_arguments: Vec<ValueId>,
        false_target: ControlBlockId,
        false_arguments: Vec<ValueId>,
    },
    Return,
    Error(i64),
}

impl ControlTerminator {
    pub fn visit_value_operands(&self, mut visit: impl FnMut(ValueId)) {
        match self {
            Self::Jump { arguments, .. } => {
                arguments.iter().copied().for_each(&mut visit);
            }
            Self::Branch {
                condition,
                true_arguments,
                false_arguments,
                ..
            } => {
                visit(*condition);
                true_arguments.iter().copied().for_each(&mut visit);
                false_arguments.iter().copied().for_each(&mut visit);
            }
            Self::Return | Self::Error(_) => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub ty: ValueType,
    pub scope: ValueScope,
    pub region: RegionId,
    pub kind: ValueKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueKind {
    BlockParameter {
        block: ControlBlockId,
        index: usize,
    },
    Constant {
        value: BigUint,
        unknown: BigUint,
    },
    ReadClockSnapshot(ObjectRange),
    ReadPersistentMemory {
        object: AbsoluteAddr,
        /// Logical event-entry address. Keeping element metadata here avoids
        /// turning a narrow dynamic memory read into a whole-object SSA value.
        offset: ValueOffset,
        width: usize,
    },
    ReadCombDefinition {
        definition: CombDefinitionId,
        access: BitAccess,
    },
    Unary {
        op: UnaryOp,
        input: ValueId,
    },
    /// Width/state-kind conversion whose destination semantics are carried by
    /// the enclosing [`ValueType`].
    Resize {
        input: ValueId,
    },
    Binary {
        op: BinaryOp,
        lhs: ValueId,
        rhs: ValueId,
    },
    Mux {
        condition: ValueId,
        then_value: ValueId,
        else_value: ValueId,
    },
    Slice {
        source: ValueId,
        access: BitAccess,
    },
    Concat {
        /// MSB first, matching SIR and SLT.
        parts: Vec<ValueId>,
    },
    DynamicSelect {
        source: ValueId,
        offset: ValueOffset,
        width: usize,
    },
    /// Process-local SSA update. The result has the complete `base` type;
    /// `value` replaces only the selected range.
    UpdateRange {
        base: ValueId,
        offset: ValueOffset,
        value: ValueId,
        width: usize,
    },
    ProcessPhi {
        inputs: Vec<ValueId>,
    },
    LoopValue {
        initial: ValueId,
        update: ValueId,
    },
}

impl ValueKind {
    /// Visit operands without allocating a temporary vector per value.
    pub fn visit_operands(&self, mut visit: impl FnMut(ValueId)) {
        match self {
            Self::BlockParameter { .. }
            | Self::Constant { .. }
            | Self::ReadClockSnapshot(_)
            | Self::ReadCombDefinition { .. } => {}
            Self::ReadPersistentMemory { offset, .. } => {
                offset.visit_value_operands(&mut visit);
            }
            Self::Unary { input, .. } => visit(*input),
            Self::Resize { input } => visit(*input),
            Self::Binary { lhs, rhs, .. } => {
                visit(*lhs);
                visit(*rhs);
            }
            Self::Mux {
                condition,
                then_value,
                else_value,
            } => {
                visit(*condition);
                visit(*then_value);
                visit(*else_value);
            }
            Self::Slice { source, .. } => visit(*source),
            Self::Concat { parts } | Self::ProcessPhi { inputs: parts } => {
                parts.iter().copied().for_each(&mut visit);
            }
            Self::DynamicSelect { source, offset, .. } => {
                visit(*source);
                offset.visit_value_operands(&mut visit);
            }
            Self::UpdateRange {
                base,
                offset,
                value,
                ..
            } => {
                visit(*base);
                offset.visit_value_operands(&mut visit);
                visit(*value);
            }
            Self::LoopValue { initial, update } => {
                visit(*initial);
                visit(*update);
            }
        }
    }
}

/// A settled combinational range and the SLT recipe which defines it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombDefinition {
    pub target: ObjectRange,
    pub recipe: CombRecipeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    pub region: RegionId,
    /// Effect-token predecessors. IDs are topological; pure value operands are
    /// represented by the effect kind instead.
    pub predecessors: Vec<EffectId>,
    pub kind: EffectKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectKind {
    StageNextFf {
        process: ProcessId,
        target: ObjectAccess,
        value: ValueId,
        guard: Option<ValueId>,
        priority: usize,
    },
    WritePersistentMemory {
        object: AbsoluteAddr,
        offset: ValueId,
        value: ValueId,
        guard: Option<ValueId>,
    },
    RuntimeEvent {
        site_id: u32,
        arguments: Vec<ValueId>,
        guard: Option<ValueId>,
    },
    Capture {
        site_id: u32,
        arguments: Vec<ValueId>,
        guard: Option<ValueId>,
    },
    TriggerPublication {
        target: ObjectRange,
        old_value: ValueId,
        new_value: ValueId,
    },
    CommitFfState {
        stages: Vec<EffectId>,
    },
    RuntimeObservationBarrier,
}

impl EffectKind {
    /// Visit value operands without allocating a temporary vector per effect.
    pub fn visit_value_operands(&self, mut visit: impl FnMut(ValueId)) {
        match self {
            Self::StageNextFf {
                target,
                value,
                guard,
                ..
            } => {
                visit(*value);
                if let Some(guard) = guard {
                    visit(*guard);
                }
                target.offset.visit_value_operands(&mut visit);
            }
            Self::WritePersistentMemory {
                offset,
                value,
                guard,
                ..
            } => {
                visit(*offset);
                visit(*value);
                if let Some(guard) = guard {
                    visit(*guard);
                }
            }
            Self::RuntimeEvent {
                arguments, guard, ..
            }
            | Self::Capture {
                arguments, guard, ..
            } => {
                arguments.iter().copied().for_each(&mut visit);
                if let Some(guard) = guard {
                    visit(*guard);
                }
            }
            Self::TriggerPublication {
                old_value,
                new_value,
                ..
            } => {
                visit(*old_value);
                visit(*new_value);
            }
            Self::CommitFfState { .. } | Self::RuntimeObservationBarrier => {}
        }
    }
}

/// Complete semantic graph for one event domain.
#[derive(Debug, Clone)]
pub struct EventIr {
    domain: EventDomain,
    comb: Arc<CombGraph>,
    regions: Vec<Region>,
    processes: Vec<Process>,
    blocks: Vec<ControlBlock>,
    values: Vec<Value>,
    effects: Vec<Effect>,
}

impl EventIr {
    pub fn new(domain: EventDomain, comb: Arc<CombGraph>) -> Self {
        Self {
            domain,
            comb,
            regions: vec![Region {
                parent: None,
                kind: RegionKind::EventRoot,
            }],
            processes: Vec::new(),
            blocks: Vec::new(),
            values: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub fn domain(&self) -> &EventDomain {
        &self.domain
    }

    pub fn comb_graph(&self) -> &CombGraph {
        &self.comb
    }

    pub fn root_region(&self) -> RegionId {
        RegionId(0)
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    pub fn processes(&self) -> &[Process] {
        &self.processes
    }

    pub fn blocks(&self) -> &[ControlBlock] {
        &self.blocks
    }

    pub fn values(&self) -> &[Value] {
        &self.values
    }

    pub fn comb_definitions(&self) -> &[CombDefinition] {
        self.comb.definitions()
    }

    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    pub fn add_region(&mut self, parent: RegionId, kind: RegionKind) -> RegionId {
        let id = RegionId(self.regions.len());
        self.regions.push(Region {
            parent: Some(parent),
            kind,
        });
        id
    }

    pub fn add_process(&mut self, source_order: usize) -> ProcessId {
        self.add_process_with_resets(source_order, Vec::new())
    }

    pub fn add_process_with_resets(
        &mut self,
        source_order: usize,
        mut resets: Vec<AbsoluteAddr>,
    ) -> ProcessId {
        resets.sort_unstable();
        resets.dedup();
        let process = ProcessId(self.processes.len());
        let region = self.add_region(self.root_region(), RegionKind::FfProcess(process));
        self.processes.push(Process {
            region,
            source_order,
            resets,
            entry: ControlBlockId(usize::MAX),
            blocks: Vec::new(),
        });
        let entry = self.add_control_block(process);
        self.processes[process.0].entry = entry;
        process
    }

    pub fn add_control_block(&mut self, process: ProcessId) -> ControlBlockId {
        let process_region = self.processes[process.0].region;
        let block = ControlBlockId(self.blocks.len());
        let region = self.add_region(process_region, RegionKind::ControlBlock { process, block });
        self.blocks.push(ControlBlock {
            process,
            region,
            parameters: Vec::new(),
            terminator: None,
        });
        self.processes[process.0].blocks.push(block);
        block
    }

    pub fn add_block_parameter(&mut self, block: ControlBlockId, ty: ValueType) -> ValueId {
        let control_block = &self.blocks[block.0];
        let process = control_block.process;
        let region = control_block.region;
        let index = control_block.parameters.len();
        let value = self.add_value(Value {
            ty,
            scope: ValueScope::Process(process),
            region,
            kind: ValueKind::BlockParameter { block, index },
        });
        self.blocks[block.0].parameters.push(value);
        value
    }

    pub fn set_terminator(&mut self, block: ControlBlockId, terminator: ControlTerminator) {
        let previous = self.blocks[block.0].terminator.replace(terminator);
        assert!(previous.is_none(), "{block} already has a terminator");
    }

    pub fn add_value(&mut self, value: Value) -> ValueId {
        let id = ValueId(self.values.len());
        self.values.push(value);
        id
    }

    pub fn add_effect(&mut self, effect: Effect) -> EffectId {
        let id = EffectId(self.effects.len());
        self.effects.push(effect);
        id
    }

    pub fn process_region(&self, process: ProcessId) -> Option<RegionId> {
        self.processes.get(process.0).map(|process| process.region)
    }

    pub fn verify(&self) -> Result<(), EventIrError> {
        verify::verify(self)
    }
}
