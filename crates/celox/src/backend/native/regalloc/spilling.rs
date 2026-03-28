//! Spilling phase: Braun & Hack extended MIN algorithm.
//!
//! Walks the CFG in reverse post order, applying the MIN algorithm per block.
//! Inserts spill/reload MIR instructions to ensure register pressure ≤ k.
//!
//! Domain-specific optimizations:
//! - **Rematerialization**: constants (SpillKind::Remat) are never stored;
//!   reloads become LoadImm.
//! - **SimState reload**: values loaded from simulation state (SpillKind::SimState)
//!   are reloaded from their original location, not from the stack.
//! - **Spill slot allocation**: each spilled value gets a unique stack slot.

use std::collections::{BTreeMap, BTreeSet};

use crate::backend::native::mir::*;

use super::analysis::{self, AnalysisResult};

// ────────────────────────────────────────────────────────────────
// Spill slot allocator
// ────────────────────────────────────────────────────────────────

/// Assigns unique stack frame offsets to spilled VRegs.
pub(super) struct SpillSlotAllocator {
    /// VReg → stack offset (bytes from frame base)
    pub(super) slots: BTreeMap<VReg, i32>,
    /// Next available offset
    pub(super) next_offset: i32,
}

impl SpillSlotAllocator {
    pub(super) fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            next_offset: 0,
        }
    }

    /// Get or allocate a spill slot for a VReg. Returns the byte offset.
    pub(super) fn slot_for(&mut self, vreg: VReg) -> i32 {
        *self.slots.entry(vreg).or_insert_with(|| {
            let off = self.next_offset;
            self.next_offset += 8; // all slots are 8 bytes (i64)
            off
        })
    }

    /// Total bytes of spill slots allocated.
    pub(super) fn total_size(&self) -> i32 {
        self.next_offset
    }
}

// ────────────────────────────────────────────────────────────────
// Spill/reload instruction generation
// ────────────────────────────────────────────────────────────────

/// Generate a spill instruction for `vreg` based on its SpillDesc.
/// Returns None if no spill store is needed (remat, store-back-only).
pub(super) fn make_spill(
    vreg: VReg,
    func: &MFunction,
    slots: &mut SpillSlotAllocator,
) -> Option<MInst> {
    let desc = func.spill_desc(vreg);
    match desc.map(|d| &d.kind) {
        Some(SpillKind::Remat { .. }) => {
            // Rematerializable: no spill store needed
            None
        }
        Some(SpillKind::SimState { .. }) if desc.unwrap().spill_cost == 0 => {
            // Store-back-only: value is already in simulation state
            None
        }
        Some(SpillKind::SimState { .. }) => {
            // Value was loaded from sim state but may have been modified;
            // spill to stack to be safe
            let offset = slots.slot_for(vreg);
            Some(MInst::Store {
                base: BaseReg::StackFrame,
                offset,
                src: vreg,
                size: OpSize::S64,
            })
        }
        Some(SpillKind::Stack) | None => {
            // Transient value: spill to stack
            let offset = slots.slot_for(vreg);
            Some(MInst::Store {
                base: BaseReg::StackFrame,
                offset,
                src: vreg,
                size: OpSize::S64,
            })
        }
    }
}

/// Generate a reload instruction for `vreg` based on its SpillDesc.
pub(super) fn make_reload(
    vreg: VReg,
    func: &MFunction,
    slots: &mut SpillSlotAllocator,
) -> MInst {
    let desc = func.spill_desc(vreg);
    match desc.map(|d| &d.kind) {
        Some(SpillKind::Remat { value }) => {
            // Rematerialize: just reload the constant
            MInst::LoadImm {
                dst: vreg,
                value: *value,
            }
        }
        Some(SpillKind::SimState {
            bit_offset,
            width_bits,
            ..
        }) if desc.unwrap().spill_cost == 0 => {
            // Store-back-only: reload from simulation state.
            // The original Load instruction had the correct offset;
            // we reconstruct it from SpillDesc.
            let byte_offset = (bit_offset / 8) as i32;
            let op_size = match *width_bits {
                0..=8 => OpSize::S8,
                9..=16 => OpSize::S16,
                17..=32 => OpSize::S32,
                _ => OpSize::S64,
            };
            MInst::Load {
                dst: vreg,
                base: BaseReg::SimState,
                offset: byte_offset,
                size: op_size,
            }
        }
        _ => {
            // Reload from stack slot
            let offset = slots.slot_for(vreg);
            MInst::Load {
                dst: vreg,
                base: BaseReg::StackFrame,
                offset,
                size: OpSize::S64,
            }
        }
    }
}
