use super::*;
use crate::allocation::{Assignment, EdgeCopyPlan};
use crate::mir::{BaseReg, MBlock, MInst, OpSize, PhiNode};

fn diamond_function() -> MFunction {
    let entry = MBlock {
        id: BlockId(10),
        phis: Vec::new(),
        insts: vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 1,
            },
            MInst::Branch {
                cond: VReg(0),
                true_bb: BlockId(20),
                false_bb: BlockId(30),
            },
        ],
    };
    let left = MBlock {
        id: BlockId(20),
        phis: Vec::new(),
        insts: vec![
            MInst::LoadImm {
                dst: VReg(1),
                value: 2,
            },
            MInst::Jump {
                target: BlockId(40),
            },
        ],
    };
    let right = MBlock {
        id: BlockId(30),
        phis: Vec::new(),
        insts: vec![
            MInst::LoadImm {
                dst: VReg(2),
                value: 3,
            },
            MInst::Jump {
                target: BlockId(40),
            },
        ],
    };
    let join = MBlock {
        id: BlockId(40),
        phis: vec![PhiNode {
            dst: VReg(3),
            sources: vec![(BlockId(20), VReg(1)), (BlockId(30), VReg(2))],
        }],
        insts: vec![MInst::Return],
    };
    MFunction::new(vec![entry, left, right, join], Vec::new())
}

#[test]
fn exports_normalized_cfg_phi_and_instruction_facts() {
    let facts = build_facts(&diamond_function()).unwrap();

    assert_eq!(facts.blocks[0].successors, vec![1, 2]);
    assert_eq!(facts.blocks[0].instructions[0].defs, vec![VReg(0)]);
    assert_eq!(facts.blocks[0].instructions[1].uses, vec![VReg(0)]);
    assert_eq!(facts.blocks[3].phis[0].destination, VReg(3));
    assert_eq!(facts.blocks[3].phis[0].sources[0].predecessor, 1);
    assert_eq!(facts.blocks[3].phis[0].sources[1].predecessor, 2);
    analyze_live_intervals(&facts).unwrap();
}

#[test]
fn rejects_missing_and_reserved_instruction_assignments() {
    let function = diamond_function();
    let mut allocated = AllocatedFunction {
        function,
        assignment: Assignment::default(),
        edge_copies: EdgeCopyPlan::default(),
    };
    assert!(matches!(
        verify_allocated(&allocated),
        Err(TargetRegallocError::MissingAssignment { value: VReg(0), .. })
    ));

    for (value, register) in [
        (VReg(0), Arm64Reg::new(1)),
        (VReg(1), Arm64Reg::new(2)),
        (VReg(2), Arm64Reg::new(3)),
    ] {
        allocated.assignment.set(value, register);
    }
    allocated.assignment.set(VReg(0), Arm64Reg::new(16));
    assert!(matches!(
        verify_allocated(&allocated),
        Err(TargetRegallocError::ReservedAssignment { value: VReg(0), .. })
    ));
}

#[test]
fn rejects_interfering_values_assigned_to_one_register() {
    let function = MFunction::new(
        vec![MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 1,
                },
                MInst::LoadImm {
                    dst: VReg(1),
                    value: 2,
                },
                MInst::Add {
                    dst: VReg(2),
                    lhs: VReg(0),
                    rhs: VReg(1),
                },
                MInst::Return,
            ],
        }],
        Vec::new(),
    );
    let mut assignment = Assignment::default();
    assignment.set(VReg(0), Arm64Reg::new(1));
    assignment.set(VReg(1), Arm64Reg::new(1));
    assignment.set(VReg(2), Arm64Reg::new(2));
    let allocated = AllocatedFunction {
        function,
        assignment,
        edge_copies: EdgeCopyPlan::default(),
    };

    assert!(matches!(
        verify_allocated(&allocated),
        Err(TargetRegallocError::RegisterConflict {
            register,
            ..
        }) if register == Arm64Reg::new(1)
    ));
}

