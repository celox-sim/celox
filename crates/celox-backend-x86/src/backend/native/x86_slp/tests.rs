use super::*;
use crate::native::mir::{BlockId, MBlock, PhiNode, SpillDesc, VRegAllocator};

#[test]
fn float_temporaries_do_not_clobber_live_simd_values() {
    let mut scalar = VRegAllocator::new();
    let input = scalar.alloc();
    let output = scalar.alloc();
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 2]);
    let vectors = (0..9).map(|_| func.alloc_x86_vec()).collect::<Vec<_>>();
    let mut block = MBlock::new(BlockId(0));
    for &dst in &vectors {
        block.push(MInst::X86Simd(X86SimdInst::Zero128 { dst }));
    }
    block.push(MInst::LoadImm {
        dst: input,
        value: 1.5f64.to_bits(),
    });
    block.push(MInst::FloatBinary {
        dst: output,
        lhs: input,
        rhs: input,
        op: celox_sir::FloatBinaryOp::Add,
        precision: celox_sir::FloatPrecision::F64,
    });
    for (index, &src) in vectors.iter().enumerate() {
        block.push(MInst::X86Simd(X86SimdInst::Store128 {
            base: BaseReg::SimState,
            offset: (index * 16) as i32,
            src,
        }));
    }
    block.push(MInst::Return);
    func.blocks.push(block);
    for tick_loop in [false, true] {
        let allocation = allocate(&func, 0, tick_loop);
        for (_, location) in allocation.assignments {
            if let X86VectorLocation::Register(register) = location {
                assert!(register.0 > 1, "FP clobbers must exclude XMM0/1");
            }
        }
        if !tick_loop {
            assert!(allocation.spilled_values >= 2);
        }
    }
}

#[test]
fn shared_adjacent_load_pair_becomes_one_vector_value() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let high = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: high,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 256,
            src: low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 264,
            src: high,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(), SpillDesc::transient()]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_loads, 1);
    assert_eq!(stats.vector_stores, 2);
    assert_eq!(stats.scalar_instructions_removed, 3);
    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::X86Simd(X86SimdInst::Load128 { .. }),
            MInst::X86Simd(X86SimdInst::Store128 { .. }),
            MInst::X86Simd(X86SimdInst::Store128 { .. }),
            MInst::Return
        ]
    ));
    let assignment = allocate(&func, 0, true);
    assert_eq!(
        assignment.assignments,
        vec![(X86VecReg(0), X86VectorLocation::Register(X86PhysVec(0)))]
    );
    assert_eq!(assignment.spill_bytes, 0);
}

#[test]
fn independent_instruction_between_adjacent_loads_still_vectorizes() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let unrelated = scalar.alloc();
    let high = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: unrelated,
            value: 7,
        },
        MInst::Load {
            dst: high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: high,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(
        scalar,
        vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ],
    );
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_loads, 1);
    assert_eq!(stats.vector_stores, 1);
    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::X86Simd(X86SimdInst::Load128 { .. }),
            MInst::LoadImm { .. },
            MInst::X86Simd(X86SimdInst::Store128 { .. }),
            MInst::Return
        ]
    ));
    func.verify();
}

#[test]
fn intervening_write_to_second_lane_blocks_load_hoisting() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let replacement = scalar.alloc();
    let high = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::LoadImm {
            dst: replacement,
            value: 9,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 40,
            src: replacement,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: high,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(
        scalar,
        vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ],
    );
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats, SlpStats::default());
    assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
    assert!(matches!(func.blocks[0].insts[3], MInst::Load { .. }));
    func.verify();
}

