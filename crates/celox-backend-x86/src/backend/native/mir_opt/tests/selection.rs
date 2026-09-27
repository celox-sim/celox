use super::*;

fn delayed_immediate_branch(kind: CmpKind, imm: i32) -> MFunction {
    let mut function = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::CmpImm {
                dst: VReg(1),
                lhs: VReg(0),
                imm,
                kind,
            },
            // Clobber flags and overwrite the original memory. The
            // delayed predicate must still compare the loaded SSA value.
            MInst::AddImm {
                dst: VReg(2),
                src: VReg(0),
                imm: 1,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S64,
            },
            MInst::Branch {
                cond: VReg(1),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ],
        3,
    );
    for (block, code) in [(1, 1), (2, 2)] {
        let mut body = MBlock::new(BlockId(block));
        body.push(MInst::ReturnError { code });
        function.blocks.push(body);
    }
    function
}

#[test]
fn folds_single_use_compare_branch_before_allocation() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::Cmp {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::LtU,
            },
            MInst::Branch {
                cond: VReg(2),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ],
        3,
    );

    assert_eq!(fold_register_branch_predicates(&mut func), 1);
    assert!(matches!(
        func.blocks[0].insts.last(),
        Some(MInst::BranchPred {
            predicate: BranchPredicate::Compare {
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::LtU,
            },
            ..
        })
    ));
    assert!(
        !func.blocks[0]
            .insts
            .iter()
            .any(|instruction| instruction.def() == Some(VReg(2)))
    );
}

#[test]
fn delays_direct_memory_branch_until_after_state_forwarding() {
    let original = vec![
        MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 4,
            size: OpSize::S16,
        },
        MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    let mut early = make_func(original.clone(), 1);
    assert_eq!(fold_register_branch_predicates(&mut early), 0);
    assert_eq!(early.blocks[0].insts, original);

    let mut late = make_func(original, 1);
    assert_eq!(fold_memory_branch_predicates(&mut late), 1);
    assert!(matches!(
        late.blocks[0].insts.as_slice(),
        [MInst::BranchPred {
            predicate: BranchPredicate::MemoryNonZero {
                base: BaseReg::SimState,
                offset: 4,
                size: OpSize::S16,
            },
            ..
        }]
    ));
}

