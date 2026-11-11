use std::collections::BTreeSet;

use super::*;
use crate::native::mir::{
    BaseReg, BlockId, MBlock, MInst, OpSize, PhiNode, SpillDesc, VRegAllocator,
};

/// The anticipated sets as the dense least fixpoint of their equation,
/// iterated over all blocks until nothing changes.
fn dense_anticipated(func: &MFunction, cfg: &NormalizedCfg) -> Vec<HashSet<VReg>> {
    let transfers = block_transfers(func);
    let phi_uses = phi_edge_uses(func, cfg).unwrap();
    let mut sets = vec![HashSet::<VReg>::default(); func.blocks.len()];
    loop {
        let mut changed = false;
        for block in 0..func.blocks.len() {
            let successors = &cfg.successors[block];
            let mut next = HashSet::<VReg>::default();
            if let Some(&first) = successors.first() {
                next = sets[first]
                    .iter()
                    .copied()
                    .filter(|value| !transfers[first].phi_definitions.contains(value))
                    .chain(phi_uses[block][0].iter().copied())
                    .collect();
                for (edge, &successor) in successors.iter().enumerate().skip(1) {
                    next.retain(|value| {
                        (sets[successor].contains(value)
                            && !transfers[successor].phi_definitions.contains(value))
                            || phi_uses[block][edge].contains(value)
                    });
                }
            }
            next.retain(|value| !transfers[block].instruction_definitions.contains(value));
            next.extend(transfers[block].after_phi_uses.iter().copied());
            if next != sets[block] {
                sets[block] = next;
                changed = true;
            }
        }
        if !changed {
            return sets;
        }
    }
}

/// A linear congruential generator for [`generated_function`].
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }
}

/// A generated function: branches, jumps forward and back, phis at
/// joins, and up to `max_instructions` stores and adds per block over a
/// small set of values.
fn generated_function(seed: u64, max_instructions: u64) -> MFunction {
    let mut rng = Lcg(seed);
    const VALUES: u32 = 48;
    let mut vregs = VRegAllocator::new();
    let values = (0..VALUES).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let pick = |rng: &mut Lcg| values[rng.next(u64::from(VALUES)) as usize];
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); VALUES as usize]);
    let count = 2 + rng.next(10) as usize;
    let mut defined = 8;
    let mut blocks = (0..count)
        .map(|index| MBlock::new(BlockId(index as u32)))
        .collect::<Vec<_>>();
    for value in &values[..8] {
        blocks[0].push(MInst::LoadImm {
            dst: *value,
            value: 1,
        });
    }
    let mut targets = vec![Vec::new(); count];
    for (index, block) in blocks.iter_mut().enumerate() {
        for _ in 0..rng.next(max_instructions + 1) {
            if rng.next(3) == 0 && defined < VALUES as usize {
                block.push(MInst::Add {
                    dst: values[defined],
                    lhs: pick(&mut rng),
                    rhs: pick(&mut rng),
                });
                defined += 1;
            } else {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: pick(&mut rng),
                    size: OpSize::S64,
                });
            }
        }
        // Every block falls through to the next, so all are reachable;
        // a branch adds an edge to any block but the entry.
        if index + 1 == count {
            block.push(MInst::Return);
            continue;
        }
        let following = BlockId(index as u32 + 1);
        let other = BlockId(1 + rng.next(count as u64 - 1) as u32);
        if rng.next(2) == 0 || other == following {
            targets[index].push(following);
            block.push(MInst::Jump { target: following });
        } else {
            targets[index].extend([following, other]);
            block.push(MInst::Branch {
                cond: pick(&mut rng),
                true_bb: following,
                false_bb: other,
            });
        }
    }
    for (join, block) in blocks.iter_mut().enumerate().skip(1) {
        let predecessors = (0..count)
            .filter(|&predecessor| targets[predecessor].contains(&BlockId(join as u32)))
            .collect::<Vec<_>>();
        if predecessors.len() < 2 {
            continue;
        }
        for _ in 0..rng.next(3) {
            if defined == VALUES as usize {
                break;
            }
            block.phis.push(PhiNode {
                dst: values[defined],
                sources: predecessors
                    .iter()
                    .map(|&block| (BlockId(block as u32), pick(&mut rng)))
                    .collect(),
            });
            defined += 1;
        }
    }
    func.blocks = blocks;
    func
}

