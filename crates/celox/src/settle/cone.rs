//! The clock cone of a partitioned settle: the partitions a kernel's write
//! of an event signal (or the slot time) can leave dirty, and a copy of the
//! settle that runs only them.
//!
//! A settle leaves every partition flag clear (publishing goes forward in
//! the partition order), so at the slot start after a kernel that wrote
//! only clock nets the dirty partitions are the readers of those nets and,
//! transitively, the partitions they publish to. The cone is the settle
//! with the guards of every other partition turned into jumps past their
//! bodies and the group tests of groups without cone partitions turned into
//! jumps too; the compiled scheduler runs it when its dirty byte says that
//! only clocks changed ([`crate::scheduler`] `Incremental`). Pinned
//! partitions have no guard and stay.
use super::prune::Analysis;
use crate::AbsoluteAddr;
use crate::ir::{RegionedAbsoluteAddr, SIRInstruction};
#[cfg(feature = "host-runtime")]
use celox_design::BinaryOp;
#[cfg(feature = "host-runtime")]
use celox_sir::SIRTerminator;
use celox_sir::{BlockId, ExecutionUnit, SIROffset};

type Unit = ExecutionUnit<RegionedAbsoluteAddr>;

#[cfg(feature = "host-runtime")]
/// The settle `units` restricted to the closure of `seeds` under
/// publishing, and the number of partitions it runs. `None` when a
/// partition's marks cannot be decoded.
pub(crate) fn clock_cone(
    units: &[Unit],
    flags: &RegionedAbsoluteAddr,
    partitions: usize,
    offsets: &crate::HashMap<AbsoluteAddr, usize>,
    seeds: impl IntoIterator<Item = usize>,
) -> Option<(Vec<Unit>, usize)> {
    let marks = publish_marks(units, flags, partitions, offsets)?;
    let cone = closure(&marks, seeds);
    let mut restricted = Vec::with_capacity(units.len());
    let untracked = celox_design::settle_untracked_flag(partitions);
    let mut tracked = true;
    for unit in units {
        tracked &= !unit
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .any(|inst| inst.state_writes().overlaps(flags, untracked * 8, 8));
        let analysis = Analysis::new(unit, flags, partitions, offsets);
        let mut copy = unit.clone();
        for (&id, block) in &unit.blocks {
            let SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } = &block.terminator
            else {
                continue;
            };
            let target = if let Some((partition, _, _)) = analysis.guard(id) {
                (!cone[partition]).then_some(false_block)
            } else if let Some((first, count)) = flag_test(&analysis, *cond) {
                // The caller has already tested this byte before entering the
                // clock-only schedule. Drop the full-settle initialization
                // (one store per flag word) from the specialized entry.
                if count == 1 {
                    (tracked && first == untracked).then_some(false_block)
                } else {
                    let live = cone[first.min(partitions)..(first + count).min(partitions)]
                        .iter()
                        .filter(|&&live| live)
                        .take(2)
                        .count();
                    match live {
                        0 => Some(false_block),
                        // The remaining partition tests its own flag. Entering
                        // the group directly drops a redundant flag-word load.
                        1 => Some(true_block),
                        _ => None,
                    }
                }
            } else {
                None
            };
            if let Some((target, args)) = target {
                copy.blocks.get_mut(&id).expect("a copied block").terminator =
                    SIRTerminator::Jump(*target, args.clone());
            }
        }
        remove_unreachable_blocks(&mut copy);
        celox_sir_opt::optimizer::remove_dead_sir_definitions(&mut copy);
        restricted.push(copy);
    }
    Some((restricted, cone.iter().filter(|&&in_cone| in_cone).count()))
}

