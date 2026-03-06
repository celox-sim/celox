use super::block_opt::optimize_block;
use super::shared::{batch_replace_in_inst, batch_replace_in_terminator};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;

use super::pass_manager::ExecutionUnitPass;

pub(super) struct OptimizeBlocksPass;

impl ExecutionUnitPass for OptimizeBlocksPass {
    fn name(&self) -> &'static str {
        "optimize_blocks"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut replacement_map = HashMap::default();
        let mut block_ids: Vec<_> = eu.blocks.keys().copied().collect();
        block_ids.sort();

        let mut reg_counter: usize = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);

        for id in block_ids {
            let block = eu.blocks.get_mut(&id).unwrap();
            optimize_block(
                block,
                &mut eu.register_map,
                &mut replacement_map,
                &mut reg_counter,
            );
        }

        if replacement_map.is_empty() {
            return;
        }

        // Resolve transitive replacements to avoid chain issues
        let mut final_map = HashMap::default();
        for &from in replacement_map.keys() {
            let mut to = replacement_map[&from];
            let mut depth = 0;
            while let Some(&next_to) = replacement_map.get(&to) {
                if next_to == to || depth > replacement_map.len() {
                    break;
                }
                to = next_to;
                depth += 1;
            }
            final_map.insert(from, to);
        }

        // Batch apply all replacements in a single pass over all blocks
        for block in eu.blocks.values_mut() {
            for p in &mut block.params {
                if let Some(&to) = final_map.get(p) {
                    *p = to;
                }
            }
            for inst in &mut block.instructions {
                batch_replace_in_inst(inst, &final_map);
            }
            batch_replace_in_terminator(&mut block.terminator, &final_map);
        }
    }
}