#[test]
fn register_verification_checks_values_across_block_boundaries() {
    let mut entry = MBlock::new(BlockId(10));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 1,
    });
    entry.push(MInst::Jump {
        target: BlockId(20),
    });
    let mut middle = MBlock::new(BlockId(20));
    middle.push(MInst::LoadImm {
        dst: VReg(1),
        value: 2,
    });
    middle.push(MInst::Jump {
        target: BlockId(30),
    });
    let mut exit = MBlock::new(BlockId(30));
    exit.push(MInst::Add {
        dst: VReg(2),
        lhs: VReg(0),
        rhs: VReg(1),
    });
    exit.push(MInst::Return);
    let function = MFunction::new(vec![entry, middle, exit], Vec::new());
    let mut allocated = allocate_without_spills(function).unwrap();
    verify_allocated(&allocated).unwrap();
    let register = allocated.assignment.get(&VReg(0)).unwrap();
    allocated.assignment.set(VReg(1), register);
    assert!(matches!(
        verify_allocated(&allocated),
        Err(TargetRegallocError::RegisterConflict {
            block: BlockId(20),
            ..
        })
    ));
}

#[test]
fn allocates_target_owned_mir_without_legacy_colors() {
    let allocated = allocate_without_spills(diamond_function()).unwrap();

    for value in [VReg(0), VReg(1), VReg(2), VReg(3)] {
        let register = allocated.assignment.get(&value).unwrap();
        assert!((1..=15).contains(&register.number()) || (19..=27).contains(&register.number()));
    }
    verify_allocated(&allocated).unwrap();
}

#[test]
fn lowers_target_phi_rows_with_the_common_copy_resolver() {
    let predecessor = MBlock {
        id: BlockId(0),
        phis: Vec::new(),
        insts: vec![
            MInst::LoadImm {
                dst: VReg(0),
                value: 7,
            },
            MInst::Jump { target: BlockId(1) },
        ],
    };
    let successor = MBlock {
        id: BlockId(1),
        phis: vec![PhiNode {
            dst: VReg(1),
            sources: vec![(BlockId(0), VReg(0))],
        }],
        insts: vec![MInst::Return],
    };
    let function = MFunction::new(vec![predecessor, successor], Vec::new());
    let mut assignment = Assignment::default();
    assignment.set(VReg(0), Arm64Reg::new(1));
    assignment.set(VReg(1), Arm64Reg::new(2));

    let copies = build_edge_copies(&function, &assignment).unwrap();

    assert_eq!(
        copies.edge(BlockId(0), BlockId(1)),
        Some(
            [CopyOperation::Move {
                destination: CopyDestination::Register(Arm64Reg::new(2)),
                source: CopySource::Register(Arm64Reg::new(1)),
            }]
            .as_slice()
        )
    );
}

#[test]
fn reports_pressure_before_spill_support_is_enabled() {
    let mut instructions = (0..26)
        .map(|value| MInst::LoadImm {
            dst: VReg(value),
            value: u64::from(value),
        })
        .collect::<Vec<_>>();
    instructions.extend((0..26).map(|value| MInst::Store {
        base: BaseReg::SimState,
        offset: value * 8,
        src: VReg(value as u32),
        size: OpSize::S64,
    }));
    instructions.push(MInst::Return);
    let function = MFunction::new(
        vec![MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: instructions,
        }],
        Vec::new(),
    );

    assert!(matches!(
        allocate_without_spills(function),
        Err(TargetRegallocError::RegisterPressure { .. })
    ));
}

#[test]
fn prefers_long_lived_values_with_fewer_uses_for_spilling() {
    let mut instructions = (0..21_u32)
        .map(|value| MInst::LoadImm {
            dst: VReg(value),
            value: u64::from(value),
        })
        .collect::<Vec<_>>();
    instructions.extend((0..100).map(|index| MInst::Store {
        base: BaseReg::SimState,
        offset: index * 8,
        src: VReg(1),
        size: OpSize::S64,
    }));
    instructions.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 800,
        src: VReg(0),
        size: OpSize::S64,
    });
    instructions.extend((2..21_u32).map(|value| MInst::Store {
        base: BaseReg::SimState,
        offset: 800 + (value as i32) * 8,
        src: VReg(value),
        size: OpSize::S64,
    }));
    instructions.push(MInst::Return);
    let function = MFunction::new(
        vec![MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: instructions,
        }],
        Vec::new(),
    );
    let facts = build_facts(&function).unwrap();
    let intervals = analyze_live_intervals(&facts).unwrap();
    let candidates = facts.blocks[0]
        .instructions
        .iter()
        .flat_map(|instruction| instruction.defs.iter().copied())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        select_spill_batch(&function, &intervals, &candidates, &BTreeMap::new(), false),
        vec![VReg(0)]
    );
}