/// The partitions each partition's body marks, from every unit; `None`
/// when a partition's marks cannot be decoded.
pub(crate) fn publish_marks(
    units: &[Unit],
    flags: &RegionedAbsoluteAddr,
    partitions: usize,
    offsets: &crate::HashMap<AbsoluteAddr, usize>,
) -> Option<Vec<Vec<usize>>> {
    let mut marks: Vec<Vec<usize>> = vec![Vec::new(); partitions];
    for unit in units {
        let analysis = Analysis::new(unit, flags, partitions, offsets);
        for &block in unit.blocks.keys() {
            let Some((partition, body, merge)) = analysis.guard(block) else {
                continue;
            };
            for block in analysis.body(body, merge) {
                marks[partition].extend(marked_by(&analysis, block)?);
            }
        }
    }
    Some(marks)
}

/// The partitions `seeds` reach under publishing (`marks`), by partition.
pub(crate) fn closure(marks: &[Vec<usize>], seeds: impl IntoIterator<Item = usize>) -> Vec<bool> {
    let partitions = marks.len();
    let mut cone = vec![false; partitions];
    let mut work: Vec<usize> = seeds
        .into_iter()
        .filter(|&seed| seed < partitions)
        .collect();
    while let Some(partition) = work.pop() {
        if std::mem::replace(&mut cone[partition], true) {
            continue;
        }
        work.extend(marks[partition].iter().copied());
    }
    cone
}

/// The partitions whose flags `block` stores; `None` for a flag store that
/// cannot be decoded.
fn marked_by(analysis: &Analysis<'_>, block: BlockId) -> Option<Vec<usize>> {
    let mut marked = Vec::new();
    for instruction in &analysis.unit.blocks.get(&block)?.instructions {
        let SIRInstruction::Store(address, offset, width, source, ..) = instruction else {
            continue;
        };
        if address != analysis.flags {
            continue;
        }
        let SIROffset::Static(offset) = offset else {
            return None;
        };
        // The same object also holds wait flags and optional profiling
        // counters. Their stores cannot activate a settle partition.
        if offset / 8 > celox_design::settle_untracked_flag(analysis.partitions) {
            continue;
        }
        if !offset.is_multiple_of(8) || !width.is_multiple_of(8) {
            return None;
        }
        let bytes = if *width == 8 {
            if analysis.is_zero(*source) {
                continue;
            }
            vec![analysis.is_one(*source).then_some(true)?]
        } else {
            analysis.published_bytes(*source, *width)?
        };
        marked.extend(
            bytes
                .iter()
                .enumerate()
                .filter(|(_, set)| **set)
                .map(|(byte, _)| offset / 8 + byte)
                .filter(|&partition| partition < analysis.partitions),
        );
    }
    Some(marked)
}

#[cfg(feature = "host-runtime")]
/// The first byte and byte count covered by a nonzero test of the flags.
fn flag_test(analysis: &Analysis<'_>, cond: celox_sir::RegisterId) -> Option<(usize, usize)> {
    let SIRInstruction::Binary(_, lhs, BinaryOp::Ne, rhs) = analysis.defs.get(&cond)? else {
        return None;
    };
    let word = |register| match analysis.defs.get(&register)? {
        SIRInstruction::Load(_, address, SIROffset::Static(offset), width)
            if address == analysis.flags
                && offset.is_multiple_of(8)
                && width.is_multiple_of(8)
                && *width >= 8 =>
        {
            Some((offset / 8, width / 8))
        }
        _ => None,
    };
    match (word(*lhs), word(*rhs)) {
        (Some(group), None) if analysis.is_zero(*rhs) => Some(group),
        (None, Some(group)) if analysis.is_zero(*lhs) => Some(group),
        _ => None,
    }
}

