//! Must-write analysis of an imperative combinational body, before its guard.
use super::*;
use num_traits::{ToPrimitive, Zero};

pub(super) fn retains_state(
    unit: &ExecutionUnit<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
    widths: &HashMap<SourceVarId, usize>,
) -> bool {
    retained_state(unit, targets, widths, &HashMap::default(), true)
}

pub(super) fn retains_state_with_aliases(
    unit: &ExecutionUnit<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
    widths: &HashMap<SourceVarId, usize>,
    canonical: &HashMap<SourceVarId, SourceVarId>,
) -> bool {
    retained_state(unit, targets, widths, canonical, true)
}

/// Whether any read of these temporary slots can observe their entry values.
/// Unlike combinational outputs, scratch need not be written on paths that
/// never use it (for example, a loop body skipped on a zero-trip loop).
pub(super) fn reads_prior_state(
    unit: &ExecutionUnit<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
    widths: &HashMap<SourceVarId, usize>,
) -> bool {
    retained_state(unit, targets, widths, &HashMap::default(), false)
}

fn retained_state(
    unit: &ExecutionUnit<RegionedVarAddr>,
    targets: &[VarAtomBase<SourceVarId>],
    widths: &HashMap<SourceVarId, usize>,
    canonical: &HashMap<SourceVarId, SourceVarId>,
    require_complete_outputs: bool,
) -> bool {
    let canon = |id| canonical.get(&id).copied().unwrap_or(id);
    let mask = |offset: usize, width: usize, limit: usize| {
        if offset >= limit {
            return BigUint::zero();
        }
        let width = width.min(limit - offset);
        ((BigUint::from(1u8) << width) - 1u8) << offset
    };
    let mut all: HashMap<SourceVarId, BigUint> = HashMap::default();
    let mut canon_widths: HashMap<SourceVarId, usize> = HashMap::default();
    for target in targets {
        let id = canon(target.id);
        let width = *canon_widths.entry(id).or_insert(widths[&target.id]);
        *all.entry(id).or_default() |= mask(
            target.access.lsb,
            target.access.msb - target.access.lsb + 1,
            width,
        );
    }
    if all.is_empty() {
        return false;
    }
    let successors = |term: &SIRTerminator| -> Vec<BlockId> {
        match term {
            SIRTerminator::Jump(block, _) => vec![*block],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![true_block.0, false_block.0],
            SIRTerminator::Switch { cases, default, .. } => cases
                .iter()
                .map(|case| case.target)
                .chain(std::iter::once(*default))
                .collect(),
            _ => Vec::new(),
        }
    };
    let mut reachable = HashSet::default();
    let mut pending = vec![unit.entry_block_id];
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    while let Some(block) = pending.pop() {
        if !reachable.insert(block) {
            continue;
        }
        for next in successors(&unit.blocks[&block].terminator) {
            predecessors.entry(next).or_default().push(block);
            pending.push(next);
        }
    }
    let mut blocks: Vec<_> = reachable.into_iter().collect();
    blocks.sort_unstable();
    // Constant unpacked indices stay explicit in SIR so physical array layout
    // remains a backend choice. They still describe definite logical writes.
    let constants: HashMap<_, _> = unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| match instruction {
            SIRInstruction::Imm(register, value) if value.mask.is_zero() => {
                value.payload.to_usize().map(|value| (*register, value))
            }
            _ => None,
        })
        .collect();
    let static_offset = |offset: &SIROffset| match offset {
        SIROffset::Static(offset)
        | SIROffset::PackedElements {
            bit_offset: offset, ..
        } => Some(*offset),
        SIROffset::Dynamic(register) => constants.get(register).copied(),
        SIROffset::ElementRun {
            index,
            element_width,
        } => constants
            .get(index)
            .and_then(|index| index.checked_mul(*element_width)),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => constants
            .get(index)
            .and_then(|index| index.checked_mul(*element_width))
            .and_then(|offset| offset.checked_add(*bit_offset))
            .and_then(|offset| match dynamic_bit_offset {
                Some(register) => constants
                    .get(register)
                    .and_then(|extra| offset.checked_add(*extra)),
                None => Some(offset),
            }),
    };
    let transfer = |block: BlockId, written: &mut HashMap<SourceVarId, BigUint>| {
        for inst in &unit.blocks[&block].instructions {
            if let SIRInstruction::Store(address, offset, width, ..) = inst {
                let id = canon(address.var_id);
                if let Some(target) = all.get(&id)
                    && let Some(offset) = static_offset(offset)
                {
                    // `target` is an absolute bitmask, so bits() is the highest
                    // written bit plus one (0xf0 has bits() == 8), not its popcount.
                    *written.get_mut(&id).unwrap() |=
                        mask(offset, *width, target.bits() as usize) & target;
                }
            }
        }
    };
    let empty: HashMap<_, _> = all.keys().map(|&id| (id, BigUint::zero())).collect();
    let mut incoming: HashMap<_, _> = blocks.iter().map(|&block| (block, all.clone())).collect();
    let mut outgoing = incoming.clone();
    loop {
        let mut changed = false;
        for &block in &blocks {
            let mut before = if block == unit.entry_block_id {
                empty.clone()
            } else {
                all.clone()
            };
            if block != unit.entry_block_id {
                for predecessor in &predecessors[&block] {
                    for (id, written) in &mut before {
                        *written &= &outgoing[predecessor][id];
                    }
                }
            }
            let mut after = before.clone();
            transfer(block, &mut after);
            changed |= incoming[&block] != before || outgoing[&block] != after;
            incoming.insert(block, before);
            outgoing.insert(block, after);
        }
        if !changed {
            break;
        }
    }
    for &block in &blocks {
        let mut written = incoming[&block].clone();
        for inst in &unit.blocks[&block].instructions {
            match inst {
                SIRInstruction::Load(_, address, offset, width) => {
                    let id = canon(address.var_id);
                    if let Some(target) = all.get(&id) {
                        let read = static_offset(offset).map_or_else(
                            || target.clone(),
                            |offset| mask(offset, *width, target.bits() as usize) & target,
                        );
                        if (&read & &written[&id]) != read {
                            return true;
                        }
                    }
                }
                SIRInstruction::Store(address, offset, width, ..) => {
                    let id = canon(address.var_id);
                    if let Some(target) = all.get(&id)
                        && let Some(offset) = static_offset(offset)
                    {
                        *written.get_mut(&id).unwrap() |=
                            mask(offset, *width, canon_widths[&id]) & target;
                    }
                }
                _ => (),
            }
        }
        if require_complete_outputs
            && matches!(unit.blocks[&block].terminator, SIRTerminator::Return)
            && all.iter().any(|(id, target)| &written[id] != target)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_run_reads_check_the_selected_elements() {
        for (index_value, expected_retention) in
            [(Some(1u64), false), (Some(0), true), (None, true)]
        {
            let id = SourceVarId(0);
            let address = RegionedVarAddr {
                region: celox_design::STABLE_REGION,
                var_id: id,
            };
            let mut builder = SIRBuilder::new();
            let index = builder.alloc_bit(32, false);
            let value = builder.alloc_bit(8, false);
            if let Some(index_value) = index_value {
                builder.emit(SIRInstruction::Imm(index, SIRValue::new(index_value)));
            }
            builder.emit(SIRInstruction::Imm(value, SIRValue::new(0u8)));
            builder.emit(SIRInstruction::Store(
                address,
                SIROffset::Static(8),
                8,
                value,
                Vec::new(),
                Vec::new(),
            ));
            builder.emit(SIRInstruction::Load(
                value,
                address,
                SIROffset::ElementRun {
                    index,
                    element_width: 8,
                },
                8,
            ));
            builder.emit(SIRInstruction::Store(
                address,
                SIROffset::Static(0),
                8,
                value,
                Vec::new(),
                Vec::new(),
            ));
            builder.seal_block(SIRTerminator::Return);
            let (blocks, register_map, _) = builder.drain();
            let unit = ExecutionUnit {
                blocks,
                register_map,
                entry_block_id: BlockId(0),
            };
            assert_eq!(
                retains_state(
                    &unit,
                    &[VarAtomBase::new(id, 0, 15)],
                    &[(id, 16)].into_iter().collect()
                ),
                expected_retention
            );
        }
    }

    #[test]
    fn out_of_range_accesses_are_clipped_before_mask_allocation() {
        let id = SourceVarId(0);
        let stable = |var_id| RegionedVarAddr {
            region: celox_design::STABLE_REGION,
            var_id,
        };
        let mut builder = SIRBuilder::new();
        let value = builder.alloc_bit(8, false);
        builder.emit(SIRInstruction::Load(
            value,
            stable(id),
            SIROffset::Static(usize::MAX),
            8,
        ));
        builder.emit(SIRInstruction::Store(
            stable(id),
            SIROffset::Static(usize::MAX),
            8,
            value,
            Vec::new(),
            Vec::new(),
        ));
        builder.emit(SIRInstruction::Store(
            stable(id),
            SIROffset::Static(0),
            8,
            value,
            Vec::new(),
            Vec::new(),
        ));
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let unit = ExecutionUnit {
            blocks,
            register_map,
            entry_block_id: BlockId(0),
        };
        assert!(!retains_state(
            &unit,
            &[VarAtomBase::new(id, 0, 7)],
            &[(id, 8)].into_iter().collect()
        ));
    }

    #[test]
    fn canonical_alias_merges_coverage_and_reads() {
        let a = SourceVarId(0);
        let b = SourceVarId(1);
        let y = SourceVarId(2);
        let stable = |var_id| RegionedVarAddr {
            region: celox_design::STABLE_REGION,
            var_id,
        };
        let mut builder = SIRBuilder::new();
        let val = builder.alloc_bit(8, false);
        let sel = builder.alloc_bit(1, false);
        builder.emit(SIRInstruction::Imm(val, SIRValue::new(10u8)));
        builder.emit(SIRInstruction::Store(
            stable(a),
            SIROffset::Static(0),
            8,
            val,
            Vec::new(),
            Vec::new(),
        ));
        builder.emit(SIRInstruction::Load(
            val,
            stable(b),
            SIROffset::Static(0),
            8,
        ));
        builder.emit(SIRInstruction::Store(
            stable(y),
            SIROffset::Static(0),
            8,
            val,
            Vec::new(),
            Vec::new(),
        ));
        let then_block = builder.new_block();
        let join = builder.new_block();
        builder.emit(SIRInstruction::Imm(sel, SIRValue::new(1u8)));
        builder.seal_block(SIRTerminator::Branch {
            cond: sel,
            true_block: (then_block, Vec::new()),
            false_block: (join, Vec::new()),
        });
        builder.switch_to_block(then_block);
        builder.emit(SIRInstruction::Store(
            stable(b),
            SIROffset::Static(0),
            8,
            val,
            Vec::new(),
            Vec::new(),
        ));
        builder.seal_block(SIRTerminator::Jump(join, Vec::new()));
        builder.switch_to_block(join);
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let unit = ExecutionUnit {
            blocks,
            register_map,
            entry_block_id: BlockId(0),
        };
        let targets = [
            VarAtomBase::new(a, 0, 7),
            VarAtomBase::new(b, 0, 7),
            VarAtomBase::new(y, 0, 7),
        ];
        let widths = [(a, 8), (b, 8), (y, 8)].into_iter().collect();
        let aliased = [(b, a)].into_iter().collect();
        assert!(!retains_state_with_aliases(
            &unit, &targets, &widths, &aliased
        ));
        assert!(retains_state(&unit, &targets, &widths));
    }
}
