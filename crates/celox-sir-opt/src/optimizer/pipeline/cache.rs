//! Exact-equivalence cache for optimizing repeated execution-unit groups once.

use super::*;

fn fingerprint(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) -> u64 {
    fn hash_item(value: impl Hash) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    units.len().hash(&mut hasher);
    for unit in units {
        unit.entry_block_id.hash(&mut hasher);

        unit.blocks.len().hash(&mut hasher);
        let mut block_xor = 0u64;
        let mut block_sum = 0u64;
        for (&block_id, block) in &unit.blocks {
            let item = hash_item((
                block_id,
                &block.params,
                &block.instructions,
                &block.terminator,
            ));
            block_xor ^= item;
            block_sum = block_sum.wrapping_add(item);
        }
        block_xor.hash(&mut hasher);
        block_sum.hash(&mut hasher);

        unit.register_map.len().hash(&mut hasher);
        let mut register_xor = 0u64;
        let mut register_sum = 0u64;
        for (&register, ty) in &unit.register_map {
            let item = hash_item((register, ty));
            register_xor ^= item;
            register_sum = register_sum.wrapping_add(item);
        }
        register_xor.hash(&mut hasher);
        register_sum.hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn optimize_unit_groups_cached(
    groups: &mut crate::HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    let timing = options.optimize_options.diagnostics.pass_timing;
    let total_start = timing.then(crate::timing::now);
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct UnitShape {
        entry: BlockId,
        blocks: usize,
        registers: usize,
        instructions: usize,
    }

    fn shape(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) -> Vec<UnitShape> {
        units
            .iter()
            .map(|unit| UnitShape {
                entry: unit.entry_block_id,
                blocks: unit.blocks.len(),
                registers: unit.register_map.len(),
                instructions: unit
                    .blocks
                    .values()
                    .map(|block| block.instructions.len())
                    .sum(),
            })
            .collect()
    }

    struct EquivalenceClass {
        representative: AbsoluteAddr,
        aliases: Vec<AbsoluteAddr>,
        shape: Vec<UnitShape>,
        fingerprint: u64,
    }

    // Establish exact source equivalence before mutating any representative.
    // Keeping a cloned pre-optimization group in the old cache doubled the
    // live SIR and copied every unique group even when it had no alias.
    let mut addresses = groups.keys().copied().collect::<Vec<_>>();
    addresses.sort_unstable();
    let mut classes: Vec<EquivalenceClass> = Vec::new();
    for address in addresses {
        let candidate_shape = shape(&groups[&address]);
        let candidate_fingerprint = fingerprint(&groups[&address]);
        if let Some(class) = classes.iter_mut().find(|class| {
            class.shape == candidate_shape
                && class.fingerprint == candidate_fingerprint
                && groups[&class.representative] == groups[&address]
        }) {
            class.aliases.push(address);
        } else {
            classes.push(EquivalenceClass {
                representative: address,
                aliases: Vec::new(),
                shape: candidate_shape,
                fingerprint: candidate_fingerprint,
            });
        }
    }
    let aliases = classes
        .iter()
        .map(|class| class.aliases.len())
        .sum::<usize>();
    if let Some(start) = total_start {
        tracing::debug!(
            "[group-cache-timing] classify groups={} classes={} aliases={} elapsed={:?}",
            groups.len(),
            classes.len(),
            aliases,
            start.elapsed()
        );
    }

    for class in classes {
        {
            let units = groups
                .get_mut(&class.representative)
                .expect("equivalence-class representative must exist");
            passes.run_parallel(units, options);
        }
        if class.aliases.is_empty() {
            continue;
        }
        let optimized = groups[&class.representative].clone();
        for alias in class.aliases {
            *groups
                .get_mut(&alias)
                .expect("equivalence-class alias must exist") = optimized.clone();
        }
    }
    if let Some(start) = total_start {
        tracing::debug!(
            "[group-cache-timing] total groups={} elapsed={:?}",
            groups.len(),
            start.elapsed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::fingerprint;
    use crate::HashMap;
    use crate::ir::{
        BasicBlock, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction,
        SIRTerminator, SIRValue, UnaryOp,
    };

    fn unit(reverse: bool) -> ExecutionUnit<crate::ir::RegionedAbsoluteAddr> {
        let entry = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        };
        let exit = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Unary(
                RegisterId(1),
                UnaryOp::Ident,
                RegisterId(0),
            )],
            terminator: SIRTerminator::Return,
        };
        let mut blocks = HashMap::default();
        let mut register_map = HashMap::default();
        let logic = RegisterType::Logic { width: 1 };
        if reverse {
            blocks.insert(BlockId(1), exit);
            blocks.insert(BlockId(0), entry);
            register_map.insert(RegisterId(1), logic.clone());
            register_map.insert(RegisterId(0), logic);
        } else {
            blocks.insert(BlockId(0), entry);
            blocks.insert(BlockId(1), exit);
            register_map.insert(RegisterId(0), logic.clone());
            register_map.insert(RegisterId(1), logic);
        }
        ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map,
        }
    }

    #[test]
    fn fingerprint_is_independent_of_hash_map_iteration_order() {
        assert_eq!(fingerprint(&[unit(false)]), fingerprint(&[unit(true)]));
    }
}
