use super::super::cfg;
use super::*;
use crate::native::mir::{MBlock, MFunction, MInst, SpillDesc, VRegAllocator};

#[test]
fn baseline_slots_keep_sparse_homes_disjoint_within_the_budget() {
    let homes = [SpillHome(1), SpillHome(27), SpillHome(u32::MAX)]
        .into_iter()
        .collect();
    assert!(unique_stack_slots(&homes, 23).is_none());
    let coloring = unique_stack_slots(&homes, 24).unwrap();
    assert_eq!(coloring.frame_size, 24);
    assert_eq!(coloring.slot_count, 3);
    let mut offsets = coloring.offsets.values().copied().collect::<Vec<_>>();
    offsets.sort_unstable();
    assert_eq!(offsets, [0, 8, 16]);
    assert_eq!(coloring.offsets[&SpillHome(u32::MAX)], 16);
    let empty = unique_stack_slots(&BTreeSet::new(), 0).unwrap();
    assert_eq!(empty.frame_size, 0);
    assert!(empty.offsets.is_empty());
}

fn function(value_count: u32, instructions: Vec<MInst>) -> MFunction {
    let mut values = VRegAllocator::new();
    for _ in 0..value_count {
        values.alloc();
    }
    let mut function = MFunction::new(values, vec![SpillDesc::transient(); value_count as usize]);
    let mut block = MBlock::new(BlockId(0));
    block.insts = instructions;
    function.blocks.push(block);
    function
}

fn color_planned_events(
    func: &MFunction,
    cfg: &NormalizedCfg,
    rows: &[(BlockId, Vec<(PlannedStackEventKind, SpillHome)>)],
) -> Result<PlannedStackColoring, StackColorError> {
    let mut events = vec![Vec::new(); func.blocks.len()];
    let mut homes = BTreeSet::new();
    let mut sequence = 0usize;
    for (block, row) in rows {
        let block = cfg.block_index[block];
        for (instruction, &(kind, home)) in row.iter().enumerate() {
            if matches!(kind, PlannedStackEventKind::Store) {
                homes.insert(home);
            }
            events[block].push(PlannedStackEvent {
                instruction,
                sequence,
                home,
                kind,
                definition: None,
                reaching: None,
            });
            sequence += 1;
        }
    }
    let (program, homes) = build_planned_stack_program_from_events(func, cfg, events, homes)?;
    color_planned_stack_program(program, homes, cfg, false, true)
}

fn diamond_function() -> MFunction {
    let mut values = VRegAllocator::new();
    let condition = values.alloc();
    let mut function = MFunction::new(values, vec![SpillDesc::transient()]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut left = MBlock::new(BlockId(1));
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Return);
    function.blocks = vec![entry, left, right, join];
    function
}

fn reference_phi_relations(
    events: &[Vec<PlannedStackEvent>],
    cfg: &NormalizedCfg,
) -> Vec<(SpillHome, usize)> {
    let mut definitions = HashSet::<(SpillHome, usize)>::default();
    let mut upward_uses = HashSet::<(SpillHome, usize)>::default();
    for (block, block_events) in events.iter().enumerate() {
        let mut locally_defined = HashSet::<SpillHome>::default();
        for event in block_events {
            match event.kind {
                PlannedStackEventKind::Store => {
                    locally_defined.insert(event.home);
                    definitions.insert((event.home, block));
                }
                PlannedStackEventKind::Reload => {
                    if !locally_defined.contains(&event.home) {
                        upward_uses.insert((event.home, block));
                    }
                }
            }
        }
    }
    let mut live_in = upward_uses.clone();
    let mut live_work = upward_uses.iter().copied().collect::<VecDeque<_>>();
    while let Some((home, block)) = live_work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            let relation = (home, predecessor);
            if !definitions.contains(&relation) && live_in.insert(relation) {
                live_work.push_back(relation);
            }
        }
    }

    let mut phi_relations = HashSet::<(SpillHome, usize)>::default();
    let mut queued = definitions.clone();
    let mut phi_work = definitions.iter().copied().collect::<Vec<_>>();
    while let Some((home, block)) = phi_work.pop() {
        for &frontier in &cfg.dominance_frontier[block] {
            let relation = (home, frontier);
            if frontier == 0 || !live_in.contains(&relation) || !phi_relations.insert(relation) {
                continue;
            }
            if queued.insert(relation) {
                phi_work.push(relation);
            }
        }
    }

    let mut result = phi_relations.into_iter().collect::<Vec<_>>();
    result.sort_unstable_by_key(|(home, block)| (*block, *home));
    result
}