#[test]
fn spill_cost_keeps_repeated_loop_uses_in_registers() {
    let mut entry = MBlock::new(BlockId(10));
    entry.push(MInst::LoadImm {
        dst: VReg(0),
        value: 7,
    });
    entry.push(MInst::LoadImm {
        dst: VReg(1),
        value: 9,
    });
    // Both values span a long entry block, but only v0 is read on every
    // loop iteration. The eight exit stores execute just once.
    entry
        .insts
        .extend((0..100).map(|_| MInst::KeepAlive { src: VReg(0) }));
    entry.push(MInst::Jump {
        target: BlockId(20),
    });
    let mut header = MBlock::new(BlockId(20));
    header.push(MInst::Store {
        base: BaseReg::SimState,
        offset: 0,
        src: VReg(0),
        size: OpSize::S64,
    });
    header.push(MInst::Branch {
        cond: VReg(0),
        true_bb: BlockId(20),
        false_bb: BlockId(30),
    });
    let mut exit = MBlock::new(BlockId(30));
    exit.insts.extend((0..8).map(|index| MInst::Store {
        base: BaseReg::SimState,
        offset: 8 + index * 8,
        src: VReg(1),
        size: OpSize::S64,
    }));
    exit.push(MInst::Return);
    let function = MFunction::new(vec![entry, header, exit], Vec::new());
    let facts = build_facts(&function).unwrap();
    let frequencies = spill_frequencies(&function, &facts);
    assert_eq!(
        frequencies,
        BTreeMap::from([(BlockId(10), 1), (BlockId(20), 8), (BlockId(30), 1)])
    );
    let intervals = analyze_live_intervals(&facts).unwrap();
    let candidates = BTreeSet::from([VReg(0), VReg(1)]);
    assert_eq!(
        select_spill_batch(&function, &intervals, &candidates, &BTreeMap::new(), true),
        vec![VReg(0)]
    );
    assert_eq!(
        select_spill_batch(&function, &intervals, &candidates, &frequencies, true),
        vec![VReg(1)]
    );
}

#[test]
fn spill_frequencies_distinguish_nested_loops_and_exits() {
    let mut blocks = (0..6)
        .map(|index| MBlock::new(BlockId(index * 10)))
        .collect::<Vec<_>>();
    blocks[0].push(MInst::LoadImm {
        dst: VReg(0),
        value: 0,
    });
    for (index, target) in [(0, 10), (1, 20), (2, 30)] {
        blocks[index].push(MInst::Jump {
            target: BlockId(target),
        });
    }
    blocks[3].push(MInst::Branch {
        cond: VReg(0),
        true_bb: BlockId(20),
        false_bb: BlockId(40),
    });
    blocks[4].push(MInst::Branch {
        cond: VReg(0),
        true_bb: BlockId(10),
        false_bb: BlockId(50),
    });
    blocks[5].push(MInst::Return);
    let function = MFunction::new(blocks, Vec::new());
    let facts = build_facts(&function).unwrap();
    assert_eq!(
        spill_frequencies(&function, &facts)
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 8, 64, 64, 8, 1]
    );
}

#[test]
fn inserts_target_owned_spill_and_reload_instructions() {
    let mut instructions = (0..26)
        .map(|value| MInst::Load {
            dst: VReg(value),
            base: BaseReg::SimState,
            offset: (value * 8) as i32,
            size: OpSize::S64,
        })
        .collect::<Vec<_>>();
    // Keep a pressure boundary: scheduling independent loads and stores
    // together would otherwise remove the need for spill reconstruction.
    instructions.push(MInst::KeepAlive { src: VReg(0) });
    instructions.extend((0..26).map(|value| MInst::Store {
        base: BaseReg::SimState,
        offset: value * 8,
        src: VReg(value as u32),
        size: OpSize::S64,
    }));
    instructions.push(MInst::Return);
    let allocation = allocate_with_spills(
        MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ),
        || false,
    )
    .unwrap();

    assert!(allocation.spill_frame_size >= 8);
    assert_eq!(allocation.spill_frame_size % 8, 0);
    let reload_count = allocation.allocated.function.blocks[0]
        .insts
        .iter()
        .filter(|instruction| {
            matches!(
                instruction,
                MInst::Load {
                    base: BaseReg::StackFrame,
                    size: OpSize::S64,
                    ..
                }
            )
        })
        .count();
    assert!(reload_count > 0);
    assert!(
        allocation.allocated.function.blocks[0]
            .insts
            .iter()
            .any(|instruction| matches!(
                instruction,
                MInst::Store {
                    base: BaseReg::StackFrame,
                    offset: 0,
                    size: OpSize::S64,
                    ..
                }
            ))
    );
    verify_allocated(&allocation.allocated).unwrap();
}

