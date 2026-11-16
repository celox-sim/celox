use super::*;
use crate::native::mir::{BaseReg, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator};

#[test]
fn byte_spill_sets_preserve_gaps_chunk_boundaries_and_shared_clones() {
    let pool = (0..100_000)
        .map(LogicalValue)
        .collect::<std::sync::Arc<[_]>>();
    let values = (10_000..10_130)
        .chain([10_255, 10_256, 10_511, 10_512])
        .chain(90_000..90_080)
        .map(LogicalValue)
        .collect::<Vec<_>>();
    let original = values.iter().copied().collect::<LogicalSet>();
    let mut frozen = original.clone();
    frozen.freeze(&pool);
    let LogicalSet::Frozen(row) = &frozen else {
        panic!("expected frozen set")
    };
    let FrozenLogicalStorage::Sparse8 { chunks, offsets } = &row.storage else {
        panic!("expected byte offsets")
    };
    assert_eq!(offsets.len(), values.len());
    assert!(offsets.len() + chunks.len() * 8 < values.len() * 2);
    assert_eq!(frozen, original);
    for value in (0..100_100).chain([u32::MAX]).map(LogicalValue) {
        assert_eq!(frozen.contains(&value), original.contains(&value));
    }
    let mut iter = frozen.iter();
    for (index, value) in values.iter().enumerate() {
        assert_eq!(iter.len(), values.len() - index);
        assert_eq!(iter.next(), Some(value));
    }
    assert_eq!(iter.len(), 0);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next(), None);
    let shared = frozen.clone();
    frozen.insert(LogicalValue(u32::MAX));
    frozen.remove(&LogicalValue(10_255));
    assert_eq!(shared, original);
    assert!(frozen.contains(&LogicalValue(u32::MAX)));
    assert!(!frozen.contains(&LogicalValue(10_255)));
}

#[test]
fn bounded_loop_candidates_match_stable_distance_order() {
    let values = (0..4096)
        .map(|id| {
            let distance = if id % 13 == 0 {
                NextUseDistance::Dead
            } else {
                NextUseDistance::Finite {
                    loop_exits: (id % 3) as usize,
                    instructions: (id % 17) as usize,
                }
            };
            (distance, LogicalValue(id))
        })
        .collect::<Vec<_>>();
    let mut expected = values.clone();
    expected.sort_by_key(|&(distance, _)| distance);
    for limit in [0, 1, 8, 16, 4096, 4097] {
        let mut closest = BinaryHeap::new();
        for &candidate in &values {
            retain_closest(&mut closest, limit, candidate);
            assert!(closest.len() <= limit);
        }
        assert_eq!(
            closest.into_sorted_vec(),
            expected[..limit.min(expected.len())]
        );
    }
}

#[test]
fn dense_spill_sets_match_sparse_encoding_across_empty_words() {
    let pool = (0..2048).map(LogicalValue).collect::<std::sync::Arc<[_]>>();
    let expected = (64..192)
        .chain(1024..1152)
        .map(LogicalValue)
        .collect::<Vec<_>>();
    let mut dense = expected.iter().copied().collect::<LogicalSet>();
    dense.freeze(&pool);
    let LogicalSet::Frozen(row) = &dense else {
        panic!("expected frozen set")
    };
    let FrozenLogicalStorage::Dense { words, len } = &row.storage else {
        panic!("expected dense set")
    };
    assert_eq!(*len, 256);
    assert_eq!(words.len(), 18);
    let sparse = LogicalSet::Frozen(std::sync::Arc::new(FrozenLogicalSet {
        values: pool,
        storage: FrozenLogicalStorage::Sparse {
            chunks: vec![(64, 0), (128, 64), (1024, 128), (1088, 192)].into_boxed_slice(),
            offsets: (0u16..64).cycle().take(256).collect(),
        },
    }));
    assert_eq!(dense, sparse);
    assert_eq!(sparse, dense);
    for value in (0..2048).chain([u32::MAX]).map(LogicalValue) {
        assert_eq!(
            dense.contains(&value),
            expected.binary_search(&value).is_ok()
        );
    }
    let mut iter = dense.iter();
    for (position, value) in expected.iter().enumerate() {
        assert_eq!(iter.len(), expected.len() - position);
        assert_eq!(iter.next(), Some(value));
    }
    assert_eq!(iter.len(), 0);
    assert_eq!(iter.next(), None);
    assert_eq!(iter.next(), None);
}

#[test]
fn frozen_spill_sets_preserve_membership_order_and_mutation() {
    let pool = (0..200_000)
        .map(LogicalValue)
        .collect::<std::sync::Arc<[_]>>();
    for stride in [1, 7, 257, 1025] {
        let values = (0..130)
            .map(|i| LogicalValue(i * stride))
            .collect::<Vec<_>>();
        let mut expected = values.iter().copied().collect::<BTreeSet<_>>();
        let mut actual = values.into_iter().collect::<LogicalSet>();
        let original = actual.clone();
        actual.freeze(&pool);
        assert!(matches!(actual, LogicalSet::Frozen(_)));
        assert_eq!(actual, original);
        assert_eq!(original, actual);
        for value in (0..200_000)
            .step_by(97)
            .map(LogicalValue)
            .chain(expected.iter().copied())
        {
            assert_eq!(actual.contains(&value), expected.contains(&value));
        }
        assert_eq!(
            actual.iter().copied().collect::<Vec<_>>(),
            expected.iter().copied().collect::<Vec<_>>()
        );
        assert_eq!(
            actual.clone().into_iter().collect::<Vec<_>>(),
            original.into_iter().collect::<Vec<_>>()
        );
        assert_eq!(
            actual.insert(LogicalValue(199_999)),
            expected.insert(LogicalValue(199_999))
        );
        actual.freeze(&pool);
        assert_eq!(
            actual.remove(&LogicalValue(0)),
            expected.remove(&LogicalValue(0))
        );
        actual.freeze(&pool);
        actual.retain(|value| value.0 % 3 != 0);
        expected.retain(|value| value.0 % 3 != 0);
        assert_eq!(
            actual.into_iter().collect::<Vec<_>>(),
            expected.into_iter().collect::<Vec<_>>()
        );
    }
    let mut invalid = (0..128)
        .map(LogicalValue)
        .chain([LogicalValue(u32::MAX)])
        .collect::<LogicalSet>();
    invalid.freeze(&pool);
    assert!(matches!(invalid, LogicalSet::Mutable(_)));
    assert!(invalid.contains(&LogicalValue(u32::MAX)));
}

