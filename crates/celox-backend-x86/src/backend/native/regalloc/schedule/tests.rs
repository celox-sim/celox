use super::*;
use crate::native::mir::{
    BaseReg, BlockId, MBlock, MemoryAliasRange, OpSize, SpillDesc, VRegAllocator,
};

#[test]
#[cfg(target_arch = "x86_64")]
fn vector_lifetimes_stay_on_their_side_of_perm_boundaries() {
    use crate::native::mir::{X86SimdInst, X86VecReg};
    for boundary in [
        MInst::SDiv {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::Shl {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
    ] {
        let region = vec![
            MInst::X86Simd(X86SimdInst::Zero128 { dst: X86VecReg(0) }),
            MInst::X86Simd(X86SimdInst::Store128 {
                base: BaseReg::SimState,
                offset: 0,
                src: X86VecReg(0),
            }),
            boundary,
            MInst::X86Simd(X86SimdInst::Zero128 { dst: X86VecReg(1) }),
            MInst::X86Simd(X86SimdInst::Store128 {
                base: BaseReg::SimState,
                offset: 16,
                src: X86VecReg(1),
            }),
        ];
        let dag = InstructionDag::build(&region).unwrap();
        assert!(dependency_order_valid(&dag.dependencies, &[0, 1, 2, 3, 4]));
        assert!(!dependency_order_valid(&dag.dependencies, &[0, 2, 1, 3, 4]));
        assert!(!dependency_order_valid(&dag.dependencies, &[0, 1, 3, 2, 4]));
    }
}

#[test]
fn local_pressure_counts_match_materialized_oracle() {
    for seed in 0..32u32 {
        let instructions = (0..128u32)
            .map(|index| MInst::AddImm {
                dst: VReg((index * 13 + seed) % 192),
                src: VReg((index * 31 + seed * 7) % 192),
                imm: 1,
            })
            .collect::<Vec<_>>();
        let live_out = (0..8192)
            .filter(|value| (value + seed) % 3 != 0)
            .map(VReg)
            .collect::<BTreeSet<_>>();
        for reverse in [false, true] {
            let mut order = (0..instructions.len()).collect::<Vec<_>>();
            if reverse {
                order.reverse();
            }
            let ordered = order
                .iter()
                .map(|&index| instructions[index].clone())
                .collect::<Vec<_>>();
            for capacity in [0, 16, 8192] {
                let expected = pressure_cost(&ordered, &live_out, capacity);
                assert_eq!(
                    pressure_cost_for_order(&instructions, &order, &live_out, capacity),
                    (expected.excess_area, expected.maximum)
                );
            }
        }
    }
}

#[test]
fn pressure_order_finishes_one_recurrence_before_starting_another() {
    let first_seed = VReg(0);
    let second_seed = VReg(1);
    let first_middle = VReg(2);
    let second_middle = VReg(3);
    let first_result = VReg(4);
    let second_result = VReg(5);
    let instructions = vec![
        MInst::LoadImm {
            dst: first_seed,
            value: 0,
        },
        MInst::LoadImm {
            dst: second_seed,
            value: 0,
        },
        MInst::AddImm {
            dst: first_middle,
            src: first_seed,
            imm: 1,
        },
        MInst::AddImm {
            dst: second_middle,
            src: second_seed,
            imm: 1,
        },
        MInst::AddImm {
            dst: first_result,
            src: first_middle,
            imm: 1,
        },
        MInst::AddImm {
            dst: second_result,
            src: second_middle,
            imm: 1,
        },
    ];
    let constraints = vec![Default::default(); instructions.len()];

    let order = pressure_preferred_block_order(
        &instructions,
        &constraints,
        [first_result, second_result].into_iter(),
        16,
    )
    .unwrap();

    assert!(
        order == [0, 2, 4, 1, 3, 5] || order == [1, 3, 5, 0, 2, 4],
        "independent recurrence lanes must not be interleaved: {order:?}"
    );
}

#[test]
fn forward_ready_region_discloses_only_dependency_ready_instructions() {
    let left = VReg(0);
    let right = VReg(1);
    let sum = VReg(2);
    let masked = VReg(3);
    let region = vec![
        MInst::LoadImm {
            dst: left,
            value: 1,
        },
        MInst::LoadImm {
            dst: right,
            value: 2,
        },
        MInst::Add {
            dst: sum,
            lhs: left,
            rhs: right,
        },
        MInst::AndImm {
            dst: masked,
            src: sum,
            imm: 3,
        },
    ];
    let mut ready = ForwardReadyRegion::build(&region).unwrap();

    assert_eq!(ready.ready(), &BTreeSet::from([0, 1]));
    assert!(!ready.is_ready(2));
    assert_eq!(ready.remaining_uses(left), 1);
    assert_eq!(ready.emit(2), None);
    assert_eq!(ready.ready(), &BTreeSet::from([0, 1]));

    assert_eq!(ready.emit(1), Some(Vec::new()));
    assert_eq!(ready.emit(0), Some(vec![2]));
    assert_eq!(ready.remaining_uses(left), 1);
    assert_eq!(ready.ready(), &BTreeSet::from([2]));
    assert_eq!(ready.emit(2), Some(vec![3]));
    assert_eq!(ready.remaining_uses(left), 0);
    assert_eq!(ready.emit(3), Some(Vec::new()));
    assert!(ready.is_complete());
    assert_eq!(ready.discharged_edges(), ready.edge_count());
}

#[test]
fn forward_ready_region_processes_each_sparse_edge_once() {
    const LANES: usize = 4096;
    let mut region = Vec::with_capacity(LANES * 2);
    for lane in 0..LANES {
        let input = VReg((lane * 2) as u32);
        let output = VReg((lane * 2 + 1) as u32);
        region.push(MInst::LoadImm {
            dst: input,
            value: lane as u64,
        });
        region.push(MInst::AndImm {
            dst: output,
            src: input,
            imm: 3,
        });
    }
    let mut ready = ForwardReadyRegion::build(&region).unwrap();
    assert_eq!(ready.ready().len(), LANES);

    for lane in 0..LANES {
        let producer = lane * 2;
        let consumer = producer + 1;
        assert_eq!(ready.emit(producer), Some(vec![consumer]));
        assert_eq!(ready.emit(consumer), Some(Vec::new()));
    }

    assert!(ready.is_complete());
    assert_eq!(ready.discharged_edges(), LANES);
    assert_eq!(ready.discharged_edges(), ready.edge_count());
}

#[test]
fn pressure_gate_rejects_turning_lane_local_chains_into_a_wavefront() {
    const LANES: usize = 8;
    let mut instructions = Vec::with_capacity(LANES * 2);
    for lane in 0..LANES {
        let input = VReg((lane * 2) as u32);
        let result = VReg((lane * 2 + 1) as u32);
        instructions.push(MInst::LoadImm {
            dst: input,
            value: lane as u64,
        });
        instructions.push(MInst::AndImm {
            dst: result,
            src: input,
            imm: 1,
        });
    }
    let wavefront = (0..LANES)
        .map(|lane| lane * 2)
        .chain((0..LANES).map(|lane| lane * 2 + 1))
        .collect::<Vec<_>>();

    assert!(!preserves_original_pressure(
        &instructions,
        &wavefront,
        &BTreeSet::new(),
        4,
    ));
}

#[test]
fn forward_ready_region_does_not_invent_read_after_read_edges() {
    let source = VReg(0);
    let first = VReg(1);
    let second = VReg(2);
    let region = vec![
        MInst::LoadImm {
            dst: source,
            value: 3,
        },
        MInst::AndImm {
            dst: first,
            src: source,
            imm: 1,
        },
        MInst::OrImm {
            dst: second,
            src: source,
            imm: 4,
        },
    ];
    let mut ready = ForwardReadyRegion::build(&region).unwrap();

    assert_eq!(ready.emit(0), Some(vec![1, 2]));
    assert_eq!(ready.ready(), &BTreeSet::from([1, 2]));
    assert_eq!(ready.remaining_uses(source), 2);
    assert_eq!(ready.emit(2), Some(Vec::new()));
    assert_eq!(ready.remaining_uses(source), 1);
    assert_eq!(ready.emit(1), Some(Vec::new()));
    assert!(ready.is_complete());
}

#[test]
fn indexed_buckets_do_not_scan_a_long_ready_set() {
    const INSTRUCTIONS: usize = 4096;
    let region = (0..INSTRUCTIONS)
        .map(|index| MInst::LoadImm {
            dst: VReg(index as u32),
            value: index as u64,
        })
        .collect::<Vec<_>>();

    let scheduled = schedule_region(&region, BTreeSet::new());

    assert!(scheduled.dependency_verified);
    assert_eq!(scheduled.instructions, region);
    assert_eq!(scheduled.work.ready_insertions, INSTRUCTIONS);
    assert_eq!(scheduled.work.ready_pops, INSTRUCTIONS);
    assert_eq!(scheduled.work.priority_computations, INSTRUCTIONS);
    assert_eq!(scheduled.work.priority_updates, 0);
    assert_eq!(scheduled.work.priority_value_index_visits, 0);
    // Every pop probes only the fixed number of score buckets. The former
    // min-by-key implementation inspected a shrinking ready set and did
    // INSTRUCTIONS * (INSTRUCTIONS + 1) / 2 candidate evaluations here.
    assert!(
        scheduled.work.priority_bucket_probes <= INSTRUCTIONS * PRIORITY_BUCKETS,
        "bucket probes must be O(region length)"
    );
}

#[test]
fn memory_dependent_block_uses_one_backward_liveness_pass() {
    const REGIONS: usize = 512;
    let mut vregs = VRegAllocator::new();
    let mut instructions = Vec::with_capacity(REGIONS * 2 + 1);
    for value in 0..REGIONS {
        let register = vregs.alloc();
        instructions.push(MInst::LoadImm {
            dst: register,
            value: value as u64,
        });
        instructions.push(MInst::Store {
            base: BaseReg::StackFrame,
            offset: 0,
            src: register,
            size: OpSize::S64,
        });
    }
    instructions.push(MInst::Return);
    let instruction_count = instructions.len();
    let mut block = MBlock::new(BlockId(0));
    block.insts = instructions;
    let mut func = MFunction::new(
        vregs,
        (0..REGIONS).map(|_| SpillDesc::transient()).collect(),
    );
    func.blocks.push(block);
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let constraints = ConstraintModel::build(&func, &cfg).unwrap();
    let analysis = super::super::analysis::analyze(&func);

    let stats = schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap();

    assert_eq!(stats.regions_considered, 1);
    assert_eq!(stats.backward_liveness_steps, instruction_count);
    assert!(stats.maximum_after <= stats.maximum_before);
    func.verify();
}

#[test]
fn sparse_pseudo_effects_feed_shared_dependence_analysis() {
    const MARKS: usize = 256;
    const ACTIVE_CAPACITY: usize = 1_000_000;
    let mut tracker = MemoryDependencyTracker::<MemoryObject, usize>::default();
    for index in 0..MARKS {
        let inst = MInst::SparseMarkActive {
            active_index: index as u32,
            active_bits_offset: 1024,
            active_capacity: ACTIVE_CAPACITY,
        };
        let mut dependencies = BTreeSet::new();
        let reads = memory_effect::reads(&inst);
        let writes = memory_effect::writes(&inst);
        tracker.add_event(
            index,
            analysis_effects(&reads),
            analysis_effects(&writes),
            &mut dependencies,
        );
        if index % 64 != 0 {
            assert!(dependencies.contains(&(index - 1)));
        } else if index != 0 {
            assert!(!dependencies.contains(&(index - 1)));
        }
    }

    let mark = MInst::SparseMarkActive {
        active_index: MARKS as u32,
        active_bits_offset: 1024,
        active_capacity: ACTIVE_CAPACITY,
    };
    assert!(
        is_pressure_schedulable_kind(&mark),
        "exact sparse metadata effects must be represented by DAG edges, not a region barrier"
    );
}

#[test]
fn disjoint_memory_chains_are_scheduled_near_their_uses() {
    let first = VReg(0);
    let second = VReg(1);
    let first_result = VReg(2);
    let second_result = VReg(3);
    let region = vec![
        MInst::Load {
            dst: first,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: second,
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        },
        MInst::AddImm {
            dst: first_result,
            src: first,
            imm: 1,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: first_result,
            size: OpSize::S64,
        },
        MInst::AddImm {
            dst: second_result,
            src: second,
            imm: 1,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: second_result,
            size: OpSize::S64,
        },
    ];

    let scheduled = schedule_region(&region, BTreeSet::new());

    assert!(scheduled.dependency_verified);
    assert!(
        max_pressure(&scheduled.instructions, &BTreeSet::new())
            < max_pressure(&region, &BTreeSet::new())
    );
    for chain in [
        [region[0].clone(), region[2].clone(), region[3].clone()],
        [region[1].clone(), region[4].clone(), region[5].clone()],
    ] {
        let positions = chain.map(|instruction| {
            scheduled
                .instructions
                .iter()
                .position(|candidate| candidate == &instruction)
                .unwrap()
        });
        assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    }
}

#[test]
fn w32_operations_do_not_split_a_pressure_scheduling_region() {
    let mut vregs = VRegAllocator::new();
    let first = vregs.alloc();
    let second = vregs.alloc();
    let first_masked = vregs.alloc();
    let second_masked = vregs.alloc();
    let first_result = vregs.alloc();
    let second_result = vregs.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: first,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S32,
        },
        MInst::Load {
            dst: second,
            base: BaseReg::SimState,
            offset: 4,
            size: OpSize::S32,
        },
        MInst::AndImm32 {
            dst: first_masked,
            src: first,
            imm: 0xff,
        },
        MInst::AndImm32 {
            dst: second_masked,
            src: second,
            imm: 0xff,
        },
        MInst::Mov32 {
            dst: first_result,
            src: first_masked,
        },
        MInst::Mov32 {
            dst: second_result,
            src: second_masked,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: first_result,
            size: OpSize::S32,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 12,
            src: second_result,
            size: OpSize::S32,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(vregs, (0..6).map(|_| SpillDesc::transient()).collect());
    func.blocks.push(block);
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let constraints = ConstraintModel::build(&func, &cfg).unwrap();
    let analysis = super::super::analysis::analyze(&func);

    let stats = schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap();

    assert_eq!(stats.regions_considered, 1);
    assert!(stats.maximum_after < stats.maximum_before);
    func.verify();
}

#[test]
fn dependency_spine_stays_behind_its_independent_w32_input() {
    let mut vregs = VRegAllocator::new();
    let first = vregs.alloc();
    let second = vregs.alloc();
    let side_input = vregs.alloc();
    let first_reduction = vregs.alloc();
    let result = vregs.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: first,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S32,
        },
        MInst::Load {
            dst: second,
            base: BaseReg::SimState,
            offset: 4,
            size: OpSize::S32,
        },
        MInst::Load {
            dst: side_input,
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S32,
        },
        MInst::And32 {
            dst: first_reduction,
            lhs: first,
            rhs: second,
        },
        MInst::And32 {
            dst: result,
            lhs: first_reduction,
            rhs: side_input,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 12,
            src: result,
            size: OpSize::S32,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(vregs, (0..5).map(|_| SpillDesc::transient()).collect());
    func.blocks.push(block);
    func.verify();
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let constraints = ConstraintModel::build(&func, &cfg).unwrap();
    let analysis = super::super::analysis::analyze(&func);

    schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap();

    let instructions = &func.blocks[0].insts;
    let side_input_position = instructions
        .iter()
        .position(|inst| inst.def() == Some(side_input))
        .unwrap();
    let first_reduction_position = instructions
        .iter()
        .position(|inst| inst.def() == Some(first_reduction))
        .unwrap();
    let result_position = instructions
        .iter()
        .position(|inst| inst.def() == Some(result))
        .unwrap();
    assert!(side_input_position < first_reduction_position);
    assert!(first_reduction_position < result_position);
    func.verify();
}

#[test]
fn ready_queue_switches_to_pressure_at_the_register_capacity() {
    let priorities = vec![(8, 8), (1, 1)];
    let mut below_capacity = IndexedReadyQueue::new(priorities.clone());
    let mut work = RegionWork::default();
    below_capacity.insert(0, 2, &mut work);
    below_capacity.insert(1, -1, &mut work);
    assert_eq!(below_capacity.pop_best(10, 14, &mut work), Some(0));

    let mut at_capacity = IndexedReadyQueue::new(priorities);
    at_capacity.insert(0, 2, &mut work);
    at_capacity.insert(1, -1, &mut work);
    assert_eq!(at_capacity.pop_best(14, 14, &mut work), Some(1));
}

#[test]
fn excess_live_area_distinguishes_equal_peak_schedules() {
    let prefix = vec![
        MInst::LoadImm {
            dst: VReg(10),
            value: 1,
        },
        MInst::LoadImm {
            dst: VReg(11),
            value: 2,
        },
        MInst::LoadImm {
            dst: VReg(12),
            value: 3,
        },
        MInst::Select {
            dst: VReg(13),
            cond: VReg(10),
            true_val: VReg(11),
            false_val: VReg(12),
        },
    ];
    let mut compact = prefix.clone();
    compact.extend([
        MInst::LoadImm {
            dst: VReg(0),
            value: 4,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 5,
        },
        MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::LoadImm {
            dst: VReg(3),
            value: 6,
        },
    ]);
    let mut stretched = prefix;
    stretched.extend([
        MInst::LoadImm {
            dst: VReg(0),
            value: 4,
        },
        MInst::LoadImm {
            dst: VReg(3),
            value: 6,
        },
        MInst::LoadImm {
            dst: VReg(1),
            value: 5,
        },
        MInst::Add {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
    ]);
    let live_out = BTreeSet::from([VReg(2), VReg(3)]);

    let compact_cost = pressure_cost(&compact, &live_out, 1);
    let stretched_cost = pressure_cost(&stretched, &live_out, 1);

    assert_eq!(compact_cost.maximum, stretched_cost.maximum);
    assert!(compact_cost.excess_area < stretched_cost.excess_area);
}

#[test]
fn overlapping_memory_access_order_is_preserved() {
    let before = VReg(0);
    let stored = VReg(1);
    let after = VReg(2);
    let region = vec![
        MInst::Load {
            dst: before,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: stored,
            value: 7,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 4,
            src: stored,
            size: OpSize::S32,
        },
        MInst::Load {
            dst: after,
            base: BaseReg::SimState,
            offset: 7,
            size: OpSize::S8,
        },
    ];

    let scheduled = schedule_region(&region, BTreeSet::from([before, after]));
    let positions = [0, 2, 3].map(|original| {
        scheduled
            .instructions
            .iter()
            .position(|candidate| candidate == &region[original])
            .unwrap()
    });

    assert!(scheduled.dependency_verified);
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
}

#[test]
fn memfill_is_scheduled_by_its_exact_memory_dependencies() {
    let before = VReg(0);
    let after = VReg(1);
    let region = vec![
        MInst::Load {
            dst: before,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::MemFill {
            dst_offset: 36,
            byte_len: 8,
            value: 0,
        },
        MInst::Load {
            dst: after,
            base: BaseReg::SimState,
            offset: 43,
            size: OpSize::S8,
        },
    ];

    assert!(is_pressure_schedulable_kind(&region[1]));
    let scheduled = schedule_region(&region, BTreeSet::from([before, after]));
    let positions = [0, 1, 2].map(|original| {
        scheduled
            .instructions
            .iter()
            .position(|candidate| candidate == &region[original])
            .unwrap()
    });

    assert!(scheduled.dependency_verified);
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
}

#[test]
fn overlapping_loads_can_move_with_their_independent_consumers() {
    let first = VReg(0);
    let second = VReg(1);
    let first_result = VReg(2);
    let second_result = VReg(3);
    let region = vec![
        MInst::Load {
            dst: first,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: second,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        },
        MInst::AddImm {
            dst: second_result,
            src: second,
            imm: 1,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 16,
            src: second_result,
            size: OpSize::S64,
        },
        MInst::AddImm {
            dst: first_result,
            src: first,
            imm: 1,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 24,
            src: first_result,
            size: OpSize::S64,
        },
    ];

    let scheduled = schedule_region(&region, BTreeSet::new());
    let first_load = scheduled
        .instructions
        .iter()
        .position(|inst| inst == &region[0])
        .unwrap();
    let second_load = scheduled
        .instructions
        .iter()
        .position(|inst| inst == &region[1])
        .unwrap();

    assert!(scheduled.dependency_verified);
    assert!(
        second_load < first_load,
        "read-after-read must not pin independent producer chains in source order"
    );
    assert!(
        max_pressure(&scheduled.instructions, &BTreeSet::new())
            < max_pressure(&region, &BTreeSet::new())
    );
}

#[test]
fn overlapping_store_stays_after_every_prior_reader() {
    let first = VReg(0);
    let second = VReg(1);
    let stored = VReg(2);
    let region = vec![
        MInst::Load {
            dst: first,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: second,
            base: BaseReg::SimState,
            offset: 4,
            size: OpSize::S32,
        },
        MInst::LoadImm {
            dst: stored,
            value: 9,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: stored,
            size: OpSize::S64,
        },
    ];

    let scheduled = schedule_region(&region, BTreeSet::from([first, second]));
    let positions = [0, 1, 3].map(|original| {
        scheduled
            .instructions
            .iter()
            .position(|candidate| candidate == &region[original])
            .unwrap()
    });

    assert!(scheduled.dependency_verified);
    assert!(positions[0] < positions[2]);
    assert!(positions[1] < positions[2]);
}

#[test]
fn indexed_or_store_preserves_read_and_write_dependencies() {
    let before = VReg(0);
    let index = VReg(1);
    let mask = VReg(2);
    let after = VReg(3);
    let region = vec![
        MInst::Load {
            dst: before,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: index,
            value: 0,
        },
        MInst::LoadImm {
            dst: mask,
            value: 4,
        },
        MInst::OrStoreIndexed {
            base: BaseReg::SimState,
            offset: 32,
            index,
            src: mask,
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(32, 8),
        },
        MInst::Load {
            dst: after,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
    ];

    assert!(
        is_pressure_schedulable_kind(&region[3]),
        "indexed memory RMW must use its conservative alias range inside the scheduling DAG"
    );
    let scheduled = schedule_region(&region, BTreeSet::from([before, after]));
    let positions = [0, 3, 4].map(|original| {
        scheduled
            .instructions
            .iter()
            .position(|candidate| candidate == &region[original])
            .unwrap()
    });

    assert!(scheduled.dependency_verified);
    assert!(positions[0] < positions[1]);
    assert!(positions[1] < positions[2]);
}

#[test]
fn indexed_loads_are_scheduled_with_their_consumers() {
    let index = VReg(0);
    let loaded = [VReg(1), VReg(2), VReg(3), VReg(4)];
    let masked = [VReg(5), VReg(6), VReg(7), VReg(8)];
    let combined = [VReg(9), VReg(10), VReg(11)];
    let mut region = vec![MInst::LoadImm {
        dst: index,
        value: 0,
    }];
    for (lane, &dst) in loaded.iter().enumerate() {
        region.push(MInst::LoadIndexed {
            dst,
            base: BaseReg::SimState,
            offset: (lane * 8) as i32,
            index,
            scale: 1,
            size: OpSize::S64,
            alias_range: None,
        });
    }
    for (&dst, &src) in masked.iter().zip(&loaded) {
        region.push(MInst::AndImm { dst, src, imm: 1 });
    }
    region.extend([
        MInst::Or {
            dst: combined[0],
            lhs: masked[0],
            rhs: masked[1],
        },
        MInst::Or {
            dst: combined[1],
            lhs: masked[2],
            rhs: masked[3],
        },
        MInst::Or {
            dst: combined[2],
            lhs: combined[0],
            rhs: combined[1],
        },
    ]);

    let scheduled = schedule_region(&region, BTreeSet::from([combined[2]]));

    assert!(
        region.iter().all(is_pressure_schedulable_kind),
        "indexed reads and their pure consumers must form one production scheduling region"
    );
    assert!(scheduled.dependency_verified);
    assert!(
        max_pressure(&scheduled.instructions, &BTreeSet::from([combined[2]]))
            < max_pressure(&region, &BTreeSet::from([combined[2]])),
        "commuting indexed reads must not keep every loaded lane live together"
    );
}

#[test]
fn indexed_load_keeps_conservative_store_order() {
    let stored_before = VReg(0);
    let index = VReg(1);
    let loaded = VReg(2);
    let stored_after = VReg(3);
    let region = vec![
        MInst::LoadImm {
            dst: stored_before,
            value: 7,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 0,
            src: stored_before,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: index,
            value: 0,
        },
        MInst::LoadIndexed {
            dst: loaded,
            base: BaseReg::SimState,
            offset: 64,
            index,
            scale: 1,
            size: OpSize::S64,
            alias_range: None,
        },
        MInst::AddImm {
            dst: stored_after,
            src: loaded,
            imm: 1,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: stored_after,
            size: OpSize::S64,
        },
    ];

    let scheduled = schedule_region(&region, BTreeSet::new());
    let positions = [1, 3, 5].map(|original| {
        scheduled
            .instructions
            .iter()
            .position(|candidate| candidate == &region[original])
            .unwrap()
    });

    assert!(scheduled.dependency_verified);
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
}

#[test]
fn indexed_stores_do_not_split_independent_value_chains() {
    let index = VReg(0);
    let first = VReg(1);
    let second = VReg(2);
    let first_result = VReg(3);
    let second_result = VReg(4);
    let region = vec![
        MInst::Load {
            dst: first,
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: second,
            base: BaseReg::SimState,
            offset: 8,
            size: OpSize::S64,
        },
        MInst::AddImm {
            dst: first_result,
            src: first,
            imm: 1,
        },
        MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 32,
            index,
            src: first_result,
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(32, 8),
        },
        MInst::AddImm {
            dst: second_result,
            src: second,
            imm: 1,
        },
        MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: 48,
            index,
            src: second_result,
            size: OpSize::S64,
            alias_range: MemoryAliasRange::new(48, 8),
        },
    ];

    assert!(region.iter().all(is_pressure_schedulable_kind));
    let scheduled = schedule_region(&region, BTreeSet::new());

    assert!(scheduled.dependency_verified);
    assert!(
        max_pressure(&scheduled.instructions, &BTreeSet::new())
            < max_pressure(&region, &BTreeSet::new()),
        "disjoint indexed stores must allow each producer chain to close before the next one"
    );
}

#[test]
fn stale_constraint_shape_is_a_structured_error() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::Return);
    func.blocks.push(block);
    let cfg = super::super::cfg::normalize(&mut func).unwrap();
    let mut constraints = ConstraintModel::build(&func, &cfg).unwrap();
    constraints.instructions[0].pop();
    let analysis = super::super::analysis::analyze(&func);

    let error = schedule_for_pressure(&mut func, &cfg, &constraints, &analysis).unwrap_err();

    assert_eq!(error.rule, "SCHEDULE.MODEL_SHAPE");
    assert_eq!(error.block, BlockId(0));
}

#[test]
fn dependency_verifier_rejects_non_topological_orders() {
    let dependencies = vec![vec![], vec![0], vec![1]];

    assert!(dependency_order_valid(&dependencies, &[0, 1, 2]));
    assert!(!dependency_order_valid(&dependencies, &[1, 0, 2]));
    assert!(!dependency_order_valid(&dependencies, &[0, 1, 1]));
}

#[test]
fn cyclic_dependency_is_an_error_not_an_unchanged_fallback() {
    let region = vec![
        MInst::Neg {
            dst: VReg(0),
            src: VReg(1),
        },
        MInst::Neg {
            dst: VReg(1),
            src: VReg(0),
        },
    ];
    let constraints = vec![Default::default(), Default::default()];
    let mut stats = ScheduleStats::default();

    let error = schedule_block(&region, &constraints, &BTreeSet::new(), &mut stats)
        .expect_err("cyclic producer input must not silently keep the original order");

    assert!(error.to_string().contains("dependency DAG"));
}

#[test]
fn value_changes_rekey_only_indexed_ready_instructions() {
    let source = VReg(0);
    let first = VReg(1);
    let second = VReg(2);
    let region = vec![
        MInst::LoadImm {
            dst: source,
            value: 7,
        },
        MInst::Neg {
            dst: first,
            src: source,
        },
        MInst::BitNot {
            dst: second,
            src: source,
        },
    ];
    let live_out = BTreeSet::from([first, second]);

    let scheduled = schedule_region(&region, live_out);

    assert!(scheduled.dependency_verified);
    assert_eq!(scheduled.work.ready_pops, region.len());
    assert!(scheduled.work.priority_updates > 0);
    // Each live/dead transition visits only instructions named in the
    // value's use/definition index, never the whole ready population.
    assert!(scheduled.work.priority_value_index_visits <= 2 * (region.len() + 2));
}