#[test]
fn shared_anticipated_sets_match_the_dense_fixpoint() {
    for (seed, max_instructions) in (0..1500)
        .map(|seed| (seed, 4))
        .chain((0..300).map(|seed| (seed, 40)))
    {
        let mut func = generated_function(seed, max_instructions);
        let cfg = super::super::cfg::normalize(&mut func)
            .unwrap_or_else(|error| panic!("seed {seed}: {error:?}"));
        let analysis = analyze(&func, &cfg).unwrap();
        analysis.verify(&func, &cfg).unwrap();
        let expected = dense_anticipated(&func, &cfg);
        for (block, expected) in expected.iter().enumerate() {
            for value in (0..func.vregs.count()).map(VReg) {
                assert_eq!(
                    analysis.anticipated_at_entry(block, value),
                    expected.contains(&value),
                    "seed {seed}/{max_instructions}: block {block}, {value}"
                );
            }
        }
    }
}

#[test]
fn anticipatability_intersects_branch_paths_and_verifies_its_equation() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let one_arm = vregs.alloc();
    let after_join = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: one_arm,
        value: 2,
    });
    entry.push(MInst::LoadImm {
        dst: after_join,
        value: 3,
    });
    entry.push(MInst::Jump { target: BlockId(1) });

    let mut decision = MBlock::new(BlockId(1));
    decision.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(2),
        false_bb: BlockId(3),
    });

    let mut left = MBlock::new(BlockId(2));
    left.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: one_arm,
        size: OpSize::S64,
    });
    left.push(MInst::Jump { target: BlockId(4) });

    let mut right = MBlock::new(BlockId(3));
    right.push(MInst::Jump { target: BlockId(4) });

    let mut join = MBlock::new(BlockId(4));
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: after_join,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, decision, left, right, join];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let decision = cfg.block_index[&BlockId(1)];
    let analysis = analyze(&func, &cfg).unwrap();

    assert!(analysis.anticipated_at_entry(decision, condition));
    assert!(!analysis.anticipated_at_entry(decision, one_arm));
    assert!(analysis.anticipated_at_entry(decision, after_join));
    analysis.verify(&func, &cfg).unwrap();

    let mut stale = analysis;
    stale.anticipated_after_phis[decision].insert(one_arm);
    let error = stale.verify(&func, &cfg).unwrap_err();
    assert_eq!(error.rule, "NEXT_USE.ANTICIPATED_EQUATION");
    assert_eq!(error.block, Some(BlockId(1)));
    assert_eq!(error.values, vec![one_arm]);
}

#[test]
fn phi_destination_is_anticipated_after_the_phi_not_on_predecessor_edges() {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let left_value = vregs.alloc();
    let right_value = vregs.alloc();
    let merged = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

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
    left.push(MInst::LoadImm {
        dst: left_value,
        value: 2,
    });
    left.push(MInst::Jump { target: BlockId(3) });
    let mut right = MBlock::new(BlockId(2));
    right.push(MInst::LoadImm {
        dst: right_value,
        value: 3,
    });
    right.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(1), left_value), (BlockId(2), right_value)],
    });
    join.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: merged,
        size: OpSize::S64,
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, left, right, join];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let analysis = analyze(&func, &cfg).unwrap();
    let join = cfg.block_index[&BlockId(3)];

    assert!(analysis.anticipated_at_entry(join, merged));
    assert!(!analysis.anticipated_at_entry(join, left_value));
    assert!(!analysis.anticipated_at_entry(join, right_value));
    analysis.verify(&func, &cfg).unwrap();
}