#[test]
fn complete_two_lane_bitwise_cone_stays_in_vector_registers() {
    let mut scalar = VRegAllocator::new();
    let lhs_low = scalar.alloc();
    let lhs_high = scalar.alloc();
    let rhs_low = scalar.alloc();
    let rhs_high = scalar.alloc();
    let result_low = scalar.alloc();
    let result_high = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: lhs_low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: lhs_high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: rhs_low,
            base: BaseReg::SimState,
            offset: 64,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: rhs_high,
            base: BaseReg::SimState,
            offset: 72,
            size: OpSize::S64,
        },
        MInst::And {
            dst: result_low,
            lhs: lhs_low,
            rhs: rhs_low,
        },
        // Commutative operand orientation need not match the low lane.
        MInst::And {
            dst: result_high,
            lhs: rhs_high,
            rhs: lhs_high,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: result_low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: result_high,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 6]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_loads, 2);
    assert_eq!(stats.vector_binary_ops, 1);
    assert_eq!(stats.vector_stores, 1);
    assert_eq!(stats.scalar_instructions_removed, 4);
    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::X86Simd(X86SimdInst::Load128 { .. }),
            MInst::X86Simd(X86SimdInst::Load128 { .. }),
            MInst::X86Simd(X86SimdInst::Binary128 {
                op: X86SimdBinaryOp::And,
                ..
            }),
            MInst::X86Simd(X86SimdInst::Store128 { .. }),
            MInst::Return,
        ]
    ));
    func.verify();
    assert_eq!(allocate(&func, 0, true).spilled_values, 0);
}

#[test]
fn scalar_use_outside_bitwise_cone_prevents_partial_vectorization() {
    let mut scalar = VRegAllocator::new();
    let lhs_low = scalar.alloc();
    let lhs_high = scalar.alloc();
    let rhs_low = scalar.alloc();
    let rhs_high = scalar.alloc();
    let result_low = scalar.alloc();
    let result_high = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.insts = vec![
        MInst::Load {
            dst: lhs_low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: lhs_high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: rhs_low,
            base: BaseReg::SimState,
            offset: 64,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: rhs_high,
            base: BaseReg::SimState,
            offset: 72,
            size: OpSize::S64,
        },
        MInst::And {
            dst: result_low,
            lhs: lhs_low,
            rhs: rhs_low,
        },
        MInst::And {
            dst: result_high,
            lhs: lhs_high,
            rhs: rhs_high,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: result_low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: result_high,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 192,
            src: lhs_low,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 6]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats, SlpStats::default());
    assert!(
        func.blocks[0]
            .insts
            .iter()
            .all(|inst| !matches!(inst, MInst::X86Simd(_)))
    );
    func.verify();
}

#[test]
fn vector_pressure_uses_a_stack_home_instead_of_failing() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let vectors = (0..16).map(|_| func.alloc_x86_vec()).collect::<Vec<_>>();
    let mut block = MBlock::new(BlockId(0));
    for (index, &vector) in vectors.iter().enumerate() {
        block.push(MInst::X86Simd(X86SimdInst::Load128 {
            dst: vector,
            base: BaseReg::SimState,
            offset: i32::try_from(index * 16).unwrap(),
        }));
    }
    for (index, &vector) in vectors.iter().enumerate() {
        block.push(MInst::X86Simd(X86SimdInst::Store128 {
            base: BaseReg::SimState,
            offset: i32::try_from(256 + index * 16).unwrap(),
            src: vector,
        }));
    }
    block.push(MInst::Return);
    func.blocks.push(block);

    let assignment = allocate(&func, 24, true);

    assert_eq!(assignment.assignments.len(), 16);
    // Once vector stack recipes are needed, XMM5 is reserved for their
    // emission scratch and the remaining pressure spills two values.
    assert_eq!(assignment.spilled_values, 2);
    assert_eq!(assignment.spill_bytes, 32);
    assert!(
        assignment
            .assignments
            .iter()
            .any(|(_, location)| { matches!(location, X86VectorLocation::Stack(32)) })
    );
}

#[test]
fn fused_vector_pressure_uses_all_registers_without_a_temporary() {
    let mut func = MFunction::new(VRegAllocator::new(), Vec::new());
    let vectors = (0..15).map(|_| func.alloc_x86_vec()).collect::<Vec<_>>();
    let mut block = MBlock::new(BlockId(0));
    for (index, &vector) in vectors.iter().enumerate() {
        block.push(MInst::X86Simd(X86SimdInst::Load128 {
            dst: vector,
            base: BaseReg::SimState,
            offset: i32::try_from(index * 16).unwrap(),
        }));
    }
    for (index, &vector) in vectors.iter().enumerate() {
        block.push(MInst::X86Simd(X86SimdInst::Store128 {
            base: BaseReg::SimState,
            offset: i32::try_from(512 + index * 16).unwrap(),
            src: vector,
        }));
    }
    block.push(MInst::Return);
    func.blocks.push(block);

    let assignment = allocate(&func, 0, true);

    assert_eq!(assignment.spilled_values, 0);
    assert_eq!(
        assignment
            .assignments
            .iter()
            .filter_map(|(_, location)| match location {
                X86VectorLocation::Register(register) => Some(register.0),
                X86VectorLocation::Stack(_) => None,
            })
            .collect::<HashSet<_>>(),
        (0..15).collect()
    );
}

