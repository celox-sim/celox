use super::block_opt::{optimize_block, schedule_instructions};
use super::shared::{batch_replace_in_inst, batch_replace_in_terminator};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;
use std::sync::Arc;

use super::pass_manager::ExecutionUnitPass;

pub(super) struct OptimizeBlocksPass {
    /// When true, skip the final `schedule_instructions` inside `optimize_block`
    /// because a `ReschedulePass` will run afterward in the same pipeline.
    pub skip_final_schedule: bool,
    /// Element widths for arrays whose element boundaries must survive until
    /// backend layout selection. Coalescing within one element remains legal;
    /// only cross-element packed accesses are forbidden.
    pub element_widths: Arc<HashMap<RegionedAbsoluteAddr, usize>>,
}

impl ExecutionUnitPass for OptimizeBlocksPass {
    fn name(&self) -> &'static str {
        "optimize_blocks"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let skip_final_schedule = self.skip_final_schedule;

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
                true,
                &self.element_widths,
            );
        }

        if !replacement_map.is_empty() {
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

        // Scheduling must see the final operand graph. Scheduling first and
        // applying unit-wide replacements afterward can replace an operand
        // with a value whose definition was moved below that use.
        if !skip_final_schedule {
            for block in eu.blocks.values_mut() {
                schedule_instructions(&mut block.instructions, 8);
            }
        }
    }
}