#[test]
fn reuses_one_reload_across_adjacent_uses() {
    let mut function = MFunction::new(
        vec![MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: vec![
                MInst::LoadImm {
                    dst: VReg(0),
                    value: 42,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 0,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Store {
                    base: BaseReg::SimState,
                    offset: 8,
                    src: VReg(0),
                    size: OpSize::S64,
                },
                MInst::Return,
            ],
        }],
        Vec::new(),
    );
    let homes = [(VReg(0), 0)].into_iter().collect();
    function.spill_homes.insert(VReg(0), 0);
    let mut next_value = 1;

    spill_values(&mut function, &homes, &mut next_value).unwrap();

    assert_eq!(
        function.blocks[0]
            .insts
            .iter()
            .filter(|instruction| matches!(
                instruction,
                MInst::Load {
                    base: BaseReg::StackFrame,
                    ..
                }
            ))
            .count(),
        0
    );
    allocate_without_spills(function).unwrap();
}

#[test]
fn reuses_a_spilled_index_across_address_calculations() {
    let mut instructions = vec![MInst::Load {
        dst: VReg(0),
        base: BaseReg::SimState,
        offset: 0,
        size: OpSize::S64,
    }];
    // Move the first use beyond the reuse window, so it must reload.
    instructions.extend((1..=10).map(|value| MInst::LoadImm {
        dst: VReg(value),
        value: u64::from(value),
    }));
    for index in 0..3 {
        instructions.push(MInst::AddImm {
            dst: VReg(11 + index),
            src: VReg(0),
            imm: index as i32,
        });
        instructions.push(MInst::Store {
            base: BaseReg::SimState,
            offset: (index * 8) as i32,
            src: VReg(11 + index),
            size: OpSize::S64,
        });
    }
    instructions.push(MInst::Return);
    let mut function = MFunction::new(
        vec![MBlock {
            id: BlockId(0),
            phis: vec![],
            insts: instructions,
        }],
        vec![],
    );
    let homes = [(VReg(0), 0)].into_iter().collect();
    function.spill_homes.insert(VReg(0), 0);
    spill_values(&mut function, &homes, &mut 14).unwrap();
    assert_eq!(
        function.blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(
                inst,
                MInst::Load {
                    base: BaseReg::StackFrame,
                    ..
                }
            ))
            .count(),
        1,
    );
    allocate_without_spills(function).unwrap();
}

#[test]
fn colors_disjoint_pressure_waves_into_shared_stack_slots() {
    let mut instructions = Vec::new();
    for wave in 0..2_u32 {
        let first = wave * 26;
        instructions.extend((0..26).map(|value| MInst::LoadImm {
            dst: VReg(first + value),
            value: u64::from(first + value),
        }));
        instructions.push(MInst::KeepAlive { src: VReg(first) });
        instructions.extend((0..26).map(|value| MInst::Store {
            base: BaseReg::SimState,
            offset: i32::try_from((first + value) * 8).unwrap(),
            src: VReg(first + value),
            size: OpSize::S64,
        }));
    }
    instructions.push(MInst::Return);
    let allocation = allocate_with_spills(
        MFunction::new(
            vec![MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: instructions,
            }],
            Vec::new(),
        ),
        || false,
    )
    .unwrap();

    let spill_count = allocation.allocated.function.spill_homes.len();
    let slot_count = allocation.spill_frame_size / 8;
    assert!(spill_count > usize::try_from(slot_count).unwrap());
    verify_allocated(&allocation.allocated).unwrap();
}