#[test]
fn per_home_phis_match_global_dataflow() {
    use PlannedStackEventKind::{Reload as R, Store as S};
    let patterns: &[&[PlannedStackEventKind]] = &[&[], &[S], &[R], &[S, R], &[R, S], &[R, S, R]];
    for loop_back in [false, true] {
        let mut func = diamond_function();
        if loop_back {
            func.blocks[0].insts.pop();
            func.blocks[0].push(MInst::Jump { target: BlockId(1) });
            func.blocks[1].insts = vec![MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(2),
                false_bb: BlockId(3),
            }];
            func.blocks[2].insts = vec![MInst::Jump { target: BlockId(1) }];
        }
        let cfg = cfg::normalize(&mut func).unwrap();
        for mut pattern in 0usize..patterns.len().pow(4) {
            let mut events = vec![Vec::new(); func.blocks.len()];
            for row in &mut events {
                let choice = pattern % patterns.len();
                pattern /= patterns.len();
                // Sparse, unrelated home IDs also exercise independence.
                for (home, kinds) in [
                    (SpillHome(3), patterns[choice]),
                    (SpillHome(10001), patterns[patterns.len() - 1 - choice]),
                ] {
                    for &kind in kinds {
                        row.push(PlannedStackEvent {
                            instruction: row.len(),
                            sequence: row.len(),
                            home,
                            kind,
                            definition: None,
                            reaching: None,
                        });
                    }
                }
            }
            assert_eq!(
                planned_phi_relations(&events, &cfg),
                reference_phi_relations(&events, &cfg)
            );
        }
    }
}
#[test]
fn reused_phi_scratch_matches_global_dataflow_on_permuted_loops() {
    for seed in 0..8usize {
        let mut values = VRegAllocator::new();
        let condition = values.alloc();
        let mut func = MFunction::new(values, vec![SpillDesc::transient()]);
        let id = |index: usize| BlockId(((index * 17) % 64 * 13 + 7) as u32);
        for index in 0..64 {
            let mut block = MBlock::new(id(index));
            if index == 0 {
                block.push(MInst::LoadImm {
                    dst: condition,
                    value: 1,
                });
            }
            if index == 63 {
                block.push(MInst::Return);
            } else {
                block.push(MInst::Branch {
                    cond: condition,
                    true_bb: id(index + 1),
                    false_bb: id(1 + (index * 7 + seed) % 63),
                });
            }
            func.blocks.push(block);
        }
        let cfg = cfg::normalize(&mut func).unwrap();
        let mut events = vec![Vec::new(); func.blocks.len()];
        for (block, row) in events.iter_mut().enumerate() {
            for home in 0..16usize {
                let kinds: &[PlannedStackEventKind] =
                    match (block * 17 + home * 31 + seed * 73) % 13 {
                        0..=2 => &[],
                        3..=5 => &[PlannedStackEventKind::Store],
                        6..=8 => &[PlannedStackEventKind::Reload],
                        _ => &[PlannedStackEventKind::Reload, PlannedStackEventKind::Store],
                    };
                for &kind in kinds {
                    row.push(PlannedStackEvent {
                        instruction: row.len(),
                        sequence: row.len(),
                        home: SpillHome(home as u32 * 100001),
                        kind,
                        definition: None,
                        reaching: None,
                    });
                }
            }
        }
        assert_eq!(
            planned_phi_relations(&events, &cfg),
            reference_phi_relations(&events, &cfg)
        );
    }
}