#[test]
fn pack_scratch_is_colored_separately_from_the_packed_destination() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let high = scalar.alloc();
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 2]);
    let scratch_value = func.alloc_x86_vec();
    let destination_value = func.alloc_x86_vec();
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: low, value: 1 });
    block.push(MInst::LoadImm {
        dst: high,
        value: 2,
    });
    block.push(MInst::X86Simd(X86SimdInst::Scratch128 {
        dst: scratch_value,
    }));
    block.push(MInst::X86Simd(X86SimdInst::Pack128 {
        dst: destination_value,
        low,
        high,
        scratch: Some(scratch_value),
    }));
    block.push(MInst::X86Simd(X86SimdInst::Store128 {
        base: BaseReg::SimState,
        offset: 32,
        src: destination_value,
    }));
    block.push(MInst::Return);
    func.blocks.push(block);

    func.verify();
    let assignment = allocate(&func, 0, true);
    let scratch = assignment
        .assignments
        .iter()
        .find(|(value, _)| *value == scratch_value)
        .map(|(_, location)| *location)
        .expect("scratch assignment");
    let destination = assignment
        .assignments
        .iter()
        .find(|(value, _)| *value == destination_value)
        .map(|(_, location)| *location)
        .expect("destination assignment");

    assert!(matches!(scratch, X86VectorLocation::Register(_)));
    assert!(matches!(destination, X86VectorLocation::Register(_)));
    assert_ne!(scratch, destination);
}

#[test]
#[cfg(target_arch = "x86_64")]
fn store_fanout_does_not_span_late_perm_boundaries() {
    for boundary in [
        MInst::SDiv {
            dst: VReg(2),
            lhs: VReg(0),
            rhs: VReg(1),
        },
        MInst::URem {
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
        let mut scalar = VRegAllocator::new();
        let low = scalar.alloc();
        let high = scalar.alloc();
        scalar.alloc();
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: low, value: 1 });
        block.push(MInst::LoadImm {
            dst: high,
            value: 2,
        });
        for offset in [32, 64, 96, 128] {
            if offset == 96 {
                block.push(boundary.clone());
            }
            for (lane, src) in [(0, low), (8, high)] {
                block.push(MInst::Store {
                    base: BaseReg::SimState,
                    offset: offset + lane,
                    src,
                    size: OpSize::S64,
                });
            }
        }
        block.push(MInst::Return);
        let mut func = MFunction::new(scalar, vec![SpillDesc::transient(); 3]);
        func.target_features = crate::native::features::X86Features::for_test(false);
        func.blocks.push(block);
        let stats = select(&mut func);
        assert_eq!(stats.vector_packs, 0, "{boundary:?}");
        assert_eq!(stats.vector_stores, 0, "{boundary:?}");
        func.verify();
    }
}

#[test]
fn scalar_pair_with_four_store_destinations_is_packed_once() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let high = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm { dst: low, value: 1 });
    block.push(MInst::LoadImm {
        dst: high,
        value: 2,
    });
    for offset in [32, 64, 96, 128] {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src: low,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: offset + 8,
            src: high,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Return);
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(), SpillDesc::transient()]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_packs, 1);
    assert_eq!(stats.vector_stores, 4);
    assert_eq!(stats.scalar_instructions_removed, 1);
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Pack128 { .. })))
            .count(),
        1
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Scratch128 { .. })))
            .count(),
        1
    );
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Store128 { .. })))
            .count(),
        4
    );
    func.verify();
}