#[test]
fn keeps_external_phi_register_sources_live_to_the_edge() {
    let mut function = MFunction::new(
        vec![
            MBlock {
                id: BlockId(0),
                phis: Vec::new(),
                insts: vec![
                    MInst::LoadImm {
                        dst: VReg(0),
                        value: 42,
                    },
                    MInst::Jump { target: BlockId(1) },
                ],
            },
            MBlock {
                id: BlockId(1),
                phis: vec![PhiNode {
                    dst: VReg(1),
                    sources: vec![(BlockId(0), VReg(0))],
                }],
                insts: vec![
                    MInst::Store {
                        base: BaseReg::SimState,
                        offset: 0,
                        src: VReg(1),
                        size: OpSize::S64,
                    },
                    MInst::Return,
                ],
            },
        ],
        Vec::new(),
    );
    let homes = [(VReg(1), 8)].into_iter().collect();
    let mut next_value = 2;
    function.spill_homes.insert(VReg(1), 8);

    spill_values(&mut function, &homes, &mut next_value).unwrap();

    let facts = build_facts(&function).unwrap();
    let intervals = analyze_live_intervals(&facts).unwrap();
    assert!(intervals.get(&VReg(0)).is_some());
    assert!(intervals.get(&VReg(1)).is_none());
    assert!(matches!(
        function.blocks[0].insts.as_slice(),
        [
            MInst::LoadImm { .. },
            MInst::KeepAlive { src: VReg(0) },
            MInst::Jump { .. }
        ]
    ));
    assert!(matches!(
        function.blocks[1].insts.first(),
        Some(MInst::Load {
            base: BaseReg::StackFrame,
            offset: 8,
            ..
        })
    ));
    assert_eq!(function.spilled_phis.len(), 1);
    let allocated = allocate_without_spills(function).unwrap();
    let source = allocated.assignment.get(&VReg(0)).unwrap();
    assert_eq!(
        allocated.edge_copies.edge(BlockId(0), BlockId(1)),
        Some(
            [CopyOperation::Move {
                destination: CopyDestination::Stack(8),
                source: CopySource::Register(source),
            }]
            .as_slice()
        )
    );
}

#[test]
fn external_phi_sources_interfere_with_reloaded_branch_conditions() {
    for separate_rounds in [false, true] {
        let mut entry = MBlock::new(BlockId(0));
        entry.push(MInst::Load {
            dst: VReg(0),
            base: BaseReg::SimState,
            offset: 0,
            size: OpSize::S64,
        });
        entry.push(MInst::LoadImm {
            dst: VReg(1),
            value: 11,
        });
        for _ in 0..10 {
            entry.push(MInst::Store {
                base: BaseReg::SimState,
                offset: 16,
                src: VReg(1),
                size: OpSize::S64,
            });
        }
        entry.push(MInst::Branch {
            cond: VReg(0),
            true_bb: BlockId(2),
            false_bb: BlockId(1),
        });
        let mut other = MBlock::new(BlockId(1));
        other.push(MInst::LoadImm {
            dst: VReg(3),
            value: 29,
        });
        other.push(MInst::Jump { target: BlockId(2) });
        let mut join = MBlock::new(BlockId(2));
        join.phis.push(PhiNode {
            dst: VReg(4),
            sources: vec![(BlockId(0), VReg(1)), (BlockId(1), VReg(3))],
        });
        join.push(MInst::Store {
            base: BaseReg::SimState,
            offset: 8,
            src: VReg(4),
            size: OpSize::S64,
        });
        join.push(MInst::Return);
        let mut function = MFunction::new(vec![entry, other, join], vec![]);
        let mut next = 5;
        if separate_rounds {
            let homes = BTreeMap::from([(VReg(4), 8)]);
            function.spill_homes.extend(homes.clone());
            spill_values(&mut function, &homes, &mut next).unwrap();
        }
        let homes = if separate_rounds {
            BTreeMap::from([(VReg(0), 0)])
        } else {
            BTreeMap::from([(VReg(0), 0), (VReg(4), 8)])
        };
        function.spill_homes.extend(homes.clone());
        spill_values(&mut function, &homes, &mut next).unwrap();
        let MInst::Branch { cond, .. } = *function.blocks[0].insts.last().unwrap() else {
            unreachable!()
        };
        let allocated = allocate_without_spills(function).unwrap();
        assert_ne!(
            allocated.assignment.get(&VReg(1)),
            allocated.assignment.get(&cond),
            "separate_rounds={separate_rounds}: the edge value must survive the branch-condition reload"
        );
    }
}

#[test]
fn uses_preserved_registers_above_caller_saved_pressure() {
    let mut instructions = (0..16)
        .map(|value| MInst::LoadImm {
            dst: VReg(value),
            value: u64::from(value),
        })
        .collect::<Vec<_>>();
    instructions.extend((0..16).map(|value| MInst::Store {
        base: BaseReg::SimState,
        offset: value * 8,
        src: VReg(value as u32),
        size: OpSize::S64,
    }));
    instructions.push(MInst::Return);
    let allocated = allocate_without_spills(MFunction::new(
        vec![MBlock {
            id: BlockId(0),
            phis: Vec::new(),
            insts: instructions,
        }],
        Vec::new(),
    ))
    .unwrap();

    assert!(
        allocated
            .assignment
            .iter()
            .any(|(_, register)| (19..=27).contains(&register.number()))
    );
}