#[test]
fn ordered_spill_differences_match_tree_sets_across_storage_shapes() {
    let pool = (0..200_000)
        .map(LogicalValue)
        .collect::<std::sync::Arc<[_]>>();
    let shapes = [
        Vec::new(),
        vec![LogicalValue(0), LogicalValue(199_999)],
        (0..512).map(LogicalValue).collect(),
        (0..512).map(|i| LogicalValue(i * 7)).collect(),
        (0..512).map(|i| LogicalValue(100_000 + i * 2)).collect(),
        (0..130).map(|i| LogicalValue(i * 1025)).collect(),
        (0..2000).map(|i| LogicalValue(i * 97)).collect(),
    ];
    for left in &shapes {
        for right in &shapes {
            let expected_left = left.iter().copied().collect::<BTreeSet<_>>();
            let expected_right = right.iter().copied().collect::<BTreeSet<_>>();
            let expected = expected_left
                .difference(&expected_right)
                .copied()
                .collect::<Vec<_>>();
            for freeze_left in [false, true] {
                for freeze_right in [false, true] {
                    let mut left = left.iter().copied().collect::<LogicalSet>();
                    let mut right = right.iter().copied().collect::<LogicalSet>();
                    if freeze_left {
                        left.freeze(&pool);
                    }
                    if freeze_right {
                        right.freeze(&pool);
                    }
                    assert_eq!(
                        left.difference(&right).copied().collect::<Vec<_>>(),
                        expected
                    );
                    // Edge coupling can omit renamed phi destinations from
                    // its ordered queries and can query a value repeatedly.
                    let mut membership = OrderedMembership::new(&right, left.len());
                    for value in left.iter().step_by(3) {
                        assert_eq!(membership.contains(value), expected_right.contains(value));
                        assert_eq!(membership.contains(value), expected_right.contains(value));
                    }
                }
            }
        }
    }
}

#[test]
fn remaining_use_arena_preserves_preferred_order_and_duplicate_operands() {
    let mut vregs = VRegAllocator::new();
    let root = vregs.alloc();
    let x = vregs.alloc();
    let y = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: root,
        value: 9,
    });
    block.push(MInst::Add {
        dst: x,
        lhs: root,
        rhs: root,
    });
    block.push(MInst::AddImm {
        dst: y,
        src: root,
        imm: 1,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: root,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let value = logical.checked_of(root, Some(BlockId(0)), None).unwrap();
    let mut costs = HashMap::default();
    let mut remaining = RemainingBlockUses::build(
        &func,
        &next_use,
        &logical,
        0,
        &mut costs,
        Some(&[0, 3, 2, 1, 4]),
    )
    .unwrap();
    assert_eq!(remaining.points.len(), 3);
    assert_eq!(remaining.remaining_uses(value), 3);
    assert_eq!(remaining.next_point(value).unwrap().instruction, 3);
    for (source, count, next) in [
        (0, 3, Some(3)),
        (2, 2, Some(3)),
        (3, 1, Some(1)),
        (1, 0, None),
        (4, 0, None),
    ] {
        remaining
            .emit(source, &func.blocks[0].insts[source], &logical)
            .unwrap();
        assert_eq!(remaining.remaining_uses(value), count);
        assert_eq!(
            remaining.next_point(value).map(|point| point.instruction),
            next
        );
    }
    assert_eq!(remaining.distance(value), NextUseDistance::Dead);
}

#[test]
fn remaining_use_score_invalidations_are_bounded_for_shared_operands() {
    for lanes in [1usize, 2, 128, 4096] {
        for live_out in [false, true] {
            let mut vregs = VRegAllocator::new();
            let root = vregs.alloc();
            let results = (0..lanes).map(|_| vregs.alloc()).collect::<Vec<_>>();
            let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); lanes + 1]);
            let mut block = MBlock::new(BlockId(0));
            block.push(MInst::LoadImm {
                dst: root,
                value: 123,
            });
            for (i, &dst) in results.iter().enumerate() {
                block.push(MInst::AndImm {
                    dst,
                    src: root,
                    imm: i as u64,
                });
            }
            if live_out {
                block.push(MInst::Jump { target: BlockId(1) });
                func.push_block(block);
                let mut exit = MBlock::new(BlockId(1));
                exit.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: root,
                    size: OpSize::S64,
                });
                exit.push(MInst::Return);
                func.push_block(exit);
            } else {
                block.push(MInst::Return);
                func.push_block(block);
            }
            let cfg = super::super::cfg::normalize(&mut func).unwrap();
            let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
            let logical = LogicalValues::build(&func);
            let mut costs = HashMap::default();
            let mut remaining =
                RemainingBlockUses::build(&func, &next_use, &logical, 0, &mut costs, None).unwrap();
            let value = logical.checked_of(root, Some(BlockId(0)), None).unwrap();
            assert_eq!(remaining.is_live_out(value), live_out);
            assert!(
                remaining
                    .emit(0, &func.blocks[0].insts[0], &logical)
                    .unwrap()
                    .is_empty()
            );
            // Consume out of source order. Both counts and the next-use
            // cursor must advance even when no score is invalidated.
            let order = (1..=lanes).step_by(2).chain((2..=lanes).step_by(2));
            let mut pending = (1..=lanes).collect::<BTreeSet<_>>();
            let mut notifications = 0;
            for source in order {
                pending.remove(&source);
                let changed = remaining
                    .emit(source, &func.blocks[0].insts[source], &logical)
                    .unwrap();
                let expected = if pending.len() <= 1 && !live_out {
                    vec![value]
                } else {
                    vec![]
                };
                assert_eq!(
                    changed, expected,
                    "lanes={lanes}, live_out={live_out}, source={source}"
                );
                notifications += changed.len();
                assert_eq!(remaining.remaining_uses(value), pending.len());
                let uses = &remaining.remaining[&value];
                assert_eq!(
                    remaining.points[uses.points.clone()]
                        .get(uses.next)
                        .map(|&(_, source)| source),
                    pending.first().copied()
                );
            }
            assert_eq!(notifications, if live_out { 0 } else { lanes.min(2) });
        }
    }
}