#[test]
fn delayed_branch_predicates_keep_shared_results_and_memory_order() {
    let mut shared = delayed_immediate_branch(CmpKind::Ne, 0);
    // More than 255 uses must not wrap the compact use count back to one.
    for _ in 0..256 {
        shared.blocks[0].insts.insert(
            2,
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(1),
                size: OpSize::S64,
            },
        );
    }
    shared.verify_result().unwrap();
    assert_eq!(fold_register_branch_predicates(&mut shared), 0);

    let mut memory = delayed_immediate_branch(CmpKind::Ne, 0);
    memory.blocks[0].insts[1] = MInst::Load {
        dst: VReg(1),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    };
    memory.verify_result().unwrap();
    assert_eq!(fold_memory_branch_predicates(&mut memory), 0);
    assert_eq!(fold_register_branch_predicates(&mut memory), 0);

    let mut registers = delayed_immediate_branch(CmpKind::Ne, 0);
    registers.blocks[0].insts[1] = MInst::Cmp {
        dst: VReg(1),
        lhs: VReg(0),
        rhs: VReg(0),
        kind: CmpKind::Ne,
    };
    assert_eq!(fold_register_branch_predicates(&mut registers), 0);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn delayed_immediate_branches_emit_less_code_and_preserve_signedness() {
    use crate::native::{emit, jit_mem::JitCode, regalloc};

    let mut assignment = AssignmentMap::default();
    for (value, register) in [PhysReg::RAX, PhysReg::RCX, PhysReg::RDX]
        .into_iter()
        .enumerate()
    {
        assignment.set(VReg(value as u32), register);
    }
    for kind in [
        CmpKind::Eq,
        CmpKind::Ne,
        CmpKind::LtU,
        CmpKind::LeU,
        CmpKind::GtU,
        CmpKind::GeU,
        CmpKind::LtS,
        CmpKind::LeS,
        CmpKind::GtS,
        CmpKind::GeS,
    ] {
        for imm in [i32::MIN, -1, 0, 1, i32::MAX] {
            let mut function = delayed_immediate_branch(kind, imm);
            function.verify_result().unwrap();
            let before = emit::emit(&function, &assignment, 0).unwrap();
            assert_eq!(fold_register_branch_predicates(&mut function), 1);
            function.verify_result().unwrap();
            let after = emit::emit(&function, &assignment, 0).unwrap();
            assert!(after.text_size < before.text_size);
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            let allocated = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            let before_jit = JitCode::new(&before.code).unwrap();
            let after_jit = JitCode::new(&allocated.code).unwrap();
            for input in [0u64, 1, u32::MAX as u64, i64::MAX as u64, 1 << 63, u64::MAX] {
                let rhs = imm as i64 as u64;
                let matched = match kind {
                    CmpKind::Eq => input == rhs,
                    CmpKind::Ne => input != rhs,
                    CmpKind::LtU => input < rhs,
                    CmpKind::LeU => input <= rhs,
                    CmpKind::GtU => input > rhs,
                    CmpKind::GeU => input >= rhs,
                    CmpKind::LtS => (input as i64) < (rhs as i64),
                    CmpKind::LeS => (input as i64) <= (rhs as i64),
                    CmpKind::GtS => (input as i64) > (rhs as i64),
                    CmpKind::GeS => (input as i64) >= (rhs as i64),
                };
                let mut original = vec![
                    0u8;
                    before
                        .required_state_size
                        .max(allocated.required_state_size)
                        .max(8) as usize
                ];
                original[..8].copy_from_slice(&input.to_le_bytes());
                let mut optimized = original.clone();
                let expected = if matched { 1 } else { 2 };
                assert_eq!(unsafe { before_jit.call(&mut original) }, expected);
                assert_eq!(unsafe { after_jit.call(&mut optimized) }, expected);
                assert_eq!(&original[..8], &optimized[..8]);
                assert_eq!(
                    u64::from_le_bytes(optimized[..8].try_into().unwrap()),
                    input.wrapping_add(1)
                );
            }
        }
    }
}

#[test]
fn branch_predicate_keeps_a_compare_result_used_on_an_edge() {
    let mut vregs = VRegAllocator::new();
    let input = vregs.alloc();
    let alternative = vregs.alloc();
    let condition = vregs.alloc();
    let merged = vregs.alloc();
    let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 4]);

    let mut entry = MBlock::new(BlockId(0));
    entry.insts = vec![
        MInst::CmpImm {
            dst: condition,
            lhs: input,
            imm: 0,
            kind: CmpKind::Ne,
        },
        MInst::Branch {
            cond: condition,
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    let mut true_block = MBlock::new(BlockId(1));
    true_block.push(MInst::Jump { target: BlockId(3) });
    let mut false_block = MBlock::new(BlockId(2));
    false_block.push(MInst::Jump { target: BlockId(3) });
    let mut join = MBlock::new(BlockId(3));
    join.phis.push(PhiNode {
        dst: merged,
        sources: vec![(BlockId(1), condition), (BlockId(2), alternative)],
    });
    join.push(MInst::Return);
    func.blocks = vec![entry, true_block, false_block, join];

    assert_eq!(fold_register_branch_predicates(&mut func), 0);
    assert!(matches!(
        func.blocks[0].insts.last(),
        Some(MInst::Branch { cond, .. }) if *cond == condition
    ));
}

#[test]
fn branch_predicate_does_not_consume_an_unrelated_adjacent_compare() {
    let original = vec![
        MInst::LoadImm {
            dst: VReg(2),
            value: 7,
        },
        MInst::LoadImm {
            dst: VReg(0),
            value: 1,
        },
        MInst::CmpImm {
            dst: VReg(1),
            lhs: VReg(2),
            imm: 0,
            kind: CmpKind::Eq,
        },
        MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(1),
            false_bb: BlockId(2),
        },
    ];
    let mut func = make_func(original.clone(), 3);

    assert_eq!(fold_register_branch_predicates(&mut func), 0);
    assert_eq!(func.blocks[0].insts, original);
}

