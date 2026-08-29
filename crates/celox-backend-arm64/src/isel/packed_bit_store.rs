//! AArch64-owned recognition of packed single-bit store sequences.

use crate::{
    BasicBlock, HashMap, HashSet, MemoryLayout, RegionedAbsoluteAddr, RegisterId, RegisterType,
    SIRInstruction, SIROffset,
};

#[derive(Debug, Clone)]
pub(super) struct PackedBitStorePlan {
    pub(super) source: RegisterId,
    pub(super) address: RegionedAbsoluteAddr,
    pub(super) first_lane: usize,
    pub(super) lane_count: usize,
}

#[derive(Debug, Default)]
pub(super) struct PackedBitStorePlans {
    pub(super) roots: HashMap<usize, PackedBitStorePlan>,
    pub(super) skip_indices: HashSet<usize>,
}

pub(super) fn find_packed_bit_store_plans(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    layout: &MemoryLayout,
) -> PackedBitStorePlans {
    let mut plans = PackedBitStorePlans::default();
    let mut index = 0usize;
    while index + 1 < block.instructions.len() {
        let SIRInstruction::Slice(_, source, first_lane, 1) = block.instructions[index] else {
            index += 1;
            continue;
        };
        let SIRInstruction::Store(
            address,
            SIROffset::Static(first_store_lane),
            1,
            first_slice,
            ref triggers,
            ref captures,
        ) = block.instructions[index + 1]
        else {
            index += 1;
            continue;
        };
        let SIRInstruction::Slice(first_slice_definition, _, _, _) = block.instructions[index]
        else {
            unreachable!();
        };
        let Some(array) = layout.unpacked_arrays.get(&address.absolute_addr()) else {
            index += 1;
            continue;
        };
        if first_slice != first_slice_definition
            || first_store_lane != first_lane
            || !triggers.is_empty()
            || !captures.is_empty()
            || array.element_width != 1
            || array.element_stride != 1
        {
            index += 1;
            continue;
        }

        let mut lane_count = 0usize;
        while index + lane_count * 2 + 1 < block.instructions.len() {
            let slice_index = index + lane_count * 2;
            let store_index = slice_index + 1;
            let SIRInstruction::Slice(slice, lane_source, lane, 1) =
                block.instructions[slice_index]
            else {
                break;
            };
            let SIRInstruction::Store(
                lane_address,
                SIROffset::Static(store_lane),
                1,
                stored,
                ref lane_triggers,
                ref lane_captures,
            ) = block.instructions[store_index]
            else {
                break;
            };
            if lane_source != source
                || lane != first_lane + lane_count
                || lane_address != address
                || store_lane != lane
                || stored != slice
                || !lane_triggers.is_empty()
                || !lane_captures.is_empty()
            {
                break;
            }
            lane_count += 1;
        }
        let source_width = register_types.get(&source).map(RegisterType::width);
        if lane_count >= 8
            && lane_count.is_multiple_of(8)
            && lane_count <= 64
            && first_lane.is_multiple_of(8)
            && source_width.is_some_and(|width| first_lane + lane_count <= width)
            && first_lane + lane_count <= array.element_count
        {
            let plan = PackedBitStorePlan {
                source,
                address,
                first_lane,
                lane_count,
            };
            plans.skip_indices.extend(index..index + lane_count * 2);
            plans.roots.insert(index, plan);
            index += lane_count * 2;
        } else {
            index += 1;
        }
    }
    plans
}