#[test]
fn integrated_ready_walk_closes_resident_lanes_before_starting_new_roots() {
    const LANES: usize = 32;
    let mut vregs = VRegAllocator::new();
    let roots = (0..LANES).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let results = (0..LANES).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); LANES * 2]);
    let mut block = MBlock::new(BlockId(0));
    for (lane, &root) in roots.iter().enumerate() {
        block.push(MInst::LoadImm {
            dst: root,
            value: lane as u64,
        });
    }
    for (&root, &result) in roots.iter().zip(&results) {
        block.push(MInst::AndImm {
            dst: result,
            src: root,
            imm: 1,
        });
    }
    block.push(MInst::Return);
    func.push_block(block);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let constraints = super::super::constraints::ConstraintModel::build(&func, &cfg).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let recipes = PlanningRecipes::stack_only(func.vregs.count());
    let (_, order) = plan_scheduled_block_transition(
        &func,
        &next_use,
        &recipes,
        &logical,
        &homes,
        0,
        4,
        &LogicalSet::new(),
        LogicalSet::new(),
        &constraints.instructions[0],
        &mut HashMap::default(),
    )
    .unwrap();

    let mut outstanding = BTreeSet::new();
    assert!(
        order
            .iter()
            .position(|source| *source >= LANES)
            .is_some_and(|position| position <= 4),
        "a direct consumer must run no later than the resident-capacity boundary"
    );
    for &source in &order[..order.len() - 1] {
        if source < LANES {
            outstanding.insert(source);
        } else {
            assert!(
                outstanding.remove(&(source - LANES)),
                "a lane consumer must follow its root"
            );
        }
    }
    assert!(outstanding.is_empty());
    assert_eq!(order.last(), Some(&(LANES * 2)));
}

#[test]
fn integrated_ready_walk_builds_the_earliest_bounded_sink_packet() {
    let mut vregs = VRegAllocator::new();
    let long_root = vregs.alloc();
    let short_root = vregs.alloc();
    let short_result = vregs.alloc();
    let filler_root = vregs.alloc();
    let filler_result = vregs.alloc();
    let long_result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 6]);
    let mut block = MBlock::new(BlockId(0));
    // Root source order starts `long_root` and keeps it resident while
    // the independent short cluster executes. Sink-directed order starts
    // from the earliest result and first tries to close its dependency
    // cone without exceeding the physical register capacity.
    block.push(MInst::LoadImm {
        dst: long_root,
        value: 1,
    });
    block.push(MInst::LoadImm {
        dst: short_root,
        value: 2,
    });
    block.push(MInst::AndImm {
        dst: short_result,
        src: short_root,
        imm: 1,
    });
    block.push(MInst::LoadImm {
        dst: filler_root,
        value: 3,
    });
    block.push(MInst::AndImm {
        dst: filler_result,
        src: filler_root,
        imm: 1,
    });
    block.push(MInst::AndImm {
        dst: long_result,
        src: long_root,
        imm: 1,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let constraints = super::super::constraints::ConstraintModel::build(&func, &cfg).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let recipes = PlanningRecipes::stack_only(func.vregs.count());
    let (_, order) = plan_scheduled_block_transition(
        &func,
        &next_use,
        &recipes,
        &logical,
        &homes,
        0,
        4,
        &LogicalSet::new(),
        LogicalSet::new(),
        &constraints.instructions[0],
        &mut HashMap::default(),
    )
    .unwrap();

    assert_eq!(&order[..2], &[1, 2]);
    assert!(
        order.iter().position(|source| *source == 0) < order.iter().position(|source| *source == 5)
    );
    assert_eq!(order.last(), Some(&6));
}

#[test]
fn bounded_sink_packet_materializes_a_shared_producer_once_for_adjacent_sinks() {
    let mut vregs = VRegAllocator::new();
    let distant_root = vregs.alloc();
    let shared_root = vregs.alloc();
    let first_result = vregs.alloc();
    let second_result = vregs.alloc();
    let filler_root = vregs.alloc();
    let filler_result = vregs.alloc();
    let distant_result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 7]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: distant_root,
        value: 1,
    });
    block.push(MInst::LoadImm {
        dst: shared_root,
        value: 2,
    });
    block.push(MInst::AndImm {
        dst: first_result,
        src: shared_root,
        imm: 1,
    });
    block.push(MInst::AndImm {
        dst: second_result,
        src: shared_root,
        imm: 2,
    });
    block.push(MInst::LoadImm {
        dst: filler_root,
        value: 3,
    });
    block.push(MInst::AndImm {
        dst: filler_result,
        src: filler_root,
        imm: 1,
    });
    block.push(MInst::AndImm {
        dst: distant_result,
        src: distant_root,
        imm: 1,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let constraints = super::super::constraints::ConstraintModel::build(&func, &cfg).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let recipes = PlanningRecipes::stack_only(func.vregs.count());
    let (_, order) = plan_scheduled_block_transition(
        &func,
        &next_use,
        &recipes,
        &logical,
        &homes,
        0,
        4,
        &LogicalSet::new(),
        LogicalSet::new(),
        &constraints.instructions[0],
        &mut HashMap::default(),
    )
    .unwrap();

    assert_eq!(&order[..3], &[1, 2, 3]);
    assert_eq!(order.iter().filter(|&&source| source == 1).count(), 1);
    assert_eq!(order.last(), Some(&7));
}

#[test]
fn exit_reload_price_is_deduplicated_per_edge_and_summed_across_edges() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let source = vregs.alloc();
    let true_value = vregs.alloc();
    let false_value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: source,
        value: 2,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut true_block = MBlock::new(BlockId(1));
    true_block.phis.push(PhiNode {
        dst: true_value,
        sources: vec![(BlockId(0), source)],
    });
    true_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: true_value,
        size: OpSize::S64,
    });
    true_block.push(MInst::Return);

    let mut false_block = MBlock::new(BlockId(2));
    false_block.phis.push(PhiNode {
        dst: false_value,
        sources: vec![(BlockId(0), source)],
    });
    false_block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: false_value,
        size: OpSize::S64,
    });
    false_block.push(MInst::Return);
    func.blocks = vec![entry, true_block, false_block];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
    let recipes = PlanningRecipes::stack_only(func.vregs.count());
    let entry = cfg.block_index[&BlockId(0)];
    let true_block = cfg.block_index[&BlockId(1)];
    let false_block = cfg.block_index[&BlockId(2)];
    let costs = exit_reload_costs(
        &func,
        &cfg,
        &next_use,
        &recipes,
        &logical,
        &translations,
        entry,
    )
    .unwrap();
    let source = LogicalValue(source.0);
    let expected = u32::from(reload_cost_on_edge(
        &func, &recipes, entry, true_block, source,
    ))
    .saturating_add(u32::from(reload_cost_on_edge(
        &func,
        &recipes,
        entry,
        false_block,
        source,
    )));

    assert_eq!(costs.get(&source), Some(&expected));
    assert_eq!(costs.len(), 1, "only the shared phi source is live out");
    assert_lazy_exit_costs_match(&func, &cfg, &next_use, &recipes, &logical, &translations);
}