#[test]
fn folds_only_proven_in_range_variable_shift_guards() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::AndImm {
                dst: VReg(1),
                src: VReg(0),
                imm: 7,
            },
            MInst::Shr {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 0,
            },
            MInst::CmpImmSelect {
                dst: VReg(4),
                lhs: VReg(1),
                imm: 64,
                kind: CmpKind::LtU,
                true_val: VReg(2),
                false_val: VReg(3),
            },
            MInst::AndImm {
                dst: VReg(5),
                src: VReg(0),
                imm: 127,
            },
            MInst::CmpImmSelect {
                dst: VReg(6),
                lhs: VReg(5),
                imm: 64,
                kind: CmpKind::LtU,
                true_val: VReg(2),
                false_val: VReg(3),
            },
            MInst::Return,
        ],
        7,
    );

    fold_proven_comparisons(&mut func);

    assert!(matches!(
        func.blocks[0].insts[4],
        MInst::Mov { src: VReg(2), .. }
    ));
    assert!(matches!(
        func.blocks[0].insts[6],
        MInst::CmpImmSelect { .. }
    ));
}

#[test]
fn optimization_folds_shift_guard_exposed_by_immediate_lowering() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 3,
            },
            MInst::And {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
            },
            MInst::LoadImm {
                dst: VReg(3),
                value: 1,
            },
            MInst::Shl {
                dst: VReg(4),
                lhs: VReg(3),
                rhs: VReg(2),
            },
            MInst::LoadImm {
                dst: VReg(5),
                value: 64,
            },
            MInst::LoadImm {
                dst: VReg(6),
                value: 0,
            },
            MInst::CmpSelect {
                dst: VReg(7),
                lhs: VReg(2),
                rhs: VReg(5),
                kind: CmpKind::LtU,
                true_val: VReg(4),
                false_val: VReg(6),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 8,
                src: VReg(7),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        8,
    );

    optimize(&mut func);

    assert!(
        !func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::CmpSelect { dst: VReg(7), .. } | MInst::CmpImmSelect { dst: VReg(7), .. }
        )),
        "{:#?}",
        func.blocks[0].insts
    );
}

#[test]
fn folds_repeated_boolean_normalization_after_immediate_lowering() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::CmpImm {
                dst: VReg(1),
                lhs: VReg(0),
                imm: 7,
                kind: CmpKind::LtU,
            },
            MInst::CmpImm {
                dst: VReg(2),
                lhs: VReg(1),
                imm: 0,
                kind: CmpKind::Ne,
            },
            MInst::Return,
        ],
        3,
    );

    fold_boolean_normalizations(&mut func);

    assert!(matches!(
        func.blocks[0].insts[2],
        MInst::Mov {
            dst: VReg(2),
            src: VReg(1)
        }
    ));
}