#[test]
fn local_use_slice_contains_every_distinct_straight_line_use() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let copy = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    block.push(MInst::Add {
        dst: copy,
        lhs: value,
        rhs: value,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: copy,
        size: OpSize::S64,
    });
    block.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 8,
        src: value,
        size: OpSize::S64,
    });
    block.push(MInst::Return);
    func.blocks.push(block);

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let analysis = analyze(&func, &cfg).unwrap();

    assert_eq!(analysis.local_uses_from(0, 0, value), &[1, 3]);
    assert_eq!(analysis.local_uses_from(0, 2, value), &[3]);
    assert!(analysis.local_uses_from(0, 4, value).is_empty());
    analysis.verify(&func, &cfg).unwrap();
}

#[test]
fn missing_phi_edge_source_is_a_structured_analysis_error() {
    let mut vregs = VRegAllocator::new();
    let destination = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.phis.push(PhiNode {
        dst: destination,
        sources: Vec::new(),
    });
    successor.push(MInst::Return);
    func.blocks = vec![predecessor, successor];
    let cfg = NormalizedCfg {
        block_index: [(BlockId(0), 0), (BlockId(1), 1)].into_iter().collect(),
        predecessors: vec![vec![], vec![0]],
        successors: vec![vec![1], vec![]],
        idom: vec![None, Some(0)],
        dominator_children: vec![vec![1], vec![]],
        dominance_frontier: vec![BTreeSet::new(); 2],
        loops: Vec::new(),
        loop_for_header: HashMap::default(),
    };

    let error = analyze(&func, &cfg).unwrap_err();

    assert_eq!(error.rule, "NEXT_USE.PHI_EDGE_COVERAGE");
    assert_eq!(error.block, Some(BlockId(1)));
    assert_eq!(error.values, vec![destination]);
}

#[test]
fn stale_block_table_is_a_structured_error() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let mut analysis = analyze(&func, &cfg).unwrap();
    analysis.entry.pop();

    let error = analysis.verify(&func, &cfg).unwrap_err();

    assert_eq!(error.rule, "NEXT_USE.MODEL_SHAPE");
    assert_eq!(error.block, None);
}

#[test]
fn repeated_operand_has_one_instruction_position() {
    let mut vregs = VRegAllocator::new();
    let input = vregs.alloc();
    let output = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: input,
        value: 1,
    });
    block.push(MInst::Add {
        dst: output,
        lhs: input,
        rhs: input,
    });
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();

    let analysis = analyze(&func, &cfg).unwrap();

    analysis.verify(&func, &cfg).unwrap();
    assert_eq!(analysis.use_positions[0][&input], vec![1]);
}

#[test]
fn duplicate_operand_position_is_a_structured_verifier_error() {
    let mut vregs = VRegAllocator::new();
    let input = vregs.alloc();
    let output = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: input,
        value: 1,
    });
    block.push(MInst::Add {
        dst: output,
        lhs: input,
        rhs: input,
    });
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let mut analysis = analyze(&func, &cfg).unwrap();
    analysis.use_positions[0].get_mut(&input).unwrap().push(1);

    let error = analysis.verify(&func, &cfg).unwrap_err();

    assert_eq!(error.rule, "NEXT_USE.USE_POSITIONS_MATCH_MIR");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.instruction, Some(1));
    assert_eq!(error.values, vec![input]);
}

#[test]
fn stale_exit_value_is_a_structured_dataflow_error() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient()]);
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let mut analysis = analyze(&func, &cfg).unwrap();
    analysis.exit[0].insert(value, NextUseDistance::local(0));

    let error = analysis.verify(&func, &cfg).unwrap_err();

    assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.values, vec![value]);
}

