mod bit_copy;
mod bit_permutation;
mod cfg;
mod constants;
mod gvn;
mod masks;
mod memory;
mod memory_forward;
mod post_regalloc;
mod selection;

use super::gvn::{GvnMemoryVariable, GvnMemoryVersion, compute_gvn_load_versions};
use super::memory_forward::{AvailableStores, clamped_key_range, find_best_covering_value};
use super::selection::select_values;
use super::*;

fn make_func(insts: Vec<MInst>, vreg_count: u32) -> MFunction {
    let mut vregs = VRegAllocator::new();
    for _ in 0..vreg_count {
        vregs.alloc();
    }
    let spill_descs = (0..vreg_count).map(|_| SpillDesc::transient()).collect();
    let mut func = MFunction::new(vregs, spill_descs);
    let mut block = MBlock::new(BlockId(0));
    block.insts = insts;
    func.push_block(block);
    func
}