fn assert_lazy_exit_costs_match(
    func: &MFunction,
    cfg: &NormalizedCfg,
    next_use: &NextUseAnalysis,
    recipes: &PlanningRecipes,
    logical: &LogicalValues,
    translations: &EdgeTranslations,
) {
    for block in 0..func.blocks.len() {
        let mut expected =
            exit_reload_costs(func, cfg, next_use, recipes, logical, translations, block).unwrap();
        let mut lazy = LazyExitReloadCosts {
            func,
            cfg,
            next_use,
            recipes,
            translations,
            block,
            cache: HashMap::default(),
        };
        assert!(lazy.cache.is_empty());
        // Query one identity repeatedly before touching the rest. Neither
        // live-set size nor repeated requests may grow the block cache.
        for _ in 0..3 {
            assert_eq!(lazy.cost(LogicalValue(0)), expected.cost(LogicalValue(0)));
            assert_eq!(lazy.cache.len(), 1);
        }
        for value in (0..func.vregs.count()).rev().map(LogicalValue) {
            assert_eq!(
                lazy.cost(value),
                expected.cost(value),
                "block {block}, {value:?}"
            );
        }
    }
}

#[test]
fn lazy_exit_prices_match_eager_demands_at_loops_and_duplicate_phis() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let initial = vregs.alloc();
    let first = vregs.alloc();
    let second = vregs.alloc();
    let unused = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: initial,
        value: 7,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    for (dst, backedge) in [(first, second), (second, first), (unused, first)] {
        header.phis.push(PhiNode {
            dst,
            sources: vec![(BlockId(0), initial), (BlockId(2), backedge)],
        });
    }
    header.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });
    let mut body = MBlock::new(BlockId(2));
    body.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: first,
        size: OpSize::S64,
    });
    body.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(3));
    for (offset, src) in [(0, first), (8, second), (16, initial)] {
        exit.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src,
            size: OpSize::S64,
        });
    }
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, body, exit];
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
    let recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
    assert_lazy_exit_costs_match(&func, &cfg, &next_use, &recipes, &logical, &translations);
}

#[test]
fn entry_value_evicted_before_first_use_is_planned_as_a_memory_phi() {
    let mut vregs = VRegAllocator::new();
    let initial = vregs.alloc();
    let merged = vregs.alloc();
    let pressure_a = vregs.alloc();
    let pressure_b = vregs.alloc();
    let next = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: initial,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });

    let mut header = MBlock::new(BlockId(1));
    header.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(0), initial), (BlockId(2), next)],
    });
    header.push(MInst::LoadImm {
        dst: pressure_a,
        value: 2,
    });
    header.push(MInst::LoadImm {
        dst: pressure_b,
        value: 3,
    });
    header.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: pressure_a,
        size: OpSize::S64,
    });
    header.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: pressure_b,
        size: OpSize::S64,
    });
    header.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: merged,
        size: OpSize::S64,
    });
    header.push(MInst::Jump { target: BlockId(2) });

    let mut latch = MBlock::new(BlockId(2));
    latch.push(MInst::Mov {
        dst: next,
        src: merged,
    });
    latch.push(MInst::Branch {
        cond: merged,
        true_bb: BlockId(1),
        false_bb: BlockId(3),
    });

    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, latch, exit];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let plan = plan(&func, &cfg, &next_use, 2).unwrap();
    plan.verify(&func, &cfg, 2).unwrap();

    let header = cfg.block_index[&BlockId(1)];
    let merged = LogicalValue(merged.0);
    assert!(next_use.region_at_entry(header).is_some());
    assert!(!plan.w_entry[header].contains(&merged));
    assert!(plan.point_ops.iter().any(|(point, operation)| {
        point.block == BlockId(1)
            && matches!(operation, PlannedOp::SpillPhi { value, .. } if *value == merged)
    }));
    assert!(plan.point_ops.iter().any(|(point, operation)| {
        point.block == BlockId(1)
            && point.instruction == 4
            && matches!(operation, PlannedOp::Reload { value, .. } if *value == merged)
    }));
    assert!(plan.point_ops.iter().all(|(point, operation)| {
        point.block != BlockId(1)
            || !matches!(operation, PlannedOp::Spill { value, .. } if *value == merged)
    }));
    let merged_home = plan.homes.of_logical(merged);
    for &(predecessor_id, source) in &func.blocks[header].phis[0].sources {
        let predecessor = cfg.block_index[&predecessor_id];
        assert!(
            plan.edge_ops
                .get(&(predecessor, header))
                .is_some_and(|operations| operations.iter().any(|operation| {
                    matches!(
                        operation,
                        PlannedEdgeOp::Spill {
                            destination,
                            destination_home,
                            ..
                        } if *destination == merged && *destination_home == merged_home
                    )
                })),
            "missing explicit transfer into the loop-phi home: {plan:#?}"
        );
        assert_ne!(plan.homes.of_vreg(source), merged_home);
    }
}

#[test]
fn single_predecessor_inherits_residency_without_edge_reconciliation() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);

    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.push(MInst::Return);
    func.blocks = vec![predecessor, successor];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
    let mut plan = SpillPlan {
        logical,
        homes,
        point_ops: Vec::new(),
        edge_ops: BTreeMap::new(),
        recipe_reloads: BTreeSet::new(),
        recipe_homes: BTreeSet::new(),
        state_homes: BTreeMap::new(),
        state_reload_recipes: BTreeMap::new(),
        w_entry: vec![LogicalSet::new(); func.blocks.len()],
        w_exit: vec![LogicalSet::new(); func.blocks.len()],
        s_entry: vec![LogicalSet::new(); func.blocks.len()],
        s_exit: vec![LogicalSet::new(); func.blocks.len()],
    };
    let predecessor = cfg.block_index[&BlockId(0)];
    let successor = cfg.block_index[&BlockId(1)];
    let logical_value = LogicalValue(value.0);
    plan.w_exit[predecessor].insert(logical_value);
    plan.s_exit[predecessor].insert(logical_value);

    assert!(!next_use.anticipated_at_entry(successor, value));
    let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
    let inherited = init_usual(
        &func,
        &cfg,
        &next_use,
        &planning_recipes,
        &plan,
        &translations,
        successor,
        1,
    );

    assert_eq!(inherited, LogicalSet::from_iter([logical_value]));
}

