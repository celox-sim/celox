//! Splitting combinational execution units for compilation.

use super::*;

pub(super) fn offset_registers(offset: &SIROffset, registers: &mut Vec<RegisterId>) {
    match offset {
        SIROffset::Dynamic(register) => registers.push(*register),
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            registers.push(*index);
            registers.extend(dynamic_bit_offset);
        }
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
    }
}

pub(super) fn instruction_registers<A>(instruction: &SIRInstruction<A>) -> Vec<RegisterId> {
    let mut registers = Vec::new();
    instruction.for_each_use(|register| registers.push(register));
    registers
}

pub(super) fn comb_block_execution_order<A>(unit: &ExecutionUnit<A>) -> Vec<BlockId> {
    fn visit<A>(
        unit: &ExecutionUnit<A>,
        block_id: BlockId,
        visited: &mut HashSet<BlockId>,
        postorder: &mut Vec<BlockId>,
    ) {
        if !visited.insert(block_id) {
            return;
        }
        for successor in celox_sir::cfg::terminator_successors(&unit.blocks[&block_id].terminator) {
            visit(unit, successor, visited, postorder);
        }
        postorder.push(block_id);
    }

    let mut visited = HashSet::default();
    let mut postorder = Vec::with_capacity(unit.blocks.len());
    visit(unit, unit.entry_block_id, &mut visited, &mut postorder);
    postorder.reverse();
    postorder
}

pub(super) fn is_comb_runtime_effect(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    instruction.is_host_interaction()
}

/// Put the runtime effects of `unit` between its split `store_units`, each
/// where it ran among the stores. The parts keep the unit's other
/// instructions, so it must make no call (see `split_comb_execution_unit`),
/// which would run in every part.
pub(super) fn interleave_comb_runtime_effects(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    ordered_stores: &[(BlockId, usize)],
    store_units: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
) -> Vec<ExecutionUnit<RegionedAbsoluteAddr>> {
    debug_assert!(
        unit.blocks
            .values()
            .flat_map(|block| &block.instructions)
            .all(|instruction| instruction.call_effects().is_none()),
        "a unit with calls is not split"
    );
    let ordered_sites = comb_block_execution_order(unit)
        .into_iter()
        .flat_map(|block_id| {
            (0..unit.blocks[&block_id].instructions.len()).map(move |index| (block_id, index))
        })
        .collect::<Vec<_>>();
    let positions = ordered_sites
        .iter()
        .enumerate()
        .map(|(position, &site)| (site, position))
        .collect::<HashMap<_, _>>();
    let mut effect_groups = vec![Vec::new(); ordered_stores.len() + 1];
    for site in ordered_sites {
        if !is_comb_runtime_effect(&unit.blocks[&site.0].instructions[site.1]) {
            continue;
        }
        let boundary = ordered_stores
            .iter()
            .filter(|store| positions[store] < positions[&site])
            .count();
        effect_groups[boundary].push(site);
    }
    if effect_groups.iter().all(Vec::is_empty) {
        return store_units;
    }

    let mut result = Vec::with_capacity(store_units.len() + effect_groups.len());
    let mut stores = store_units.into_iter();
    for (boundary, group) in effect_groups.into_iter().enumerate() {
        if !group.is_empty() {
            let group = group.into_iter().collect::<HashSet<_>>();
            let mut events = unit.clone();
            for (block_id, block) in &mut events.blocks {
                block.instructions = std::mem::take(&mut block.instructions)
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, instruction)| {
                        let site = (*block_id, index);
                        if matches!(
                            instruction,
                            SIRInstruction::Store(..) | SIRInstruction::Commit(..)
                        ) {
                            None
                        } else if is_comb_runtime_effect(&instruction) {
                            group.contains(&site).then_some(instruction)
                        } else {
                            Some(instruction)
                        }
                    })
                    .collect();
            }
            result.push(events);
        }
        if boundary < ordered_stores.len() {
            result.push(stores.next().unwrap());
        }
    }
    result
}

