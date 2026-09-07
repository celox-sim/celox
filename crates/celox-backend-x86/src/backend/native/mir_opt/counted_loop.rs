//! Reuse a loop's increasing index for a redundant decreasing trip counter.

use super::*;

pub(super) fn step(
    value: VReg,
    source: VReg,
    increasing: bool,
    definitions: &[Option<&MInst>],
) -> Option<(u64, Vec<VReg>)> {
    let mut value = value;
    let mut mask = u64::MAX;
    let mut chain = Vec::new();
    for _ in 0..8 {
        chain.push(value);
        let inst = *definitions.get(value.0 as usize)?.as_ref()?;
        let constant_one = |value: VReg| {
            matches!(
                definitions[value.0 as usize],
                Some(MInst::LoadImm { value: 1, .. })
            )
        };
        match *inst {
            MInst::Mov { src, .. } => value = src,
            MInst::Mov32 { src, .. } => {
                mask &= u32::MAX as u64;
                value = src;
            }
            MInst::AndImm { src, imm, .. } => {
                mask &= imm;
                value = src;
            }
            MInst::AndImm32 { src, imm, .. } => {
                mask &= u64::from(imm);
                value = src;
            }
            MInst::AddImm { src, imm: 1, .. } if increasing && src == source => break,
            MInst::SubImm { src, imm: 1, .. } if !increasing && src == source => break,
            MInst::Add { lhs, rhs, .. } | MInst::Add32 { lhs, rhs, .. }
                if increasing
                    && (lhs == source && constant_one(rhs)
                        || rhs == source && constant_one(lhs)) =>
            {
                if matches!(inst, MInst::Add32 { .. }) {
                    mask &= u32::MAX as u64;
                }
                break;
            }
            MInst::Sub { lhs, rhs, .. } | MInst::Sub32 { lhs, rhs, .. }
                if !increasing && lhs == source && constant_one(rhs) =>
            {
                if matches!(inst, MInst::Sub32 { .. }) {
                    mask &= u32::MAX as u64;
                }
                break;
            }
            _ => return None,
        }
    }
    // A truncation must preserve a contiguous low part. Confirm that the walk
    // ended on the arithmetic operation, rather than exhausting its budget.
    if value != *chain.last()? || mask == 0 || mask & mask.wrapping_add(1) != 0 {
        return None;
    }
    Some((mask, chain))
}

/// A zero-based, unit-step phi cannot reach its limit on a backedge guarded by
/// `next != limit`. Seed that inductive invariant before the bit-fact solve.
pub(super) fn index_zeros(func: &MFunction) -> Vec<(VReg, u64)> {
    let mut definitions = vec![None; func.vregs.count() as usize];
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
            }
        }
    }
    let mut facts = Vec::new();
    for header in &func.blocks {
        for phi in &header.phis {
            if phi.sources.len() != 2 {
                continue;
            }
            let Some(&(entry, _)) = phi.sources.iter().find(|(_, source)| {
                matches!(
                    definitions[source.0 as usize],
                    Some(MInst::LoadImm { value: 0, .. })
                )
            }) else {
                continue;
            };
            let Some(&(latch, next)) = phi.sources.iter().find(|(pred, _)| *pred != entry) else {
                continue;
            };
            let Some((mask, _)) = step(next, phi.dst, true, &definitions) else {
                continue;
            };
            let Some(&MInst::Branch {
                cond,
                true_bb,
                false_bb,
            }) = func.blocks[positions[&latch]].terminator()
            else {
                continue;
            };
            let Some(MInst::CmpImm { lhs, imm, kind, .. }) = definitions[cond.0 as usize] else {
                continue;
            };
            if *lhs != next
                || *imm <= 0
                || *imm as u64 > mask
                || !matches!(
                    (*kind, true_bb == header.id, false_bb == header.id),
                    (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
                )
            {
                continue;
            }
            let maximum = (*imm - 1) as u64;
            facts.push((
                phi.dst,
                u64::MAX
                    .checked_shl(64 - maximum.leading_zeros())
                    .unwrap_or(0),
            ));
        }
    }
    facts
}