#[test]
fn scalar_splat_with_four_store_destinations_is_packed_without_scratch() {
    let mut scalar = VRegAllocator::new();
    let value = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    for offset in [32, 64, 96, 128] {
        for lane_offset in [0, 8] {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: offset + lane_offset,
                src: value,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::Return);
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient()]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_packs, 1);
    assert_eq!(stats.vector_stores, 4);
    assert_eq!(stats.scalar_instructions_removed, 2);
    assert!(func.blocks[0].insts.iter().any(|inst| matches!(
        inst,
        MInst::X86Simd(X86SimdInst::Pack128 { scratch: None, .. })
    )));
    assert!(
        !func.blocks[0]
            .insts
            .iter()
            .any(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Scratch128 { .. })))
    );
    func.verify();
}

#[test]
fn zero_fanout_uses_dependency_breaking_vector_zero() {
    let mut scalar = VRegAllocator::new();
    let zero = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    for offset in [32, 64, 96, 128] {
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset,
            src: zero,
            size: OpSize::S64,
        });
        block.push(MInst::Store {
            base: BaseReg::SimState,
            offset: offset + 8,
            src: zero,
            size: OpSize::S64,
        });
    }
    block.push(MInst::Return);
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient()]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_zeroes, 1);
    assert_eq!(stats.vector_packs, 0);
    assert_eq!(stats.vector_stores, 4);
    assert_eq!(stats.scalar_instructions_removed, 3);
    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::X86Simd(X86SimdInst::Zero128 { .. })))
            .count(),
        1
    );
    func.verify();
}

#[test]
fn scalar_splat_with_three_store_destinations_remains_scalar() {
    let mut scalar = VRegAllocator::new();
    let value = scalar.alloc();
    let mut block = MBlock::new(BlockId(0));
    block.push(MInst::LoadImm {
        dst: value,
        value: 1,
    });
    for offset in [32, 64, 96] {
        for lane_offset in [0, 8] {
            block.push(MInst::Store {
                base: BaseReg::SimState,
                offset: offset + lane_offset,
                src: value,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::Return);
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient()]);
    func.blocks.push(block);

    let stats = select(&mut func);

    assert_eq!(stats.vector_packs, 0);
    assert_eq!(stats.vector_stores, 0);
    assert_eq!(stats.scalar_instructions_removed, 0);
    func.verify();
}

#[test]
fn load_pair_with_cross_block_use_remains_scalar() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let high = scalar.alloc();
    let mut first = MBlock::new(BlockId(0));
    first.insts = vec![
        MInst::Load {
            dst: low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: high,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut second = MBlock::new(BlockId(1));
    second.insts = vec![
        MInst::Store {
            base: BaseReg::SimState,
            offset: 256,
            src: low,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut func = MFunction::new(scalar, vec![SpillDesc::transient(), SpillDesc::transient()]);
    func.blocks = vec![first, second];

    let stats = select(&mut func);

    assert_eq!(stats, SlpStats::default());
    assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
    assert!(matches!(func.blocks[0].insts[1], MInst::Load { .. }));
}

#[test]
fn load_pair_with_phi_use_remains_scalar() {
    let mut scalar = VRegAllocator::new();
    let low = scalar.alloc();
    let high = scalar.alloc();
    let merged = scalar.alloc();
    let mut first = MBlock::new(BlockId(0));
    first.insts = vec![
        MInst::Load {
            dst: low,
            base: BaseReg::SimState,
            offset: 32,
            size: OpSize::S64,
        },
        MInst::Load {
            dst: high,
            base: BaseReg::SimState,
            offset: 40,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 128,
            src: low,
            size: OpSize::S64,
        },
        MInst::Store {
            base: BaseReg::SimState,
            offset: 136,
            src: high,
            size: OpSize::S64,
        },
        MInst::Return,
    ];
    let mut second = MBlock::new(BlockId(1));
    second.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(0), low)],
    });
    second.insts.push(MInst::Return);
    let mut func = MFunction::new(
        scalar,
        vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ],
    );
    func.blocks = vec![first, second];

    let stats = select(&mut func);

    assert_eq!(stats, SlpStats::default());
    assert!(matches!(func.blocks[0].insts[0], MInst::Load { .. }));
    assert!(matches!(func.blocks[0].insts[1], MInst::Load { .. }));
}