pub(super) fn split_comb_execution_unit(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> Vec<ExecutionUnit<RegionedAbsoluteAddr>> {
    // A unit is split at its Stores and Commits, each part recomputing what
    // its store needs. A call is a writer the parts could neither split at
    // nor repeat, so a unit that makes one stays whole: overrides are then
    // reapplied after the unit rather than after each of its stores.
    if unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| instruction.call_effects().is_some())
    {
        return vec![unit.clone()];
    }
    if unit.blocks.len() != 1 {
        let definitions = unit
            .blocks
            .iter()
            .flat_map(|(block_id, block)| {
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .filter_map(|(index, instruction)| {
                        instruction
                            .defined_register()
                            .map(|register| (register, (*block_id, index)))
                    })
            })
            .collect::<HashMap<_, _>>();
        let store_sites = comb_block_execution_order(unit)
            .into_iter()
            .flat_map(|block_id| {
                let block = &unit.blocks[&block_id];
                block
                    .instructions
                    .iter()
                    .enumerate()
                    .filter(|(_, instruction)| {
                        matches!(
                            instruction,
                            SIRInstruction::Store(..) | SIRInstruction::Commit(..)
                        )
                    })
                    .map(move |(index, _)| (block_id, index))
            })
            .collect::<Vec<_>>();
        if store_sites.is_empty() {
            return vec![unit.clone()];
        }

        let instruction_at = |site: (BlockId, usize)| &unit.blocks[&site.0].instructions[site.1];
        let register_dependencies = store_sites
            .iter()
            .copied()
            .map(|store_site| {
                let mut dependencies = HashSet::default();
                let mut pending = instruction_registers(instruction_at(store_site));
                while let Some(register) = pending.pop() {
                    if !dependencies.insert(register) {
                        continue;
                    }
                    if let Some(&definition) = definitions.get(&register) {
                        pending.extend(instruction_registers(instruction_at(definition)));
                    }
                }
                (store_site, dependencies)
            })
            .collect::<HashMap<_, _>>();
        let store_source = |site| match instruction_at(site) {
            SIRInstruction::Store(_, _, _, source, _, _) => Some(*source),
            _ => None,
        };
        let mut remaining = store_sites;
        let mut ordered_stores = Vec::with_capacity(remaining.len());
        while !remaining.is_empty() {
            let next = remaining
                .iter()
                .position(|candidate| {
                    let candidate_source = store_source(*candidate);
                    !remaining.iter().any(|predecessor| {
                        if predecessor == candidate {
                            return false;
                        }
                        store_source(*predecessor).is_some_and(|source| {
                            Some(source) != candidate_source
                                && register_dependencies[candidate].contains(&source)
                        })
                    })
                })
                .unwrap_or(0);
            ordered_stores.push(remaining.remove(next));
        }

        let split = ordered_stores
            .iter()
            .enumerate()
            .map(|(order, &target)| {
                let reloads = ordered_stores[..order]
                    .iter()
                    .filter_map(|&prior_site| {
                        let SIRInstruction::Store(address, offset, bits, source, _, _) =
                            instruction_at(prior_site)
                        else {
                            return None;
                        };
                        (register_dependencies[&target].contains(source)
                            && store_source(target) != Some(*source)
                            && unit.register_map[source].width() == *bits)
                            .then(|| (*source, (*address, offset.clone(), *bits)))
                    })
                    .collect::<HashMap<_, _>>();
                let mut extracted = unit.clone();
                for (block_id, block) in &mut extracted.blocks {
                    block.instructions = std::mem::take(&mut block.instructions)
                        .into_iter()
                        .enumerate()
                        .filter_map(|(index, instruction)| {
                            let site = (*block_id, index);
                            match instruction {
                                _ if instruction.memory_write().is_some() => {
                                    (site == target).then_some(instruction)
                                }
                                _ if instruction.has_side_effects() => None,
                                _ => {
                                    if let Some(register) = instruction.defined_register()
                                        && let Some((address, offset, bits)) =
                                            reloads.get(&register)
                                    {
                                        Some(SIRInstruction::Load(
                                            register,
                                            *address,
                                            offset.clone(),
                                            *bits,
                                        ))
                                    } else {
                                        Some(instruction)
                                    }
                                }
                            }
                        })
                        .collect();
                }
                extracted
            })
            .collect::<Vec<_>>();

        return interleave_comb_runtime_effects(unit, &ordered_stores, split);
    }
    let block = &unit.blocks[&unit.entry_block_id];
    if !block.params.is_empty() || block.terminator != SIRTerminator::Return {
        return vec![unit.clone()];
    }

    let definitions = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            instruction
                .defined_register()
                .map(|register| (register, index))
        })
        .collect::<HashMap<_, _>>();
    let store_indices = block
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| {
            matches!(
                instruction,
                SIRInstruction::Store(..) | SIRInstruction::Commit(..)
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    if store_indices.is_empty() {
        return vec![unit.clone()];
    }

    let register_dependencies = store_indices
        .iter()
        .copied()
        .map(|store_index| {
            let mut dependencies = HashSet::default();
            let mut pending = instruction_registers(&block.instructions[store_index]);
            while let Some(register) = pending.pop() {
                if !dependencies.insert(register) {
                    continue;
                }
                if let Some(&definition) = definitions.get(&register) {
                    pending.extend(instruction_registers(&block.instructions[definition]));
                }
            }
            (store_index, dependencies)
        })
        .collect::<HashMap<_, _>>();
    let store_source = |index| match &block.instructions[index] {
        SIRInstruction::Store(_, _, _, source, _, _) => Some(*source),
        _ => None,
    };
    let mut remaining = store_indices.clone();
    let mut ordered_stores = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let next = remaining
            .iter()
            .position(|candidate| {
                let candidate_source = store_source(*candidate);
                !remaining.iter().any(|predecessor| {
                    if predecessor == candidate {
                        return false;
                    }
                    store_source(*predecessor).is_some_and(|source| {
                        Some(source) != candidate_source
                            && register_dependencies[candidate].contains(&source)
                    })
                })
            })
            .unwrap_or(0);
        ordered_stores.push(remaining.remove(next));
    }

    let split = ordered_stores
        .iter()
        .enumerate()
        .map(|(order, &store_index)| {
            let reloads = ordered_stores[..order]
                .iter()
                .filter_map(|&prior_index| {
                    let SIRInstruction::Store(address, offset, bits, source, _, _) =
                        &block.instructions[prior_index]
                    else {
                        return None;
                    };
                    (register_dependencies[&store_index].contains(source)
                        && store_source(store_index) != Some(*source)
                        && unit.register_map[source].width() == *bits)
                        .then(|| (*source, (*address, offset.clone(), *bits)))
                })
                .collect::<HashMap<_, _>>();
            let mut prefix = HashSet::<usize>::default();
            let mut pending = instruction_registers(&block.instructions[store_index]);
            while let Some(register) = pending.pop() {
                let Some(&definition) = definitions.get(&register) else {
                    continue;
                };
                if !prefix.insert(definition) {
                    continue;
                }
                if let Some((_, offset, _)) = reloads.get(&register) {
                    offset_registers(offset, &mut pending);
                    continue;
                }
                pending.extend(instruction_registers(&block.instructions[definition]));
            }
            let mut prefix = prefix.into_iter().collect::<Vec<_>>();
            prefix.sort_unstable();
            let mut instructions = prefix
                .into_iter()
                .map(|index| {
                    let instruction = &block.instructions[index];
                    if let Some(register) = instruction.defined_register()
                        && let Some((address, offset, bits)) = reloads.get(&register)
                    {
                        return SIRInstruction::Load(register, *address, offset.clone(), *bits);
                    }
                    instruction.clone()
                })
                .collect::<Vec<_>>();
            instructions.push(block.instructions[store_index].clone());
            let split_block = celox_sir::BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions,
                terminator: SIRTerminator::Return,
            };
            ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(BlockId(0), split_block)].into_iter().collect(),
                register_map: unit.register_map.clone(),
            }
        })
        .collect::<Vec<_>>();
    let ordered_store_sites = ordered_stores
        .iter()
        .map(|&index| (unit.entry_block_id, index))
        .collect::<Vec<_>>();
    interleave_comb_runtime_effects(unit, &ordered_store_sites, split)
}