/// A load at a loop header is executed on entry. Move it to a unique jumping
/// predecessor when no loop write can change any of its physical bytes.
pub(super) fn hoist_invariant_loads(func: &mut MFunction) {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions[&target])
                .collect()
        })
        .collect();
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return;
    };
    for region in &cfg.loops {
        let mut entries = cfg.predecessors[region.header]
            .iter()
            .copied()
            .filter(|pred| !region.blocks.contains(pred));
        let Some(entry) = entries.next() else {
            continue;
        };
        if entries.next().is_some()
            || !matches!(func.blocks[entry].terminator(), Some(MInst::Jump { target }) if *target == func.blocks[region.header].id)
        {
            continue;
        }
        let writes = region
            .blocks
            .iter()
            .flat_map(|&block| func.blocks[block].insts.iter().map(memory_effect::writes))
            .filter(|effect| effect.has_effect())
            .collect::<Vec<_>>();
        let mut moved = Vec::new();
        func.blocks[region.header].insts.retain(|inst| {
            let MInst::Load {
                base, offset, size, ..
            } = *inst
            else {
                return true;
            };
            // State memory is valid for the whole native invocation. Stack
            // lifetime and runtime-owned pointer accesses stay at their sites.
            if base != BaseReg::SimState {
                return true;
            }
            let start = i64::from(offset);
            let end = start + i64::from(size.bytes());
            if writes.iter().any(|effect| {
                effect.unknown_memory() == Some(memory_effect::UnknownMemory::Direct(base))
                    || effect.ranges().any(|range| {
                        range.base == base
                            && range.offset < end
                            && range.end().is_none_or(|limit| start < limit)
                    })
            }) {
                return true;
            }
            moved.push(inst.clone());
            false
        });
        let at = func.blocks[entry].insts.len() - 1;
        func.blocks[entry].insts.splice(at..at, moved);
    }
}