#[test]
fn stale_entry_distance_is_a_structured_dataflow_error() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let used = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.push(MInst::Mov {
        dst: used,
        src: value,
    });
    successor.push(MInst::Return);
    func.blocks = vec![entry, successor];
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let predecessor = cfg.block_index[&BlockId(0)];
    let successor = cfg.block_index[&BlockId(1)];
    let mut analysis = analyze(&func, &cfg).unwrap();
    analysis.entry[successor].insert(value, NextUseDistance::local(1));
    analysis.exit[predecessor].insert(value, NextUseDistance::local(1));

    let error = analysis.verify(&func, &cfg).unwrap_err();

    assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
    assert_eq!(error.block, Some(BlockId(1)));
    assert_eq!(error.instruction, Some(0));
    assert_eq!(error.values, vec![value]);
}

#[test]
fn missing_phi_edge_distance_is_a_structured_dataflow_error() {
    let mut vregs = VRegAllocator::new();
    let source = vregs.alloc();
    let destination = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 2]);
    let mut predecessor = MBlock::new(BlockId(0));
    predecessor.push(MInst::LoadImm {
        dst: source,
        value: 1,
    });
    predecessor.push(MInst::Jump { target: BlockId(1) });
    let mut successor = MBlock::new(BlockId(1));
    successor.phis.push(PhiNode {
        dst: destination,
        sources: vec![(BlockId(0), source)],
    });
    successor.push(MInst::Return);
    func.blocks = vec![predecessor, successor];
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let predecessor = cfg.block_index[&BlockId(0)];
    let mut analysis = analyze(&func, &cfg).unwrap();
    analysis.exit[predecessor].remove(&source);

    let error = analysis.verify(&func, &cfg).unwrap_err();

    assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
    assert_eq!(error.block, Some(BlockId(0)));
    assert_eq!(error.values, vec![source]);
}

#[test]
fn irreducible_cyclic_scc_exit_is_a_loop_component() {
    // Blocks 1 and 2 form a cyclic SCC with two entries from block 0, so
    // neither is a natural-loop header.  Exiting the SCC must nevertheless
    // increment the lexicographic loop component.
    let cfg = NormalizedCfg {
        block_index: HashMap::default(),
        predecessors: vec![vec![], vec![0, 2], vec![0, 1], vec![1, 2]],
        successors: vec![vec![1, 2], vec![2, 3], vec![1, 3], vec![]],
        idom: vec![None, Some(0), Some(0), Some(0)],
        dominator_children: vec![vec![1, 2, 3], vec![], vec![], vec![]],
        dominance_frontier: vec![BTreeSet::new(); 4],
        loops: Vec::new(),
        loop_for_header: HashMap::default(),
    };
    let topology = RegionTopology::build(&cfg).unwrap();
    assert_eq!(topology.edge_exits[1], vec![0, 1]);
    assert_eq!(topology.edge_exits[2], vec![0, 1]);
    assert_eq!(topology.regions.len(), 1);
    assert_eq!(topology.regions[0].kind, LoopRegionKind::IrreducibleScc);
    assert_eq!(topology.regions[0].entries, vec![1, 2]);
    assert_eq!(topology.entry_region[1], Some(0));
    assert_eq!(topology.entry_region[2], Some(0));
}

