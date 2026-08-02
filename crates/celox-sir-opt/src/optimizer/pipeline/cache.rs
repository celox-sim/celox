//! Exact-equivalence cache for optimizing repeated execution-unit groups once.

use super::*;

pub(super) fn optimize_unit_groups_cached(
    groups: &mut crate::HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    let timing = std::env::var_os("CELOX_PASS_TIMING").is_some();
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

    fn fingerprint(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        units.len().hash(&mut hasher);
        for unit in units {
            unit.entry_block_id.hash(&mut hasher);

            let mut block_ids = unit.blocks.keys().copied().collect::<Vec<_>>();
            block_ids.sort_unstable();
            block_ids.len().hash(&mut hasher);
            for block_id in block_ids {
                let block = &unit.blocks[&block_id];
                block_id.hash(&mut hasher);
                block.params.hash(&mut hasher);
                block.instructions.hash(&mut hasher);
                block.terminator.hash(&mut hasher);
            }

            let mut registers = unit.register_map.iter().collect::<Vec<_>>();
            registers.sort_unstable_by_key(|(register, _)| **register);
            registers.len().hash(&mut hasher);
            for (register, ty) in registers {
                register.hash(&mut hasher);
                ty.hash(&mut hasher);
            }
        }
        hasher.finish()
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
        eprintln!(
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
            for eu in units {
                passes.run(eu, options);
            }
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
        eprintln!(
            "[group-cache-timing] total groups={} elapsed={:?}",
            groups.len(),
            start.elapsed()
        );
    }
}