#[cfg(feature = "host-runtime")]
fn remove_unreachable_blocks(unit: &mut Unit) {
    let mut reachable = crate::HashSet::from_iter([unit.entry_block_id]);
    let mut stack = vec![unit.entry_block_id];
    while let Some(block) = stack.pop() {
        let Some(block) = unit.blocks.get(&block) else {
            continue;
        };
        let mut next = |target: BlockId| {
            if reachable.insert(target) {
                stack.push(target);
            }
        };
        match &block.terminator {
            SIRTerminator::Jump(target, _) => next(*target),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                next(true_block.0);
                next(false_block.0);
            }
            SIRTerminator::Switch { cases, default, .. } => {
                for case in cases {
                    next(case.target);
                }
                next(*default);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    unit.blocks.retain(|id, _| reachable.contains(id));
}

#[cfg(all(test, feature = "host-runtime"))]
mod tests {
    use super::*;
    use celox_sir::{SIRBuilder, SIRValue};

    #[test]
    fn counter_updates_do_not_hide_partition_publications() {
        let flags = RegionedAbsoluteAddr::from_absolute_addr(
            celox_design::STABLE_REGION,
            AbsoluteAddr {
                instance_id: celox_design::InstanceId(0),
                var_id: celox_design::StateObjectId(1),
            },
        );
        let marks = |byte| {
            let mut b = SIRBuilder::new();
            let counter = b.alloc_bit(64, false);
            let one = b.alloc_bit(8, false);
            let offset = SIROffset::Static(byte * 8);
            b.emit(SIRInstruction::Load(counter, flags, offset.clone(), 64));
            b.emit(SIRInstruction::Store(
                flags,
                offset,
                64,
                counter,
                vec![],
                vec![],
            ));
            b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
            b.emit(SIRInstruction::Store(
                flags,
                SIROffset::Static(8),
                8,
                one,
                vec![],
                vec![],
            ));
            let unit = b.flush_eu().unwrap();
            let offsets = Default::default();
            marked_by(
                &Analysis::new(&unit, &flags, 2, &offsets),
                unit.entry_block_id,
            )
        };
        assert_eq!(
            marks(celox_design::settle_profile_offset(2, 0)),
            Some(vec![1])
        );
        // A counter-sized store overlapping a flag or the untracked byte
        // still needs a known publication mask.
        assert_eq!(marks(1), None);
        assert_eq!(marks(2), None);
    }

    #[test]
    fn tracked_entry_skips_the_full_flag_fill_only_in_the_specialized_copy() {
        let flags = RegionedAbsoluteAddr::from_absolute_addr(
            celox_design::STABLE_REGION,
            AbsoluteAddr {
                instance_id: celox_design::InstanceId(0),
                var_id: celox_design::StateObjectId(1),
            },
        );
        let mut b = SIRBuilder::new();
        let raw = b.alloc_bit(8, false);
        let zero = b.alloc_bit(8, false);
        let test = b.alloc_bit(1, false);
        let fill = b.new_block();
        let start = b.new_block();
        b.emit(SIRInstruction::Load(
            raw,
            flags,
            SIROffset::Static(celox_design::settle_untracked_flag(8) * 8),
            8,
        ));
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u32)));
        b.emit(SIRInstruction::Binary(test, raw, BinaryOp::Ne, zero));
        b.seal_block(SIRTerminator::Branch {
            cond: test,
            true_block: (fill, vec![]),
            false_block: (start, vec![]),
        });
        b.switch_to_block(fill);
        b.emit(SIRInstruction::Store(
            flags,
            SIROffset::Static(0),
            8,
            raw,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Jump(start, vec![]));
        b.switch_to_block(start);
        let unit = b.flush_eu().unwrap();
        let (tracked, _) = clock_cone(
            std::slice::from_ref(&unit),
            &flags,
            8,
            &Default::default(),
            [0],
        )
        .unwrap();
        assert!(!tracked[0].blocks.contains_key(&fill));
        assert!(unit.blocks.contains_key(&fill));

        // A preceding unit can invalidate the entry assumption.
        let mut writer = SIRBuilder::new();
        let one = writer.alloc_bit(8, false);
        writer.emit(SIRInstruction::Imm(one, SIRValue::new(1u32)));
        writer.emit(SIRInstruction::Store(
            flags,
            SIROffset::Static(celox_design::settle_untracked_flag(8) * 8),
            8,
            one,
            vec![],
            vec![],
        ));
        let (restricted, _) = clock_cone(
            &[writer.flush_eu().unwrap(), unit],
            &flags,
            8,
            &Default::default(),
            [0],
        )
        .unwrap();
        assert!(restricted[1].blocks.contains_key(&fill));
    }
}