#[test]
fn join_retention_pays_for_guaranteed_use_but_delays_one_arm_use() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let conditional = vregs.alloc();
    let guaranteed = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: conditional,
        value: 2,
    });
    entry.push(MInst::LoadImm {
        dst: guaranteed,
        value: 3,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut first = MBlock::new(BlockId(1));
    first.push(MInst::Jump { target: BlockId(3) });
    let mut second = MBlock::new(BlockId(2));
    second.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(4),
        false_bb: BlockId(5),
    });
    let mut use_arm = MBlock::new(BlockId(4));
    use_arm.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: conditional,
        size: OpSize::S64,
    });
    use_arm.push(MInst::Jump { target: BlockId(6) });
    let mut skip_arm = MBlock::new(BlockId(5));
    skip_arm.push(MInst::Jump { target: BlockId(6) });
    let mut tail = MBlock::new(BlockId(6));
    tail.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: guaranteed,
        size: OpSize::S64,
    });
    tail.push(MInst::Return);
    func.blocks = vec![entry, first, second, join, use_arm, skip_arm, tail];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    next_use.verify(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
    let mut plan = SpillPlan {
        logical,
        homes,
        point_ops: Vec::new(),
        edge_ops: BTreeMap::new(),
        recipe_reloads: BTreeSet::new(),
        recipe_homes: BTreeSet::new(),
        state_homes: BTreeMap::new(),
        state_reload_recipes: BTreeMap::new(),
        w_entry: vec![LogicalSet::new(); func.blocks.len()],
        w_exit: vec![LogicalSet::new(); func.blocks.len()],
        s_entry: vec![LogicalSet::new(); func.blocks.len()],
        s_exit: vec![LogicalSet::new(); func.blocks.len()],
    };
    let join = cfg.block_index[&BlockId(3)];
    let predecessors = &cfg.predecessors[join];
    assert_eq!(predecessors.len(), 2);
    assert!(predecessors.iter().all(|predecessor| *predecessor < join));
    plan.w_exit[predecessors[0]].extend([LogicalValue(conditional.0), LogicalValue(guaranteed.0)]);

    assert!(!next_use.anticipated_at_entry(join, conditional));
    assert!(next_use.anticipated_at_entry(join, guaranteed));
    let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
    let retained = init_usual(
        &func,
        &cfg,
        &next_use,
        &planning_recipes,
        &plan,
        &translations,
        join,
        1,
    );

    assert_eq!(
        retained,
        LogicalSet::from_iter([LogicalValue(guaranteed.0)])
    );
}

#[test]
fn join_retention_prices_every_use_in_a_straight_line_cluster() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let single_use = vregs.alloc();
    let repeated_use = vregs.alloc();
    let sum = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: single_use,
        value: 2,
    });
    entry.push(MInst::LoadImm {
        dst: repeated_use,
        value: 3,
    });
    entry.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut first = MBlock::new(BlockId(1));
    first.push(MInst::Jump { target: BlockId(3) });
    let mut second = MBlock::new(BlockId(2));
    second.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    // Both values have the same first-use point.  A first-use-only model
    // therefore chooses the lower VReg tie-break (`single_use`), while the
    // complete local cluster must retain `repeated_use`.
    join.push(MInst::Add {
        dst: sum,
        lhs: single_use,
        rhs: repeated_use,
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: repeated_use,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, first, second, join];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
    let mut plan = SpillPlan {
        logical,
        homes,
        point_ops: Vec::new(),
        edge_ops: BTreeMap::new(),
        recipe_reloads: BTreeSet::new(),
        recipe_homes: BTreeSet::new(),
        state_homes: BTreeMap::new(),
        state_reload_recipes: BTreeMap::new(),
        w_entry: vec![LogicalSet::new(); func.blocks.len()],
        w_exit: vec![LogicalSet::new(); func.blocks.len()],
        s_entry: vec![LogicalSet::new(); func.blocks.len()],
        s_exit: vec![LogicalSet::new(); func.blocks.len()],
    };
    let join = cfg.block_index[&BlockId(3)];
    let predecessors = &cfg.predecessors[join];
    plan.w_exit[predecessors[0]].extend([LogicalValue(single_use.0), LogicalValue(repeated_use.0)]);
    let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());

    let retained = init_usual(
        &func,
        &cfg,
        &next_use,
        &planning_recipes,
        &plan,
        &translations,
        join,
        1,
    );

    assert_eq!(
        retained,
        LogicalSet::from_iter([LogicalValue(repeated_use.0)])
    );
    next_use.verify(&func, &cfg).unwrap();
}

#[test]
fn reused_edge_source_maps_to_every_destination() {
    let mut vregs = VRegAllocator::new();
    let source = vregs.alloc();
    let first = vregs.alloc();
    let second = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm {
        dst: source,
        value: 1,
    });
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.phis = vec![
        PhiNode {
            dst: first,
            sources: vec![(BlockId(0), source)],
        },
        PhiNode {
            dst: second,
            sources: vec![(BlockId(0), source)],
        },
    ];
    successor.push(MInst::Return);
    func.blocks = vec![predecessor, successor];
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let logical = LogicalValues::build(&func);

    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();
    let mapped = translations
        .to_successors(0, 1, LogicalValue(source.0))
        .collect::<Vec<_>>();

    assert_eq!(mapped, [LogicalValue(first.0), LogicalValue(second.0)]);
    assert_eq!(
        translations.to_predecessor(0, 1, LogicalValue(first.0)),
        LogicalValue(source.0)
    );
    assert_eq!(
        translations.to_predecessor(0, 1, LogicalValue(second.0)),
        LogicalValue(source.0)
    );
}

#[test]
fn scratch_reload_splits_the_predecessor_phi_source_identity() {
    let source = LogicalValue(3);
    let destination = LogicalValue(9);
    let destination_home = SpillHome(9);
    let operations = [
        PlannedEdgeOp::Spill {
            source,
            destination,
            destination_home,
        },
        PlannedEdgeOp::Reload {
            source,
            source_home: destination_home,
            destination,
        },
    ];

    assert!(edge_reload_uses_transferred_home(&operations, 1));
    assert!(matches!(
        operations[1],
        PlannedEdgeOp::Reload {
            source: reload_source,
            destination: reload_destination,
            ..
        } if reload_source == source && reload_destination == destination
    ));
}

#[test]
fn excessive_operand_pressure_is_a_structured_error() {
    let mut vregs = VRegAllocator::new();
    let left = vregs.alloc();
    let right = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: left,
        value: 1,
    });
    block.push(MInst::LoadImm {
        dst: right,
        value: 2,
    });
    block.push(MInst::Add {
        dst: result,
        lhs: left,
        rhs: right,
    });
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();

    let error = plan(&func, &cfg, &next_use, 1).unwrap_err();

    assert_eq!(error.rule, "SPILL_PLAN.OPERAND_PRESSURE");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.instruction, Some(2));
    assert_eq!(error.values, vec![left, right]);
}