#[test]
fn production_sequential_homes_reuse_one_slot() {
    let mut source = function(0, vec![MInst::Return]);
    let cfg = cfg::normalize(&mut source).unwrap();
    let coloring = color_planned_events(
        &source,
        &cfg,
        &[(
            BlockId(0),
            vec![
                (PlannedStackEventKind::Store, SpillHome(0)),
                (PlannedStackEventKind::Reload, SpillHome(0)),
                (PlannedStackEventKind::Store, SpillHome(1)),
                (PlannedStackEventKind::Reload, SpillHome(1)),
            ],
        )],
    )
    .unwrap();

    assert_eq!(coloring.slot_count, 1);
    assert_eq!(coloring.frame_size, 8);
    assert_eq!(coloring.offsets[&SpillHome(0)], 0);
    assert_eq!(coloring.offsets[&SpillHome(1)], 0);
}

#[test]
fn production_overlapping_homes_keep_distinct_slots() {
    let mut source = function(0, vec![MInst::Return]);
    let cfg = cfg::normalize(&mut source).unwrap();
    let coloring = color_planned_events(
        &source,
        &cfg,
        &[(
            BlockId(0),
            vec![
                (PlannedStackEventKind::Store, SpillHome(0)),
                (PlannedStackEventKind::Store, SpillHome(1)),
                (PlannedStackEventKind::Reload, SpillHome(0)),
                (PlannedStackEventKind::Reload, SpillHome(1)),
            ],
        )],
    )
    .unwrap();

    assert_eq!(coloring.slot_count, 2);
    assert_eq!(coloring.frame_size, 16);
    assert_ne!(
        coloring.offsets[&SpillHome(0)],
        coloring.offsets[&SpillHome(1)]
    );
}

#[test]
fn production_mutually_exclusive_arms_share_one_slot() {
    let mut source = diamond_function();
    let cfg = cfg::normalize(&mut source).unwrap();
    let coloring = color_planned_events(
        &source,
        &cfg,
        &[
            (
                BlockId(1),
                vec![
                    (PlannedStackEventKind::Store, SpillHome(0)),
                    (PlannedStackEventKind::Reload, SpillHome(0)),
                ],
            ),
            (
                BlockId(2),
                vec![
                    (PlannedStackEventKind::Store, SpillHome(1)),
                    (PlannedStackEventKind::Reload, SpillHome(1)),
                ],
            ),
        ],
    )
    .unwrap();

    assert_eq!(coloring.slot_count, 1);
    assert_eq!(coloring.offsets[&SpillHome(0)], 0);
    assert_eq!(coloring.offsets[&SpillHome(1)], 0);
}

#[test]
fn production_loop_carried_home_interferes_with_body_home() {
    let mut values = VRegAllocator::new();
    let condition = values.alloc();
    let mut source = MFunction::new(values, vec![SpillDesc::transient()]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    source.blocks = vec![entry, header, body, exit];
    let cfg = cfg::normalize(&mut source).unwrap();
    let coloring = color_planned_events(
        &source,
        &cfg,
        &[
            (
                BlockId(0),
                vec![(PlannedStackEventKind::Store, SpillHome(0))],
            ),
            (
                BlockId(1),
                vec![(PlannedStackEventKind::Reload, SpillHome(0))],
            ),
            (
                BlockId(2),
                vec![
                    (PlannedStackEventKind::Store, SpillHome(1)),
                    (PlannedStackEventKind::Reload, SpillHome(1)),
                ],
            ),
        ],
    )
    .unwrap();

    assert_eq!(coloring.slot_count, 2);
    assert_ne!(
        coloring.offsets[&SpillHome(0)],
        coloring.offsets[&SpillHome(1)]
    );
}

#[test]
fn production_one_arm_store_cannot_satisfy_join_reload() {
    let mut source = diamond_function();
    let cfg = cfg::normalize(&mut source).unwrap();
    let error = color_planned_events(
        &source,
        &cfg,
        &[
            (
                BlockId(1),
                vec![(PlannedStackEventKind::Store, SpillHome(0))],
            ),
            (
                BlockId(3),
                vec![(PlannedStackEventKind::Reload, SpillHome(0))],
            ),
        ],
    )
    .unwrap_err();

    assert_eq!(error.rule, "STACK_COLOR.PLANNED_PHI_REACHING_STORE");
}