pub(super) fn run(func: &mut MFunction) {
    let positions = func
        .blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.id, index))
        .collect::<HashMap<_, _>>();
    let successors = func
        .blocks
        .iter()
        .map(|block| {
            block
                .successors()
                .into_iter()
                .map(|target| positions[&target])
                .collect()
        })
        .collect();
    let Ok(cfg) = celox_analysis::cfg::ForwardControlFlowGraph::analyze_structure(successors, 0)
    else {
        return;
    };
    let mut definitions = vec![None; func.vregs.count() as usize];
    let mut uses = vec![0usize; definitions.len()];
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                uses[source.0 as usize] += 1;
            }
        }
        for inst in &block.insts {
            for source in inst.uses() {
                uses[source.0 as usize] += 1;
            }
            if let Some(dst) = inst.def() {
                definitions[dst.0 as usize] = Some(inst);
            }
        }
    }
    let constant = |value: VReg| match definitions[value.0 as usize] {
        Some(MInst::LoadImm { value, .. }) => Some(*value),
        _ => None,
    };
    let mut plans = Vec::new();
    for region in &cfg.loops {
        let header = &func.blocks[region.header];
        for counter in &header.phis {
            if counter.sources.len() != 2 {
                continue;
            }
            let Some(&(latch, next)) = counter
                .sources
                .iter()
                .find(|(pred, _)| region.blocks.contains(&positions[pred]))
            else {
                continue;
            };
            let Some(&(entry, initial)) = counter
                .sources
                .iter()
                .find(|(pred, _)| !region.blocks.contains(&positions[pred]))
            else {
                continue;
            };
            let Some(trips) =
                constant(initial).filter(|trips| (1..=i32::MAX as u64).contains(trips))
            else {
                continue;
            };
            let Some((counter_mask, mut chain)) = step(next, counter.dst, false, &definitions)
            else {
                continue;
            };
            if trips > counter_mask {
                continue;
            }
            let latch_index = positions[&latch];
            let Some(&MInst::Branch {
                cond,
                true_bb,
                false_bb,
            }) = func.blocks[latch_index].terminator()
            else {
                continue;
            };
            if true_bb == false_bb || uses[cond.0 as usize] != 1 {
                continue;
            }
            let kind = match definitions[cond.0 as usize] {
                Some(MInst::CmpImm {
                    lhs, imm: 0, kind, ..
                }) if *lhs == next => *kind,
                Some(MInst::Cmp { lhs, rhs, kind, .. })
                    if *lhs == next && constant(*rhs) == Some(0)
                        || *rhs == next && constant(*lhs) == Some(0) =>
                {
                    *kind
                }
                _ => continue,
            };
            if !matches!(
                (kind, true_bb == header.id, false_bb == header.id),
                (CmpKind::Ne, true, false) | (CmpKind::Eq, false, true)
            ) {
                continue;
            }
            for index in &header.phis {
                if index.sources.len() != 2
                    || !index
                        .sources
                        .iter()
                        .any(|&(pred, source)| pred == entry && constant(source) == Some(0))
                {
                    continue;
                }
                let Some(&(_, next_index)) = index.sources.iter().find(|&&(pred, _)| pred == latch)
                else {
                    continue;
                };
                let Some((index_mask, _)) = step(next_index, index.dst, true, &definitions) else {
                    continue;
                };
                if trips > index_mask {
                    continue;
                }
                chain.push(counter.dst);
                plans.push((latch_index, cond, next_index, trips as i32, kind, chain));
                break;
            }
        }
    }
    for (latch, condition, next_index, trips, kind, chain) in plans {
        for block in &mut func.blocks {
            block.insts.retain(|inst| inst.def() != Some(condition));
        }
        let before_branch = func.blocks[latch].insts.len() - 1;
        func.blocks[latch].insts.insert(
            before_branch,
            MInst::CmpImm {
                dst: condition,
                lhs: next_index,
                imm: trips,
                kind,
            },
        );
        // The old induction variable is a dead SSA cycle. Ordinary local DCE
        // cannot collect it, so remove it only if its entire use set is private.
        let private = func.blocks.iter().all(|block| {
            block.phis.iter().all(|phi| {
                chain.contains(&phi.dst)
                    || phi
                        .sources
                        .iter()
                        .all(|(_, source)| !chain.contains(source))
            }) && block.insts.iter().all(|inst| {
                inst.def().is_some_and(|dst| chain.contains(&dst))
                    || inst
                        .uses()
                        .into_iter()
                        .all(|source| !chain.contains(&source))
            })
        });
        if private {
            for block in &mut func.blocks {
                block.phis.retain(|phi| !chain.contains(&phi.dst));
                block
                    .insts
                    .retain(|inst| inst.def().is_none_or(|dst| !chain.contains(&dst)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{emit, jit_mem::JitCode, mir_legalize, regalloc};

    fn fixture(
        trips: u64,
        mask: u64,
        index_mask: u64,
        reversed: bool,
        observed: bool,
    ) -> MFunction {
        let mut vregs = VRegAllocator::new();
        for _ in 0..14 {
            vregs.alloc();
        }
        let mut function = MFunction::new(vregs, vec![SpillDesc::transient(); 14]);
        let mut entry = MBlock::new(BlockId(0));
        for (dst, value) in [(VReg(0), 0), (VReg(1), trips), (VReg(2), 1)] {
            entry.push(MInst::LoadImm { dst, value });
        }
        entry.push(MInst::Jump { target: BlockId(1) });
        let mut header = MBlock::new(BlockId(1));
        for (dst, first, next) in [
            (VReg(3), VReg(1), VReg(7)),
            (VReg(4), VReg(0), VReg(9)),
            (VReg(5), VReg(0), VReg(10)),
        ] {
            header.phis.push(PhiNode {
                dst,
                sources: vec![(BlockId(0), first), (BlockId(2), next)],
            });
        }
        header.push(MInst::Add {
            dst: VReg(10),
            lhs: VReg(5),
            rhs: VReg(4),
        });
        if observed {
            header.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(3),
                size: OpSize::S64,
            });
        }
        header.push(MInst::Jump { target: BlockId(2) });
        let mut latch = MBlock::new(BlockId(2));
        latch.push(MInst::Sub32 {
            dst: VReg(6),
            lhs: VReg(3),
            rhs: VReg(2),
        });
        latch.push(MInst::AndImm {
            dst: VReg(7),
            src: VReg(6),
            imm: mask,
        });
        latch.push(MInst::Add32 {
            dst: VReg(8),
            lhs: VReg(4),
            rhs: VReg(2),
        });
        latch.push(MInst::AndImm {
            dst: VReg(9),
            src: VReg(8),
            imm: index_mask,
        });
        latch.push(MInst::CmpImm {
            dst: VReg(11),
            lhs: VReg(7),
            imm: 0,
            kind: if reversed { CmpKind::Eq } else { CmpKind::Ne },
        });
        latch.push(MInst::Branch {
            cond: VReg(11),
            true_bb: BlockId(if reversed { 3 } else { 1 }),
            false_bb: BlockId(if reversed { 1 } else { 3 }),
        });
        let mut exit = MBlock::new(BlockId(3));
        for (offset, src) in [(0, VReg(10)), (8, VReg(9))] {
            exit.push(MInst::Store {
                base: BaseReg::SimState,
                offset,
                src,
                size: OpSize::S64,
            });
        }
        exit.push(MInst::Return);
        function.blocks = vec![entry, header, latch, exit];
        function
    }

    #[test]
    fn reuses_index_without_changing_trip_counts_or_observed_counters() {
        for trips in [1u64, 2, 7, 8, 31, 32, 63] {
            for reversed in [false, true] {
                for observed in [false, true] {
                    let mut function = fixture(trips, 63, u32::MAX as u64, reversed, observed);
                    run(&mut function);
                    function.verify();
                    assert_eq!(
                        function.blocks[1].phis.iter().any(|phi| phi.dst == VReg(3)),
                        observed
                    );
                    assert!(function.blocks[2].insts.iter().any(|inst| matches!(inst, MInst::CmpImm { lhs: VReg(9), imm, .. } if *imm == trips as i32)));
                    mir_legalize::legalize(&mut function);
                    let allocation = regalloc::run_regalloc(&mut function).unwrap();
                    let emitted = emit::emit(
                        &function,
                        &allocation.assignment,
                        allocation.spill_frame_size,
                    )
                    .unwrap();
                    let jit = JitCode::new(&emitted.code).unwrap();
                    let mut state = vec![0u8; emitted.required_state_size.max(24) as usize];
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    assert_eq!(&state[..8], &(trips * (trips - 1) / 2).to_le_bytes());
                    assert_eq!(&state[8..16], &trips.to_le_bytes());
                    assert_eq!(&state[16..24], &u64::from(observed).to_le_bytes());
                }
            }
        }
    }

    #[test]
    fn rejects_wrapping_and_noncontiguous_counters() {
        for (trips, mask, index_mask) in [
            (0, 63, 63),
            (64, 63, 255),
            (32, 63, 31),
            (8, 61, 63),
            (8, 63, 61),
        ] {
            let mut function = fixture(trips, mask, index_mask, false, false);
            let before = format!("{function:?}");
            run(&mut function);
            function.verify();
            assert_eq!(format!("{function:?}"), before);
        }
    }

    #[test]
    fn places_the_exit_test_after_the_index_update_and_keeps_shared_tests() {
        let mut function = fixture(32, 63, 255, false, false);
        let condition = function.blocks[2].insts.remove(4);
        function.blocks[2].insts.insert(2, condition);
        function.verify();
        let mut shared = function.clone();
        shared.blocks[3].insts.insert(
            0,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 24,
                src: VReg(11),
                size: OpSize::S64,
            },
        );
        let unchanged = format!("{shared:?}");
        run(&mut shared);
        shared.verify();
        assert_eq!(format!("{shared:?}"), unchanged);
        run(&mut function);
        function.verify();
        assert!(matches!(
            function.blocks[2].insts[function.blocks[2].insts.len() - 2],
            MInst::CmpImm {
                lhs: VReg(9),
                imm: 32,
                ..
            }
        ));
    }

    #[test]
    fn packed_loop_loads_respect_bounds_consumers_and_loop_writes() {
        for trips in [9u64, 16, 17, 32, 33, 64] {
            for effect in 0..8 {
                let mut function = fixture(trips, 127, 255, false, false);
                let mut allocate = || {
                    let value = function.vregs.alloc();
                    function.spill_descs.push(SpillDesc::transient());
                    value
                };
                let byte = allocate();
                let bit = allocate();
                let raw = allocate();
                let shifted = allocate();
                let selected = allocate();
                let size = if trips <= 16 {
                    2
                } else if trips <= 32 {
                    4
                } else {
                    8
                };
                let envelope_size = if effect == 7 {
                    trips.div_ceil(8) as usize
                } else {
                    size
                };
                let header = &mut function.blocks[1];
                header.insts[0] = MInst::Add {
                    dst: VReg(10),
                    lhs: VReg(5),
                    rhs: selected,
                };
                header.insts.splice(
                    0..0,
                    [
                        MInst::ShrImm {
                            dst: byte,
                            src: VReg(4),
                            imm: 3,
                        },
                        MInst::AndImm32 {
                            dst: bit,
                            src: VReg(4),
                            imm: 7,
                        },
                        MInst::LoadIndexed {
                            dst: raw,
                            base: BaseReg::SimState,
                            offset: 64,
                            index: byte,
                            scale: 1,
                            size: OpSize::S8,
                            alias_range: MemoryAliasRange::new(64, envelope_size),
                        },
                        MInst::Shr {
                            dst: shifted,
                            lhs: raw,
                            rhs: bit,
                        },
                        MInst::AndImm32 {
                            dst: selected,
                            src: shifted,
                            imm: if effect == 6 { 255 } else { 1 },
                        },
                    ],
                );
                if effect == 5 {
                    let at = header.insts.len() - 1;
                    header.insts.insert(
                        at,
                        MInst::Store {
                            base: BaseReg::SimState,
                            offset: 24,
                            src: raw,
                            size: OpSize::S64,
                        },
                    );
                }
                let write = match effect {
                    1 | 2 => Some(MInst::MemFill {
                        dst_offset: if effect == 1 { 64 } else { 96 },
                        byte_len: 1,
                        value: 0,
                    }),
                    3 | 4 => Some(MInst::StoreIndexed {
                        base: BaseReg::SimState,
                        offset: if effect == 3 { 64 } else { 96 },
                        index: VReg(0),
                        src: VReg(0),
                        size: OpSize::S8,
                        alias_range: if effect == 3 {
                            None
                        } else {
                            MemoryAliasRange::new(96, 1)
                        },
                    }),
                    _ => None,
                };
                if let Some(write) = write {
                    function.blocks[2].insts.insert(0, write);
                }
                run(&mut function);
                known_bits::fold_packed_bit_loads(&mut function);
                hoist_invariant_loads(&mut function);
                function.verify();
                let widened = !matches!(effect, 5 | 6) && envelope_size == size;
                let hoisted = widened && !matches!(effect, 1 | 3);
                assert_eq!(
                    function.blocks[0]
                        .insts
                        .iter()
                        .any(|inst| matches!(inst, MInst::Load { offset: 64, .. })),
                    hoisted,
                    "trips={trips} effect={effect}"
                );
                mir_legalize::legalize(&mut function);
                let allocation = regalloc::run_regalloc(&mut function).unwrap();
                let emitted = emit::emit(
                    &function,
                    &allocation.assignment,
                    allocation.spill_frame_size,
                )
                .unwrap();
                let jit = JitCode::new(&emitted.code).unwrap();
                for input in [0u64, 1, 0x0123_4567_89ab_cdef, 1 << 63, u64::MAX] {
                    let mut state = vec![0u8; emitted.required_state_size.max(104) as usize];
                    state[64..72].copy_from_slice(&input.to_le_bytes());
                    let mut bytes = input.to_le_bytes();
                    let mut sum = 0u64;
                    for index in 0..trips as usize {
                        sum += u64::from(bytes[index / 8] >> (index % 8))
                            & if effect == 6 { 255 } else { 1 };
                        if matches!(effect, 1 | 3) {
                            bytes[0] = 0;
                        }
                    }
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    assert_eq!(
                        &state[..8],
                        &sum.to_le_bytes(),
                        "trips={trips} effect={effect} input={input:#x}"
                    );
                    assert_eq!(&state[64..72], &bytes);
                }
            }
        }
    }
}