#[test]
fn excessive_clobber_pressure_is_a_structured_error() {
    let mut vregs = VRegAllocator::new();
    let input = vregs.alloc();
    let result = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: input,
        value: 1,
    });
    block.push(MInst::UDiv {
        dst: result,
        lhs: input,
        rhs: input,
    });
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();

    let error = plan(&func, &cfg, &next_use, 1).unwrap_err();

    assert_eq!(error.rule, "SPILL_PLAN.CLOBBER_CAPACITY");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.instruction, Some(1));
}

#[test]
fn eviction_uses_target_cost_density_and_preserves_min_as_tie_breaker() {
    let mut vregs = VRegAllocator::new();
    let cheap = vregs.alloc();
    let costly = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::remat(1), SpillDesc::transient()]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let planning_recipes = PlanningRecipes::stack_only(func.vregs.count());
    let spilled = LogicalSet::new();
    let mut future = LinearFutureUses {
        func: &func,
        next_use: &next_use,
        block: 0,
        instruction: 0,
    };
    let local = |instructions| NextUseDistance::Finite {
        loop_exits: 0,
        instructions,
    };

    assert_eq!(
        compare_eviction_candidates(
            &func,
            &planning_recipes,
            &spilled,
            &mut future,
            (LogicalValue(cheap.0), local(1)),
            (LogicalValue(costly.0), local(1)),
        ),
        Ordering::Greater,
        "equal spans must evict the cheaper rematerializable value"
    );
    assert_eq!(
        compare_eviction_candidates(
            &func,
            &planning_recipes,
            &spilled,
            &mut future,
            (LogicalValue(cheap.0), local(1)),
            (LogicalValue(costly.0), local(15)),
        ),
        Ordering::Less,
        "a sufficiently long occupancy interval must outweigh a larger split cost"
    );

    let mut equal_cost = MFunction::new(
        func.vregs.clone(),
        vec![SpillDesc::transient(), SpillDesc::transient()],
    );
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Return);
    equal_cost.push_block(block);
    let equal_cfg = super::super::cfg::normalize(&mut equal_cost).unwrap();
    let equal_next_use = super::super::next_use::analyze(&equal_cost, &equal_cfg).unwrap();
    let equal_recipes = PlanningRecipes::stack_only(equal_cost.vregs.count());
    let mut equal_future = LinearFutureUses {
        func: &equal_cost,
        next_use: &equal_next_use,
        block: 0,
        instruction: 0,
    };
    assert_eq!(
        compare_eviction_candidates(
            &equal_cost,
            &equal_recipes,
            &spilled,
            &mut equal_future,
            (LogicalValue(cheap.0), local(2)),
            (LogicalValue(costly.0), local(8)),
        ),
        Ordering::Less,
        "equal target costs must reduce to furthest-next-use MIN"
    );
    let exact_recipes = PlanningRecipes::with_global_costs(vec![Some(1), None]);
    assert_eq!(
        compare_eviction_candidates(
            &equal_cost,
            &exact_recipes,
            &spilled,
            &mut equal_future,
            (LogicalValue(cheap.0), local(2)),
            (LogicalValue(costly.0), local(2)),
        ),
        Ordering::Greater,
        "an exact reload recipe must override the stale transient descriptor cost"
    );
}

#[test]
fn point_specific_memoryssa_cost_changes_the_allocator_owned_split() {
    let mut vregs = VRegAllocator::new();
    let state_backed = vregs.alloc();
    let stack_backed = vregs.alloc();
    let near_use = vregs.alloc();
    let pressure = vregs.alloc();
    let sum = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: state_backed,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: state_backed,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: stack_backed,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: near_use,
        base: BaseReg::StackFrame,
        offset: 16,
        size: OpSize::S64,
    });
    block.push(MInst::LoadImm {
        dst: pressure,
        value: 0,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 24,
        src: near_use,
        size: OpSize::S64,
    });
    block.push(MInst::Add {
        dst: sum,
        lhs: state_backed,
        rhs: stack_backed,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 48,
        src: sum,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let exact = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
    let stack_only = PlanningRecipes::stack_only(func.vregs.count());

    let exact_plan = plan_with_recipe_costs(&func, &cfg, &next_use, &exact, 3).unwrap();
    let stack_plan = plan_with_recipe_costs(&func, &cfg, &next_use, &stack_only, 3).unwrap();
    let split_at_pressure = |plan: &SpillPlan| {
        plan.point_ops.iter().find_map(|(point, operation)| {
            (point.block == BlockId(0) && point.instruction == 4)
                .then_some(operation)
                .and_then(|operation| match operation {
                    PlannedOp::Spill { value, .. } => Some(*value),
                    _ => None,
                })
        })
    };

    assert_eq!(split_at_pressure(&exact_plan), None);
    assert!(
        exact_plan
            .recipe_reloads
            .contains(&(BlockId(0), 6, LogicalValue(state_backed.0)))
    );
    assert!(exact_plan.point_ops.iter().any(|(point, operation)| {
        point.block == BlockId(0)
            && point.instruction == 6
            && matches!(
                operation,
                PlannedOp::Reload { value, .. }
                    if *value == LogicalValue(state_backed.0)
            )
    }));
    assert_eq!(
        split_at_pressure(&stack_plan),
        Some(LogicalValue(stack_backed.0)),
        "without a point recipe equal-cost MIN uses the deterministic VReg tie-break"
    );
}

#[test]
fn point_recipe_splits_one_cluster_before_a_later_invalidated_use() {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let pressure = vregs.alloc();
    let overwrite = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: stored,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: pressure,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 24,
        src: pressure,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 32,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: overwrite,
        base: BaseReg::StackFrame,
        offset: 16,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: overwrite,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 40,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();

    let plan = plan_with_recipe_costs(&func, &cfg, &next_use, &recipes, 1).unwrap();

    assert!(
        plan.recipe_reloads
            .contains(&(BlockId(0), 4, LogicalValue(stored.0)))
    );
    assert!(plan.point_ops.iter().any(|(point, operation)| {
        point.block == BlockId(0)
            && point.instruction == 5
            && matches!(
                operation,
                PlannedOp::Spill { value, .. } if *value == LogicalValue(stored.0)
            )
    }));
    assert!(!plan.point_ops.iter().any(|(point, operation)| {
        point.block == BlockId(0)
            && point.instruction == 2
            && matches!(
                operation,
                PlannedOp::Spill { value, .. } if *value == LogicalValue(stored.0)
            )
    }));
}

#[test]
fn stable_recipe_identity_materializes_at_the_output_position() {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let pressure = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: stored,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: pressure,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 16,
        src: pressure,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 24,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let recipes = super::super::reload::analyze_for_planning(&func, &cfg).unwrap();
    let logical = LogicalValues::build(&func);
    let homes = SpillHomes::build(&func).unwrap();
    let mut planner = BlockTransitionPlanner::new(
        &func,
        &next_use,
        &recipes,
        &logical,
        &homes,
        0,
        1,
        &LogicalSet::new(),
        LogicalSet::new(),
    )
    .unwrap();
    for (source, inst) in func.blocks[0].insts.iter().enumerate() {
        planner
            .step(
                TransitionPoint {
                    output: if source >= 4 { source + 3 } else { source },
                    source,
                },
                inst,
            )
            .unwrap();
    }
    let transition = planner.finish().unwrap();
    let stored = LogicalValue(stored.0);

    assert!(transition.recipe_reloads.contains(&(BlockId(0), 7, stored)));
    assert!(transition.point_ops.iter().any(|(point, operation)| {
        point.instruction == 7
            && matches!(operation, PlannedOp::Reload { value, .. } if *value == stored)
    }));
}

#[test]
fn whole_recipe_home_requires_an_exact_recipe_at_every_selected_reload() {
    let mut vregs = VRegAllocator::new();
    let stored = vregs.alloc();
    let overwrite = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: stored,
        base: BaseReg::StackFrame,
        offset: 0,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 16,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: overwrite,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 40,
        src: overwrite,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 24,
        src: stored,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let mut plan = plan(&func, &cfg, &next_use, 2).unwrap();
    plan.point_ops.clear();
    plan.edge_ops.clear();
    plan.recipe_reloads.clear();
    plan.recipe_homes.clear();
    let logical = LogicalValue(stored.0);
    let home = plan.homes.of_logical(logical);
    let reload = |instruction| {
        (
            ProgramPoint {
                block: BlockId(0),
                instruction,
                side: PointSide::Before,
            },
            PlannedOp::Reload {
                value: logical,
                home,
            },
        )
    };
    plan.point_ops.extend([reload(2), reload(5)]);
    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();

    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    assert!(plan.recipe_homes.is_empty());

    plan.point_ops.pop();
    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    assert_eq!(plan.recipe_homes, BTreeSet::from([home]));
    plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();

    plan.point_ops.push(reload(5));
    let error = plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap_err();
    assert_eq!(error.rule, "SPILL_PLAN.RECIPE_HOME_POINT");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.instruction, Some(5));
}

