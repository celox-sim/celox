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
    for unit in units {
        let analysis = Analysis::new(unit, flags, partitions, offsets);
        let mut copy = unit.clone();
        for (&id, block) in &unit.blocks {
            let SIRTerminator::Branch {
                cond,
                true_block: _,
                false_block: (skip, skip_args),
            } = &block.terminator
            else {
                continue;
            };
            let skipped = if let Some((partition, _, _)) = analysis.guard(id) {
                !cone[partition]
            } else if let Some((first, count)) = group_test(&analysis, *cond) {
                (first..(first + count).min(partitions)).all(|partition| !cone[partition])
            } else {
                false
            };
            if skipped {
                copy.blocks.get_mut(&id).expect("a copied block").terminator =
                    SIRTerminator::Jump(*skip, skip_args.clone());
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
/// The first partition and the partition count of the group a test of
/// `cond` covers (`Load flag word; Ne 0`).
fn group_test(analysis: &Analysis<'_>, cond: celox_sir::RegisterId) -> Option<(usize, usize)> {
    let SIRInstruction::Binary(_, lhs, BinaryOp::Ne, rhs) = analysis.defs.get(&cond)? else {
        return None;
    };
    let word = |register| match analysis.defs.get(&register)? {
        SIRInstruction::Load(_, address, SIROffset::Static(offset), width)
            if address == analysis.flags
                && offset.is_multiple_of(8)
                && width.is_multiple_of(8)
                && *width > 8 =>
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