#[test]
fn nested_region_facts_scan_blocks_once_and_aggregate_bottom_up() {
    let mut vregs = VRegAllocator::new();
    let outer_value = vregs.alloc();
    let inner_value = vregs.alloc();
    let outer_condition = vregs.alloc();
    let inner_condition = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: outer_value,
        value: 10,
    });
    entry.push(MInst::LoadImm {
        dst: inner_value,
        value: 20,
    });
    entry.push(MInst::LoadImm {
        dst: outer_condition,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: inner_condition,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });

    let mut outer_header = MBlock::new(BlockId(1));
    outer_header.push(MInst::Store {
        base: crate::native::mir::BaseReg::SimState,
        offset: 0,
        src: outer_value,
        size: crate::native::mir::OpSize::S64,
    });
    outer_header.push(MInst::Branch {
        cond: outer_condition,
        true_bb: BlockId(2),
        false_bb: BlockId(5),
    });

    let mut inner_header = MBlock::new(BlockId(2));
    inner_header.push(MInst::Branch {
        cond: inner_condition,
        true_bb: BlockId(3),
        false_bb: BlockId(4),
    });
    let mut inner_body = MBlock::new(BlockId(3));
    inner_body.push(MInst::Store {
        base: crate::native::mir::BaseReg::SimState,
        offset: 8,
        src: inner_value,
        size: crate::native::mir::OpSize::S64,
    });
    inner_body.push(MInst::Jump { target: BlockId(2) });
    let mut outer_latch = MBlock::new(BlockId(4));
    outer_latch.push(MInst::Jump { target: BlockId(1) });
    let mut exit = MBlock::new(BlockId(5));
    exit.push(MInst::Return);
    func.blocks = vec![
        entry,
        outer_header,
        inner_header,
        inner_body,
        outer_latch,
        exit,
    ];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let analysis = analyze(&func, &cfg).unwrap();
    analysis.verify(&func, &cfg).unwrap();
    let outer_header = cfg.block_index[&BlockId(1)];
    let inner_header = cfg.block_index[&BlockId(2)];
    let outer_region = analysis.region_at_entry(outer_header).unwrap();
    let inner_region = analysis.region_at_entry(inner_header).unwrap();
    assert_ne!(outer_region, inner_region);
    let outer = &analysis.loop_regions[outer_region];
    let inner = &analysis.loop_regions[inner_region];
    assert_eq!(outer.kind, LoopRegionKind::Natural);
    assert_eq!(inner.kind, LoopRegionKind::Natural);
    assert!(analysis.used_in_region(inner_region, inner_value));
    assert!(!analysis.used_in_region(inner_region, outer_value));
    assert!(analysis.used_in_region(outer_region, inner_value));
    assert!(analysis.used_in_region(outer_region, outer_value));
    for (header, facts) in [(outer_header, outer), (inner_header, inner)] {
        let natural_loop = cfg
            .loops
            .iter()
            .find(|natural_loop| natural_loop.header == header)
            .unwrap();
        assert_eq!(
            facts.max_pressure,
            natural_loop
                .blocks
                .iter()
                .map(|&block| analysis.block_max_pressure[block])
                .max()
                .unwrap()
        );
    }
    assert_eq!(analysis.region_work.block_scans, func.blocks.len());
    assert_eq!(
        analysis.region_work.instruction_visits,
        func.blocks
            .iter()
            .map(|block| block.insts.len())
            .sum::<usize>()
    );
    assert_eq!(analysis.region_work.indexed_block_values, 4);
    assert_eq!(analysis.region_work.unique_use_positions, 4);
    assert_eq!(analysis.region_work.pressure_propagations, 1);
}

#[test]
fn deep_nesting_does_not_copy_value_membership_to_ancestors() {
    const DEPTH: usize = 20_000;
    const VALUES: usize = 20_000;

    let regions = (0..DEPTH)
        .map(|region| RegionShape {
            kind: LoopRegionKind::Natural,
            parent: (region + 1 < DEPTH).then_some(region + 1),
            entries: Vec::new(),
        })
        .collect::<Vec<_>>();
    let topology = RegionTopology {
        regions,
        entry_region: vec![Some(0)],
        direct_region: vec![Some(0)],
        edge_exits: vec![Vec::new()],
    };
    let blocks = vec![BlockRegionSummary {
        used: (0..VALUES as u32).map(VReg).collect(),
        max_pressure: VALUES,
    }];
    let mut work = RegionAggregationWork::default();
    let uses = RegionUseIndex::build(VALUES, &topology, &blocks, &mut work).unwrap();
    let pressure = aggregate_region_pressure(&topology, &blocks, &mut work).unwrap();

    // One direct occurrence per value is sufficient for every ancestor
    // query.  A complete-set representation would require DEPTH * VALUES
    // memberships (400 million for this fixture).
    assert_eq!(uses.positions.len(), VALUES);
    assert_eq!(work.indexed_block_values, VALUES);
    assert_eq!(work.unique_use_positions, VALUES);
    assert_eq!(work.pressure_propagations, DEPTH - 1);
    assert!(pressure.iter().all(|maximum| *maximum == VALUES));
    for region in [0, DEPTH / 2, DEPTH - 1] {
        for value in [VReg(0), VReg(VALUES as u32 - 1)] {
            assert!(uses.contains(value, uses.region_start[region], uses.region_end[region]));
        }
    }
    assert!(!uses.contains(
        VReg(VALUES as u32),
        uses.region_start[DEPTH - 1],
        uses.region_end[DEPTH - 1]
    ));
}