#[test]
fn folds_exclusive_boolean_negation_into_register_compare() {
    let mut func = make_func(
        vec![
            MInst::Cmp {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::LtU,
            },
            MInst::CmpImm {
                dst: VReg(3),
                lhs: VReg(2),
                imm: 0,
                kind: CmpKind::Eq,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(3),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        4,
    );

    fold_boolean_normalizations(&mut func);
    dead_code_eliminate(&mut func);

    assert_eq!(
        func.blocks[0].insts,
        vec![
            MInst::Cmp {
                dst: VReg(3),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::GeU,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(3),
                size: OpSize::S8,
            },
            MInst::Return,
        ]
    );
}

#[test]
fn folds_exclusive_boolean_negation_into_immediate_compare() {
    let mut func = make_func(
        vec![
            MInst::CmpImm {
                dst: VReg(1),
                lhs: VReg(0),
                imm: 7,
                kind: CmpKind::Eq,
            },
            MInst::CmpImm {
                dst: VReg(2),
                lhs: VReg(1),
                imm: 0,
                kind: CmpKind::Eq,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::Return,
        ],
        3,
    );

    fold_boolean_normalizations(&mut func);
    dead_code_eliminate(&mut func);

    assert_eq!(
        func.blocks[0].insts,
        vec![
            MInst::CmpImm {
                dst: VReg(2),
                lhs: VReg(0),
                imm: 7,
                kind: CmpKind::Ne,
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 0,
                src: VReg(2),
                size: OpSize::S8,
            },
            MInst::Return,
        ]
    );
}

#[test]
fn keeps_shared_boolean_comparison_without_duplication() {
    let mut func = make_func(
        vec![
            MInst::CmpImm {
                dst: VReg(1),
                lhs: VReg(0),
                imm: 7,
                kind: CmpKind::Eq,
            },
            MInst::CmpImm {
                dst: VReg(2),
                lhs: VReg(1),
                imm: 0,
                kind: CmpKind::Eq,
            },
            MInst::And {
                dst: VReg(3),
                lhs: VReg(1),
                rhs: VReg(0),
            },
            MInst::Return,
        ],
        4,
    );

    fold_boolean_normalizations(&mut func);

    assert!(matches!(
        func.blocks[0].insts[1],
        MInst::CmpImm {
            lhs: VReg(1),
            imm: 0,
            kind: CmpKind::Eq,
            ..
        }
    ));
}

#[test]
fn fuses_single_use_cmp_select() {
    let mut func = make_func(
        vec![
            MInst::Cmp {
                dst: VReg(2),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::GtU,
            },
            MInst::Select {
                dst: VReg(5),
                cond: VReg(2),
                true_val: VReg(3),
                false_val: VReg(4),
            },
            MInst::Return,
        ],
        6,
    );

    fuse_compare_selects(&mut func);

    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::CmpSelect {
                dst: VReg(5),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::GtU,
                true_val: VReg(3),
                false_val: VReg(4),
            },
            MInst::Return
        ]
    ));
}

