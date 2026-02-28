use super::block_opt::optimize_block;
use super::shared::replace_register_uses;
use crate::HashMap;
use crate::ir::{ExecutionUnit, RegionedAbsoluteAddr};
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

        for id in block_ids {
            let block = eu.blocks.get_mut(&id).unwrap();
            optimize_block(block, &mut eu.register_map, &mut replacement_map);
        }

        use crate::HashSet;
        // Resolve transitive replacements to avoid chain issues during iteration
        let mut final_map = HashMap::default();
        for &from in replacement_map.keys() {
            let mut to = replacement_map[&from];
            let mut visited = HashSet::default();
            visited.insert(from);
            while let Some(&next_to) = replacement_map.get(&to) {
                if !visited.insert(next_to) {
                    break; // Cycle detected (should not happen in valid SIRT)
                }
                to = next_to;
            }
            final_map.insert(from, to);
        }

        for (from, to) in final_map {
            replace_register_uses(eu, from, to);
        }
    }
}
