//! Function units ([`crate::FunctionUnit`]) compiled as native functions.
//!
//! A function unit addresses its frame slots at static offsets from a frame
//! its caller chooses. Instruction selection and the MIR passes see the slots
//! as ordinary state objects placed in a window: a range of state offsets
//! past everything the program's state occupies, laid out as the slots lie in
//! the frame. The emitter addresses that range from the frame register
//! instead of the state base, so no other stage needs a notion of frames, and
//! every analysis keeps the slots apart from the state and from each other as
//! it does distinct objects. A call is an unknown write to the state, so no
//! value read from a slot is reused across one.

use celox_design::{FRAME_REGION, InstanceId};
use celox_state_layout::MAX_ACCESS_OVERREACH;

use celox_sir::FrameLayout;

use crate::{
    BasicBlock, ExecutionUnit, HashMap, MemoryLayout, RegionedAbsoluteAddr, STABLE_REGION,
};

/// The instance of the state objects that stand for frame slots. No design
/// has this many instances.
const FRAME_SLOT_INSTANCE: InstanceId = InstanceId(usize::MAX);

/// The state offsets that address the running activation's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameWindow {
    /// The state offset that stands for the frame's first byte.
    pub base: i32,
    /// The bytes of the window: the frame and the most an access reaches
    /// past it.
    pub size: i32,
}

impl FrameWindow {
    /// The frame offset that state offset `offset` addresses, when it lies
    /// in the window.
    pub fn frame_offset(self, offset: i32) -> Option<i32> {
        let relative = offset.checked_sub(self.base)?;
        (0..self.size).contains(&relative).then_some(relative)
    }
}

/// Whether `addr` is a state object that stands for a frame slot.
pub(crate) fn is_frame_slot(addr: &RegionedAbsoluteAddr) -> bool {
    addr.instance_id == FRAME_SLOT_INSTANCE
}

/// The unit of a function whose frame is `frame`, with each slot address
/// replaced by the state object that stands for it, and the layout that
/// places those objects in the frame window.
pub(crate) fn prepare(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    frame: &FrameLayout<RegionedAbsoluteAddr>,
) -> Result<
    (
        ExecutionUnit<RegionedAbsoluteAddr>,
        MemoryLayout,
        FrameWindow,
    ),
    String,
> {
    let state_end = layout
        .merged_total_size
        .checked_add(layout.triggered_bits_total_size)
        .ok_or("the state size overflows")?;
    // Accesses of the state reach at most MAX_ACCESS_OVERREACH bytes past
    // it, so none of them starts in the window. The base keeps the alignment
    // the slots have relative to a 64-byte boundary of the frame.
    let base = state_end
        .checked_add(MAX_ACCESS_OVERREACH)
        .map(|end| end.next_multiple_of(64))
        .ok_or("the frame window overflows")?;
    let size = frame
        .size
        .checked_add(MAX_ACCESS_OVERREACH)
        .ok_or("the frame window overflows")?;
    let window = FrameWindow {
        base: i32::try_from(base).map_err(|_| "the frame window exceeds i32")?,
        size: i32::try_from(size).map_err(|_| "the frame exceeds i32")?,
    };
    base.checked_add(size)
        .and_then(|end| i32::try_from(end).ok())
        .ok_or("the frame window exceeds i32")?;

    let mut layout = layout.clone();
    let mut objects = HashMap::default();
    for (index, slot) in frame.slots.iter().enumerate() {
        let object = RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: FRAME_SLOT_INSTANCE,
            var_id: celox_design::StateObjectId(
                u32::try_from(index).map_err(|_| "too many frame slots")?,
            ),
        };
        let absolute = object.absolute_addr();
        if layout.offsets.contains_key(&absolute) {
            return Err(format!("frame slot object {absolute} is a state object"));
        }
        layout.offsets.insert(absolute, base + slot.offset);
        layout.widths.insert(absolute, slot.width);
        layout.is_4states.insert(absolute, slot.four_state);
        objects.insert(slot.addr, object);
    }
    let map = |addr: &RegionedAbsoluteAddr| {
        if addr.region == FRAME_REGION {
            *objects
                .get(addr)
                .expect("the verifier keeps frame accesses to the function's own slots")
        } else {
            *addr
        }
    };
    let unit = ExecutionUnit {
        entry_block_id: unit.entry_block_id,
        blocks: unit
            .blocks
            .iter()
            .map(|(&id, block)| {
                (
                    id,
                    BasicBlock {
                        id: block.id,
                        params: block.params.clone(),
                        instructions: block
                            .instructions
                            .iter()
                            .map(|instruction| instruction.map_addr(map))
                            .collect(),
                        terminator: block.terminator.clone(),
                    },
                )
            })
            .collect(),
        register_map: unit.register_map.clone(),
    };
    Ok((unit, layout, window))
}

/// `unit` with every block that holds a `Call` split after each call, or
/// `None` when it holds none.
///
/// Instruction selection plans its fusions (load caches, packed compares
/// and stores, slice origins) within one block from the loads and stores
/// they see, and a call writes state that its instruction does not name.
/// Ending a block at each call keeps every such plan on one side of it.
pub(crate) fn split_at_calls(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Option<ExecutionUnit<RegionedAbsoluteAddr>> {
    let is_call = |instruction: &crate::SIRInstruction<RegionedAbsoluteAddr>| {
        matches!(instruction, crate::SIRInstruction::Call { .. })
    };
    if !unit
        .blocks
        .values()
        .any(|block| block.instructions.iter().any(is_call))
    {
        return None;
    }
    let mut next_id = unit.blocks.keys().map(|id| id.0).max().unwrap_or(0) + 1;
    let mut blocks = crate::HashMap::default();
    let mut ids = unit.blocks.keys().copied().collect::<Vec<_>>();
    ids.sort_unstable();
    for id in ids {
        let block = &unit.blocks[&id];
        let mut current = BasicBlock {
            id,
            params: block.params.clone(),
            instructions: Vec::new(),
            terminator: crate::SIRTerminator::Return,
        };
        for instruction in &block.instructions {
            current.instructions.push(instruction.clone());
            if is_call(instruction) {
                let rest = crate::BlockId(next_id);
                next_id += 1;
                current.terminator = crate::SIRTerminator::Jump(rest, Vec::new());
                let done = std::mem::replace(
                    &mut current,
                    BasicBlock {
                        id: rest,
                        params: Vec::new(),
                        instructions: Vec::new(),
                        terminator: crate::SIRTerminator::Return,
                    },
                );
                blocks.insert(done.id, done);
            }
        }
        current.terminator = block.terminator.clone();
        blocks.insert(current.id, current);
    }
    Some(ExecutionUnit {
        entry_block_id: unit.entry_block_id,
        blocks,
        register_map: unit.register_map.clone(),
    })
}