#[test]
fn whole_recipe_home_uses_the_existing_mixed_plan_as_its_baseline() {
    let mut vregs = VRegAllocator::new();
    let base = vregs.alloc();
    let first = vregs.alloc();
    let value = vregs.alloc();
    let overwrite = vregs.alloc();
    let mut spill_descs = vec![SpillDesc::transient(); 4];
    spill_descs[value.0 as usize].spill_cost = 1;
    let mut func = MFunction::new(vregs, spill_descs);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: base,
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::BitNot {
        dst: first,
        src: base,
    });
    block.push(MInst::BitNot {
        dst: value,
        src: first,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 48,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Load {
        dst: overwrite,
        base: BaseReg::StackFrame,
        offset: 8,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 48,
        src: overwrite,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 16,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let mut plan = plan(&func, &cfg, &next_use, 4).unwrap();
    plan.point_ops.clear();
    plan.edge_ops.clear();
    plan.recipe_reloads.clear();
    plan.recipe_homes.clear();
    let logical = LogicalValue(value.0);
    let home = plan.homes.of_logical(logical);
    let point = |instruction| ProgramPoint {
        block: BlockId(0),
        instruction,
        side: PointSide::Before,
    };
    plan.point_ops.extend([
        (
            point(3),
            PlannedOp::Spill {
                value: logical,
                home,
            },
        ),
        (
            point(4),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ),
        (
            point(7),
            PlannedOp::Reload {
                value: logical,
                home,
            },
        ),
    ]);
    plan.recipe_reloads.insert((BlockId(0), 4, logical));
    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();
    let recipe_cost = |instruction| {
        recipes
            .resolved_recipe_at_point(PointUse {
                block: BlockId(0),
                instruction,
                value,
            })
            .map(|recipe| recipe.steps.len() + 1)
    };
    assert_eq!(recipe_cost(4), Some(1));
    assert_eq!(recipe_cost(7), Some(3));

    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    assert!(
        plan.recipe_homes.is_empty(),
        "the all-recipe cost ties the selected point-recipe, stack-reload, and spill baseline"
    );
}

#[test]
fn whole_recipe_home_compares_complete_stack_and_recipe_costs() {
    let mut vregs = VRegAllocator::new();
    let base = vregs.alloc();
    let first = vregs.alloc();
    let value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Load {
        dst: base,
        base: BaseReg::SimState,
        offset: 40,
        size: OpSize::S64,
    });
    block.push(MInst::BitNot {
        dst: first,
        src: base,
    });
    block.push(MInst::BitNot {
        dst: value,
        src: first,
    });
    block.push(MInst::Store {
        base: BaseReg::StackFrame,
        offset: 0,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.push_block(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let mut plan = plan(&func, &cfg, &next_use, 3).unwrap();
    plan.point_ops.clear();
    plan.edge_ops.clear();
    plan.recipe_reloads.clear();
    plan.recipe_homes.clear();
    let logical = LogicalValue(value.0);
    let home = plan.homes.of_logical(logical);
    let point = ProgramPoint {
        block: BlockId(0),
        instruction: 3,
        side: PointSide::Before,
    };
    plan.point_ops.push((
        point,
        PlannedOp::Reload {
            value: logical,
            home,
        },
    ));
    let requested = super::super::ssa::planner_reload_queries(&func, &cfg, &plan).unwrap();
    let recipes = super::super::reload::analyze_with_queries(&func, &cfg, &requested).unwrap();

    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    assert!(
        plan.recipe_homes.is_empty(),
        "a three-instruction pure recipe must not replace a two-cost stack reload"
    );

    plan.point_ops.push((
        point,
        PlannedOp::Spill {
            value: logical,
            home,
        },
    ));
    plan.select_recipe_homes(&func, &cfg, &recipes).unwrap();
    assert_eq!(
        plan.recipe_homes,
        BTreeSet::from([home]),
        "avoiding the spill and reload makes the three-instruction recipe cheaper"
    );
    plan.verify_recipe_homes(&func, &cfg, &recipes).unwrap();
}

#[test]
fn stale_state_table_is_a_structured_error() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = super::super::next_use::analyze(&func, &cfg).unwrap();
    let mut plan = plan(&func, &cfg, &next_use, 1).unwrap();
    plan.w_entry.pop();

    let error = plan.verify(&func, &cfg, 1).unwrap_err();

    assert_eq!(error.rule, "SPILL_PLAN.STATE_SHAPE");
    assert_eq!(error.block, None);
}

#[test]
fn descending_large_phi_web_keeps_independent_homes_without_recursion() {
    const MEMBERS: u32 = 50_000;

    let mut vregs = VRegAllocator::new();
    for _ in 0..MEMBERS {
        vregs.alloc();
    }
    let mut func = MFunction::new(vregs, Vec::new());
    let mut block = MBlock::new(BlockId(0));
    for destination in (1..MEMBERS).rev() {
        block.phis.push(PhiNode {
            dst: VReg(destination),
            sources: vec![(BlockId(0), VReg(destination - 1))],
        });
    }
    func.blocks.push(block);

    let homes = SpillHomes::build(&func).unwrap();

    assert_eq!(homes.of_vreg(VReg(0)), SpillHome(0));
    assert_eq!(homes.of_vreg(VReg(MEMBERS - 1)), SpillHome(MEMBERS - 1));
    assert_eq!(homes.members(SpillHome(0)).count(), 1);
}

#[test]
fn large_phi_join_is_indexed_once_in_both_directions() {
    const PREDECESSORS: usize = 64;
    const PHIS: usize = 512;
    const INTERNAL_BLOCKS: usize = PREDECESSORS - 1;
    const TREE_BLOCKS: usize = PREDECESSORS * 2 - 1;
    let join_id = BlockId(TREE_BLOCKS as u32);
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let mut expected = Vec::with_capacity(PREDECESSORS * PHIS);
    let mut phis = Vec::with_capacity(PHIS);
    let mut leaf_definitions = (0..PREDECESSORS)
        .map(|_| Vec::with_capacity(PHIS))
        .collect::<Vec<_>>();
    for _ in 0..PHIS {
        let mut sources = Vec::with_capacity(PREDECESSORS);
        for (predecessor, definitions) in leaf_definitions.iter_mut().enumerate() {
            let source = vregs.alloc();
            let predecessor_id = BlockId((INTERNAL_BLOCKS + predecessor) as u32);
            sources.push((predecessor_id, source));
            definitions.push(source);
        }
        let destination = vregs.alloc();
        expected.extend(
            sources
                .iter()
                .map(|&(predecessor, source)| (predecessor, source, destination)),
        );
        phis.push(PhiNode {
            dst: destination,
            sources,
        });
    }

    let spill_descs = vec![SpillDesc::transient(); vregs.count() as usize];
    let mut func = MFunction::new(vregs, spill_descs);
    // A complete binary branch tree makes every one of the 64 eventual
    // join predecessors reachable from the single MIR entry block.
    for block_index in 0..INTERNAL_BLOCKS {
        let mut block = MBlock::new(BlockId(block_index as u32));
        if block_index == 0 {
            block.push(MInst::LoadImm {
                dst: condition,
                value: 1,
            });
        }
        block.push(MInst::Branch {
            cond: condition,
            true_bb: BlockId((block_index * 2 + 1) as u32),
            false_bb: BlockId((block_index * 2 + 2) as u32),
        });
        func.blocks.push(block);
    }
    for (predecessor, definitions) in leaf_definitions.iter().enumerate() {
        let predecessor_id = BlockId((INTERNAL_BLOCKS + predecessor) as u32);
        let mut block = MBlock::new(predecessor_id);
        for &source in definitions {
            block.push(MInst::LoadImm {
                dst: source,
                value: source.0 as u64,
            });
        }
        block.push(MInst::Jump { target: join_id });
        func.blocks.push(block);
    }
    let mut join = MBlock::new(join_id);
    join.phis = phis;
    join.push(MInst::Return);
    func.blocks.push(join);
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    func.verify();

    let logical = LogicalValues::build(&func);
    let translations = EdgeTranslations::build(&func, &cfg, &logical).unwrap();

    let join = cfg.block_index[&join_id];
    for (predecessor_id, source, destination) in expected {
        let predecessor = cfg.block_index[&predecessor_id];
        assert!(
            translations
                .to_successors(predecessor, join, LogicalValue(source.0))
                .any(|value| value == LogicalValue(destination.0))
        );
        assert_eq!(
            translations.to_predecessor(predecessor, join, LogicalValue(destination.0),),
            LogicalValue(source.0)
        );
    }
}

#[test]
fn irreducible_scc_entries_prioritize_values_used_in_the_region() {
    use crate::native::mir::{BaseReg, OpSize, SpillDesc};
    use crate::native::regalloc::next_use::{self, LoopRegionKind};

    let mut vregs = VRegAllocator::new();
    let hot = vregs.alloc();
    let live_through = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm { dst: hot, value: 1 });
    entry.push(MInst::LoadImm {
        dst: live_through,
        value: 2,
    });
    entry.push(MInst::Branch {
        cond: hot,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut left_entry = MBlock::new(BlockId(1));
    left_entry.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: hot,
        size: OpSize::S64,
    });
    left_entry.push(MInst::Branch {
        cond: hot,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });

    let mut right_entry = MBlock::new(BlockId(2));
    right_entry.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: hot,
        size: OpSize::S64,
    });
    right_entry.push(MInst::Branch {
        cond: hot,
        true_bb: BlockId(1),
        false_bb: BlockId(3),
    });

    let mut exit = MBlock::new(BlockId(3));
    exit.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 16,
        src: live_through,
        size: OpSize::S64,
    });
    exit.push(MInst::Return);
    func.blocks = vec![entry, left_entry, right_entry, exit];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let next_use = next_use::analyze(&func, &cfg).unwrap();
    let plan = plan(&func, &cfg, &next_use, 1).unwrap();
    let left = cfg.block_index[&BlockId(1)];
    let right = cfg.block_index[&BlockId(2)];
    let region = next_use.region_at_entry(left).unwrap();
    assert_eq!(next_use.region_at_entry(right), Some(region));
    assert_eq!(
        next_use.loop_regions[region].kind,
        LoopRegionKind::IrreducibleScc
    );
    for entry in [left, right] {
        assert_eq!(
            plan.w_entry[entry],
            LogicalSet::from_iter([LogicalValue(hot.0)])
        );
        assert!(!plan.w_entry[entry].contains(&LogicalValue(live_through.0)));
    }
}
