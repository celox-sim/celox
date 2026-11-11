//! Demand-driven SSA liveness for GVN's register-pressure profitability check.

use crate::HashMap;
use crate::native::mir::*;

#[derive(Clone, Copy)]
enum LivePoint {
    Entry(usize),
    Exit(usize),
}

enum LiveBlocks {
    Sparse(Box<[usize]>),
    Dense(Box<[u64]>),
}

impl LiveBlocks {
    fn contains(&self, block: usize) -> bool {
        match self {
            Self::Sparse(blocks) => blocks.binary_search(&block).is_ok(),
            Self::Dense(words) => words[block / 64] & (1u64 << (block % 64)) != 0,
        }
    }
}

/// Liveness of the original, unchanged MIR. GVN only asks whether an available
/// expression's leader is live at a candidate block's exit. Defer backward
/// tracing until that query and retain a compact row per queried leader,
/// rather than materializing every live block/value pair up front.
///
/// Each queried value is traced once. Dense lifetimes use one bit per block;
/// short lifetimes retain sorted block indices. Neither representation drops
/// facts or changes GVN's register-pressure profitability decision.
pub(super) struct LiveOut<'a> {
    predecessors: &'a [Vec<usize>],
    definition_blocks: Vec<usize>,
    uses: Vec<Vec<LivePoint>>,
    rows: HashMap<VReg, LiveBlocks>,
    visited_entry: Vec<u32>,
    visited_exit: Vec<u32>,
    work: Vec<LivePoint>,
}

pub(super) fn live_out<'a>(
    func: &MFunction,
    block_indices: &HashMap<BlockId, usize>,
    predecessors: &'a [Vec<usize>],
) -> LiveOut<'a> {
    let value_count = func.vregs.count() as usize;
    let block_count = func.blocks.len();
    let mut definition_blocks = vec![usize::MAX; value_count];
    let mut uses = vec![Vec::new(); value_count];
    for (block_index, block) in func.blocks.iter().enumerate() {
        for phi in &block.phis {
            definition_blocks[phi.dst.0 as usize] = block_index;
        }
        for inst in &block.insts {
            for used in inst.uses() {
                let value = used.0 as usize;
                if definition_blocks[value] != block_index
                    && !matches!(uses[value].last(), Some(LivePoint::Entry(b)) if *b == block_index)
                {
                    uses[value].push(LivePoint::Entry(block_index));
                }
            }
            if let Some(defined) = inst.def() {
                definition_blocks[defined.0 as usize] = block_index;
            }
        }
    }
    for block in &func.blocks {
        for phi in &block.phis {
            for &(predecessor, source) in &phi.sources {
                // A phi input is live at its own predecessor's exit, even
                // when that predecessor is also the value's defining block.
                uses[source.0 as usize].push(LivePoint::Exit(block_indices[&predecessor]));
            }
        }
    }

    LiveOut {
        predecessors,
        definition_blocks,
        uses,
        rows: HashMap::default(),
        visited_entry: vec![u32::MAX; block_count],
        visited_exit: vec![u32::MAX; block_count],
        work: Vec::new(),
    }
}

impl LiveOut<'_> {
    pub(super) fn contains(&mut self, vreg: VReg, block: usize) -> bool {
        if let Some(row) = self.rows.get(&vreg) {
            return row.contains(block);
        }
        let value = vreg.0 as usize;
        if self.uses[value].is_empty() {
            return false;
        }
        self.work.extend(std::mem::take(&mut self.uses[value]));
        let mut blocks = Vec::new();
        while let Some(point) = self.work.pop() {
            match point {
                LivePoint::Entry(block) => {
                    if self.visited_entry[block] == vreg.0 {
                        continue;
                    }
                    self.visited_entry[block] = vreg.0;
                    self.work.extend(
                        self.predecessors[block]
                            .iter()
                            .copied()
                            .map(LivePoint::Exit),
                    );
                }
                LivePoint::Exit(block) => {
                    if self.visited_exit[block] == vreg.0 {
                        continue;
                    }
                    self.visited_exit[block] = vreg.0;
                    blocks.push(block);
                    if self.definition_blocks[value] != block {
                        self.work.push(LivePoint::Entry(block));
                    }
                }
            }
        }
        let word_count = self.predecessors.len().div_ceil(64);
        let row = if word_count * std::mem::size_of::<u64>()
            < blocks.len() * std::mem::size_of::<usize>()
        {
            let mut words = vec![0u64; word_count];
            for block in blocks {
                words[block / 64] |= 1u64 << (block % 64);
            }
            LiveBlocks::Dense(words.into_boxed_slice())
        } else {
            blocks.sort_unstable();
            LiveBlocks::Sparse(blocks.into_boxed_slice())
        };
        let live = row.contains(block);
        self.rows.insert(vreg, row);
        live
    }
}

#[cfg(test)]
mod tests;