#[test]
fn equal_value_selects_remove_their_complete_predicate_graph() {
    let mut func = make_func(
        vec![
            MInst::Load {
                dst: VReg(0),
                base: BaseReg::SimState,
                offset: 0,
                size: OpSize::S64,
            },
            MInst::Load {
                dst: VReg(1),
                base: BaseReg::SimState,
                offset: 8,
                size: OpSize::S64,
            },
            MInst::CmpImm {
                dst: VReg(2),
                lhs: VReg(1),
                imm: 0,
                kind: CmpKind::Ne,
            },
            MInst::Select {
                dst: VReg(3),
                cond: VReg(2),
                true_val: VReg(0),
                false_val: VReg(0),
            },
            MInst::CmpSelect {
                dst: VReg(4),
                lhs: VReg(1),
                rhs: VReg(2),
                kind: CmpKind::GtU,
                true_val: VReg(3),
                false_val: VReg(3),
            },
            MInst::CmpImmSelect {
                dst: VReg(5),
                lhs: VReg(2),
                imm: 1,
                kind: CmpKind::Eq,
                true_val: VReg(4),
                false_val: VReg(4),
            },
            MInst::GuardedCmpSelect {
                dst: VReg(6),
                guard: VReg(2),
                lhs: VReg(1),
                rhs: VReg(0),
                kind: CmpKind::LeU,
                true_val: VReg(5),
                false_val: VReg(5),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(6),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        7,
    );

    optimize(&mut func);

    assert!(matches!(
        func.blocks[0].insts.as_slice(),
        [
            MInst::Load {
                dst: VReg(0),
                offset: 0,
                ..
            },
            MInst::Store {
                src: VReg(0),
                offset: 16,
                ..
            },
            MInst::Return
        ]
    ));
}

#[test]
fn keeps_multi_use_cmp_select_condition() {
    let mut func = make_func(
        vec![
            MInst::CmpImm {
                dst: VReg(1),
                lhs: VReg(0),
                imm: 0,
                kind: CmpKind::Ne,
            },
            MInst::Select {
                dst: VReg(4),
                cond: VReg(1),
                true_val: VReg(2),
                false_val: VReg(3),
            },
            MInst::Branch {
                cond: VReg(1),
                true_bb: BlockId(1),
                false_bb: BlockId(2),
            },
        ],
        5,
    );

    fuse_compare_selects(&mut func);

    assert!(matches!(func.blocks[0].insts[0], MInst::CmpImm { .. }));
    assert!(matches!(func.blocks[0].insts[1], MInst::Select { .. }));
}

#[test]
fn sinks_selected_indexed_loads_to_one_selected_address() {
    let alias = |offset| MemoryAliasRange::new(offset, 8);
    let mut func = make_func(
        vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 16,
            },
            MInst::LoadImm {
                dst: VReg(1),
                value: 1,
            },
            MInst::LoadIndexed {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 100,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: alias(100),
            },
            MInst::LoadIndexed {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 200,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: alias(200),
            },
            MInst::LoadIndexed {
                dst: VReg(4),
                base: BaseReg::SimState,
                offset: 300,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: alias(300),
            },
            MInst::LoadIndexed {
                dst: VReg(5),
                base: BaseReg::SimState,
                offset: 400,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: alias(400),
            },
            MInst::CmpImmSelect {
                dst: VReg(6),
                lhs: VReg(1),
                imm: 0,
                kind: CmpKind::Eq,
                true_val: VReg(2),
                false_val: VReg(3),
            },
            MInst::GuardedCmpSelect {
                dst: VReg(7),
                guard: VReg(1),
                lhs: VReg(0),
                rhs: VReg(1),
                kind: CmpKind::Eq,
                true_val: VReg(4),
                false_val: VReg(5),
            },
            MInst::Select {
                dst: VReg(8),
                cond: VReg(1),
                true_val: VReg(6),
                false_val: VReg(7),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 500,
                src: VReg(8),
                size: OpSize::S64,
            },
            MInst::Return,
        ],
        9,
    );

    sink_selected_indexed_loads(&mut func);

    let insts = &func.blocks[0].insts;
    let loads = insts
        .iter()
        .filter_map(|inst| match inst {
            MInst::LoadIndexed {
                dst,
                base,
                offset,
                size,
                alias_range,
                ..
            } => Some((*dst, *base, *offset, *size, *alias_range)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        loads,
        vec![(
            VReg(8),
            BaseReg::SimState,
            100,
            OpSize::S64,
            MemoryAliasRange::new(100, 308),
        )],
        "{insts:#?}"
    );
    assert_eq!(
        insts
            .iter()
            .filter(|inst| select_values(inst).is_some())
            .count(),
        3,
        "{insts:#?}"
    );
    assert!(
        insts.iter().any(|inst| matches!(inst, MInst::Add { .. })),
        "{insts:#?}"
    );
    assert_eq!(func.verify_result(), Ok(()));
}

#[test]
fn selected_indexed_load_sinking_does_not_cross_a_write() {
    let mut func = make_func(
        vec![
            MInst::LoadIndexed {
                dst: VReg(2),
                base: BaseReg::SimState,
                offset: 100,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(100, 8),
            },
            MInst::Store {
                base: BaseReg::SimState,
                offset: 104,
                src: VReg(1),
                size: OpSize::S64,
            },
            MInst::LoadIndexed {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 200,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(200, 8),
            },
            MInst::Select {
                dst: VReg(4),
                cond: VReg(1),
                true_val: VReg(2),
                false_val: VReg(3),
            },
            MInst::Return,
        ],
        5,
    );

    sink_selected_indexed_loads(&mut func);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::LoadIndexed { .. }))
            .count(),
        2
    );
}

#[test]
fn selected_indexed_load_sinking_requires_one_shared_index() {
    let mut func = make_func(
        vec![
            MInst::LoadIndexed {
                dst: VReg(3),
                base: BaseReg::SimState,
                offset: 100,
                index: VReg(0),
                scale: 1,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(100, 8),
            },
            MInst::LoadIndexed {
                dst: VReg(4),
                base: BaseReg::SimState,
                offset: 200,
                index: VReg(1),
                scale: 1,
                size: OpSize::S64,
                alias_range: MemoryAliasRange::new(200, 8),
            },
            MInst::Select {
                dst: VReg(5),
                cond: VReg(2),
                true_val: VReg(3),
                false_val: VReg(4),
            },
            MInst::Return,
        ],
        6,
    );

    sink_selected_indexed_loads(&mut func);

    assert_eq!(
        func.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst, MInst::LoadIndexed { .. }))
            .count(),
        2
    );
}