#[test]
fn loop_exit_use_is_farther_than_loop_use() {
    let mut vregs = VRegAllocator::new();
    let value = vregs.alloc();
    let condition = vregs.alloc();
    let inside = vregs.alloc();
    let outside = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);
    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });
    let mut header = MBlock::new(BlockId(1));
    header.push(MInst::Add {
        dst: inside,
        lhs: condition,
        rhs: condition,
    });
    header.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });
    let mut exit = MBlock::new(BlockId(2));
    exit.push(MInst::Add {
        dst: outside,
        lhs: value,
        rhs: value,
    });
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, exit];
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let mut analysis = analyze(&func, &cfg).unwrap();
    let header = cfg.block_index[&BlockId(1)];
    analysis.verify(&func, &cfg).unwrap();
    assert!(matches!(
        analysis.exit[header].get(&value).unwrap(),
        NextUseDistance::Finite {
            loop_exits: 1..,
            ..
        }
    ));

    let NextUseDistance::Finite { instructions, .. } = analysis.exit[header].get(&value).unwrap()
    else {
        unreachable!("loop-exit value must have a finite distance")
    };
    analysis.exit[header].insert(
        value,
        NextUseDistance::Finite {
            loop_exits: 0,
            instructions,
        },
    );
    let error = analysis.verify(&func, &cfg).unwrap_err();
    assert_eq!(error.rule, "NEXT_USE.DATAFLOW_EQUATION");
    assert_eq!(error.block, Some(BlockId(1)));
    assert_eq!(error.values, vec![value]);
}

/// A loop around a chain of `diamonds` branches. The short arm of
/// diamond `index` is one block of 16 stores, the long arm `index % 7 + 1`
/// blocks with one store, so the arm with more blocks carries the nearer
/// next uses around the loop.
fn diamond_loop(diamonds: u32) -> MFunction {
    let mut vregs = VRegAllocator::new();
    let condition = vregs.alloc();
    let values = (0..2 * diamonds).map(|_| vregs.alloc()).collect::<Vec<_>>();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); values.len() + 1]);
    let store = |block: &mut MBlock, src| {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src,
            size: OpSize::S64,
        });
    };
    let mut next_id = 0;
    let mut fresh = || {
        next_id += 1;
        BlockId(next_id - 1)
    };
    let mut entry = MBlock::new(fresh());
    for &dst in std::iter::once(&condition).chain(&values) {
        entry.push(MInst::LoadImm { dst, value: 1 });
    }
    let header = fresh();
    entry.push(MInst::Jump { target: header });
    let mut blocks = vec![entry];
    let mut current = MBlock::new(header);
    for (index, pair) in values.chunks(2).enumerate() {
        let short = fresh();
        let long = (0..index % 7 + 1).map(|_| fresh()).collect::<Vec<_>>();
        let join = fresh();
        current.push(MInst::Branch {
            cond: condition,
            true_bb: short,
            false_bb: long[0],
        });
        blocks.push(current);
        let mut short_block = MBlock::new(short);
        for _ in 0..16 {
            store(&mut short_block, pair[0]);
        }
        short_block.push(MInst::Jump { target: join });
        blocks.push(short_block);
        for (position, &id) in long.iter().enumerate() {
            let mut block = MBlock::new(id);
            let target = long.get(position + 1).copied().unwrap_or_else(|| {
                store(&mut block, pair[1]);
                join
            });
            block.push(MInst::Jump { target });
            blocks.push(block);
        }
        current = MBlock::new(join);
    }
    let exit = fresh();
    current.push(MInst::Branch {
        cond: condition,
        true_bb: header,
        false_bb: exit,
    });
    blocks.push(current);
    let mut exit_block = MBlock::new(exit);
    exit_block.push(MInst::Return);
    blocks.push(exit_block);
    func.blocks = blocks;
    func
}

/// Next-use distances settle in a bounded number of visits per block.
/// A first-in first-out worklist visited each block once for every
/// distinct number of blocks over which a nearer use reached it around
/// the loop: 76 visits per block with 256 diamonds, quadratic in the
/// loop's size.
#[test]
fn loop_distances_settle_in_a_bounded_number_of_visits() {
    for diamonds in [8, 64, 256] {
        let mut func = diamond_loop(diamonds);
        let cfg = super::super::cfg::normalize(&mut func).unwrap();
        let analysis = analyze(&func, &cfg).unwrap();
        analysis.verify(&func, &cfg).unwrap();
        assert!(
            analysis.block_visits <= 3 * func.blocks.len(),
            "{diamonds} diamonds: {} visits of {} blocks",
            analysis.block_visits,
            func.blocks.len()
        );
    }
}

#[test]
fn next_iteration_use_beats_exit_use_after_more_than_100k_instructions() {
    const LOOP_BODY: usize = 100_001;

    let mut vregs = VRegAllocator::new();
    let inside_value = vregs.alloc();
    let exit_value = vregs.alloc();
    let condition = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

    let mut entry = MBlock::new(BlockId(0));
    entry.push(MInst::LoadImm {
        dst: inside_value,
        value: 11,
    });
    entry.push(MInst::LoadImm {
        dst: exit_value,
        value: 22,
    });
    entry.push(MInst::LoadImm {
        dst: condition,
        value: 1,
    });
    entry.push(MInst::Jump { target: BlockId(1) });

    let mut header = MBlock::new(BlockId(1));
    header.insts.extend((0..LOOP_BODY).map(|_| MInst::MemCopy {
        src_offset: 0,
        dst_offset: 8,
        byte_len: 8,
    }));
    header.push(MInst::Store {
        base: crate::native::mir::BaseReg::SimState,
        offset: 0,
        src: inside_value,
        size: crate::native::mir::OpSize::S64,
    });
    header.push(MInst::Branch {
        cond: condition,
        true_bb: BlockId(1),
        false_bb: BlockId(2),
    });

    let mut exit = MBlock::new(BlockId(2));
    exit.push(MInst::Store {
        base: crate::native::mir::BaseReg::SimState,
        offset: 8,
        src: exit_value,
        size: crate::native::mir::OpSize::S64,
    });
    exit.push(MInst::Return);
    func.blocks = vec![entry, header, exit];

    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let analysis = analyze(&func, &cfg).unwrap();
    let header = cfg.block_index[&BlockId(1)];
    // Query immediately after this iteration's inside-loop use.  Its next
    // occurrence is in the next iteration, beyond the 100,001-instruction
    // body; the other value is used immediately after leaving the loop.
    let point_after_inside_use = LOOP_BODY + 1;
    let inside = analysis.distance_at(&func, header, point_after_inside_use, inside_value);
    let outside = analysis.distance_at(&func, header, point_after_inside_use, exit_value);

    assert!(
        matches!(
            inside,
            NextUseDistance::Finite {
                loop_exits: 0,
                instructions
            } if instructions > 100_000
        ),
        "inside-loop distance was {inside:?}"
    );
    assert!(
        matches!(
            outside,
            NextUseDistance::Finite {
                loop_exits: 1..,
                ..
            }
        ),
        "loop-exit distance was {outside:?}"
    );
    assert!(
        inside < outside,
        "lexicographic distance must prefer the next loop iteration: inside={inside:?}, outside={outside:?}"
    );
}
