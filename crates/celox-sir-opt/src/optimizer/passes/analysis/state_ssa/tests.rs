use super::*;
use crate::ir::cfg::SirCfg;
use celox_design::StateObjectId as VarId;

fn bit(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn logic(width: usize) -> RegisterType {
    RegisterType::Logic { width }
}

fn address(region: u32, variable: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(variable),
    }
}

fn block(
    id: usize,
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    terminator: SIRTerminator,
) -> BasicBlock<RegionedAbsoluteAddr> {
    BasicBlock {
        id: BlockId(id),
        params: Vec::new(),
        instructions,
        terminator,
    }
}

fn unit(
    blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>,
    registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
        register_map: registers.into_iter().collect(),
    }
}

fn use_access(state: &StateSsa, destination: RegisterId) -> &MemoryAccess {
    state
        .accesses
        .iter()
        .find(|access| {
            matches!(
                access.kind,
                MemoryAccessKind::Use {
                    destination: Some(register),
                    ..
                } if register == destination
            )
        })
        .expect("destination has a StateSSA use")
}

#[test]
fn native_kernel_call_observes_and_kills_every_state_region() {
    for region in [STABLE_REGION, WORKING_REGION, SPARSE_WORKING_REGION] {
        let addr = address(region, 0);
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                    SIRInstruction::ExternCall {
                        dst: None,
                        func: celox_sir::extern_abi::NATIVE_KERNEL_CALL_TAG,
                        args: Vec::new(),
                    },
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            (0..3).map(|i| (RegisterId(i), bit(8))),
        );
        eu.verify_result().unwrap();
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state = StateSsa::analyze_all_loads(&eu, &cfg, region).unwrap();
        let MemoryAccessKind::Use {
            reaching: before, ..
        } = use_access(&state, RegisterId(1)).kind
        else {
            panic!("load")
        };
        let MemoryAccessKind::Use {
            reaching: after, ..
        } = use_access(&state, RegisterId(2)).kind
        else {
            panic!("load")
        };
        assert_ne!(before, after);
        assert!(matches!(
            state.accesses[after.0].kind,
            MemoryAccessKind::Kill { .. }
        ));
        assert!(state.slots[0].escapes && state.slots[0].has_kill);
        assert!(
            state
                .accesses
                .iter()
                .any(|access| access.instruction == Some(3)
                    && matches!(
                        access.kind,
                        MemoryAccessKind::Use {
                            destination: None,
                            ..
                        }
                    ))
        );
    }
}

#[test]
fn a_call_observes_and_kills_only_what_its_callee_touches() {
    let addr = address(STABLE_REGION, 0);
    // Whether the load after a call that reads and writes `touched` sees
    // a new version.
    let killed = |touched: RegionedAbsoluteAddr| {
        let eu = unit(
            vec![block(
                0,
                vec![
                    SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
                    SIRInstruction::Imm(RegisterId(3), SIRValue::new(0u8)),
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Call {
                        function: celox_sir::FunctionId(0),
                        frame: RegisterId(3),
                        args: Vec::new(),
                        outputs: Vec::new(),
                        dst: None,
                        effects: std::sync::Arc::new(celox_sir::CallEffects {
                            reads: vec![touched],
                            writes: vec![touched],
                            ..Default::default()
                        }),
                    },
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                ],
                SIRTerminator::Return,
            )],
            (0..4).map(|i| (RegisterId(i), bit(8))),
        );
        eu.verify_result().unwrap();
        let cfg = SirCfg::analyze(&eu).unwrap();
        let state = StateSsa::analyze_all_loads(&eu, &cfg, STABLE_REGION).unwrap();
        let reaching = |register| match use_access(&state, register).kind {
            MemoryAccessKind::Use { reaching, .. } => reaching,
            _ => panic!("load"),
        };
        let (before, after) = (reaching(RegisterId(1)), reaching(RegisterId(2)));
        if before == after {
            return false;
        }
        assert!(matches!(
            state.accesses[after.0].kind,
            MemoryAccessKind::Kill { .. }
        ));
        assert!(state.slots[0].escapes);
        true
    };
    assert!(killed(addr));
    assert!(!killed(address(STABLE_REGION, 1)));
}

#[test]
fn version_snapshots_preserve_versions_across_multiple_checkpoints() {
    let stable = address(STABLE_REGION, 0);
    let last = VERSION_CHECKPOINT_INTERVAL * 2 + 3;
    let mut blocks = Vec::new();
    for id in 0..=last {
        let instructions = if id == 0 {
            vec![SIRInstruction::Store(
                stable,
                SIROffset::Static(0),
                8,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            )]
        } else if id == last {
            vec![SIRInstruction::Load(
                RegisterId(1),
                stable,
                SIROffset::Static(0),
                8,
            )]
        } else {
            Vec::new()
        };
        let terminator = if id == last {
            SIRTerminator::Return
        } else {
            SIRTerminator::Jump(BlockId(id + 1), Vec::new())
        };
        blocks.push(block(id, instructions, terminator));
    }
    let eu = unit(blocks, [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))]);
    let cfg = SirCfg::analyze(&eu).unwrap();

    let state = StateSsa::analyze_all_loads(&eu, &cfg, STABLE_REGION).unwrap();
    let (_, reaching) = state
        .read_version(BlockId(last), 0, RegisterId(1))
        .expect("final load has a reaching state version");

    assert_eq!(state.entry_version(BlockId(last), 0), Some(reaching));
    assert_eq!(state.exit_version(BlockId(last), 0), Some(reaching));
    assert!(matches!(
        state.accesses[reaching.0].kind,
        MemoryAccessKind::Def {
            source: RegisterId(0),
            ..
        }
    ));
}

#[test]
fn selected_loads_still_validate_an_earlier_exact_store_type() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![block(
            0,
            vec![
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    32,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 32),
                SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 32),
            ],
            SIRTerminator::Return,
        )],
        [
            (RegisterId(0), RegisterType::Logic { width: 32 }),
            (RegisterId(1), bit(32)),
            (RegisterId(2), bit(32)),
        ],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();
    let eligible = [RegisterId(1), RegisterId(2)]
        .into_iter()
        .collect::<HashSet<_>>();

    let state = StateSsa::analyze_selected_loads(&eu, &cfg, STABLE_REGION, &eligible).unwrap();

    assert!(state.slots.is_empty());
    assert!(state.read_version(BlockId(0), 1, RegisterId(1)).is_none());
    assert!(state.read_version(BlockId(0), 2, RegisterId(2)).is_none());
}

#[test]
fn two_state_placement_unifies_bit_and_logic_storage_versions() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![block(
            0,
            vec![
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
            ],
            SIRTerminator::Return,
        )],
        [(RegisterId(0), bit(8)), (RegisterId(1), logic(8))],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();

    let four_state = StateSsa::analyze_all_loads(&eu, &cfg, STABLE_REGION).unwrap();
    assert!(four_state.slots.is_empty());

    let two_state = StateSsa::analyze_all_loads_two_state(&eu, &cfg, STABLE_REGION).unwrap();
    assert_eq!(two_state.slots.len(), 1);
    assert!(matches!(
        two_state.slots[0].fragment.plane,
        StatePlane::TwoStateValue
    ));
    assert!(
        two_state
            .read_version(BlockId(0), 1, RegisterId(1))
            .is_some()
    );
}

#[test]
fn placement_mode_versions_a_join_without_an_original_load() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![
            block(
                0,
                vec![SIRInstruction::Load(
                    RegisterId(0),
                    stable,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Branch {
                    cond: RegisterId(1),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(3), Vec::new()),
            ),
            block(2, Vec::new(), SIRTerminator::Jump(BlockId(3), Vec::new())),
            block(3, Vec::new(), SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(1)),
            (RegisterId(2), bit(8)),
        ],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();

    let state = StateSsa::analyze_all_loads(&eu, &cfg, STABLE_REGION).unwrap();

    let (slot, original) = state
        .read_version(BlockId(0), 0, RegisterId(0))
        .expect("the original load has a version");
    let join = state
        .entry_version(BlockId(3), slot)
        .expect("placement mode versions every candidate block");
    assert_ne!(join, original);
    assert!(matches!(
        state.accesses[join.0].kind,
        MemoryAccessKind::Phi { .. }
    ));
}

#[test]
fn overlapping_store_is_a_path_local_kill_and_phi_input() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![
            block(
                0,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Branch {
                    cond: RegisterId(2),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(4),
                    4,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(3), Vec::new()),
            ),
            block(2, Vec::new(), SIRTerminator::Jump(BlockId(3), Vec::new())),
            block(
                3,
                vec![SIRInstruction::Load(
                    RegisterId(3),
                    stable,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Return,
            ),
        ],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(4)),
            (RegisterId(2), bit(1)),
            (RegisterId(3), bit(8)),
        ],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();
    let state = StateSsa::analyze(&eu, &cfg, STABLE_REGION, None).unwrap();

    assert_eq!(state.slots.len(), 1);
    assert!(state.slots[0].has_kill);
    assert_eq!(state.slots[0].phi_blocks, [cfg.index[&BlockId(3)]]);
    let phi = state
        .accesses
        .iter()
        .find(|access| matches!(access.kind, MemoryAccessKind::Phi { .. }))
        .unwrap();
    let use_access = use_access(&state, RegisterId(3));
    assert!(matches!(
        use_access.kind,
        MemoryAccessKind::Use { reaching, .. } if reaching == phi.id
    ));
    let MemoryAccessKind::Phi { incoming } = &phi.kind else {
        unreachable!()
    };
    assert!(incoming.iter().any(|(_, version)| matches!(
        state.accesses[version.0].kind,
        MemoryAccessKind::Kill { .. }
    )));
    assert!(incoming.iter().any(|(_, version)| matches!(
        state.accesses[version.0].kind,
        MemoryAccessKind::Def { .. }
    )));
}

#[test]
fn forward_cfg_keeps_branch_store_phi_inputs() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![
            block(
                0,
                Vec::new(),
                SIRTerminator::Branch {
                    cond: RegisterId(0),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(
                1,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    64,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(3), Vec::new()),
            ),
            block(
                2,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    64,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(3), Vec::new()),
            ),
            block(
                3,
                vec![SIRInstruction::Load(
                    RegisterId(3),
                    stable,
                    SIROffset::Static(0),
                    64,
                )],
                SIRTerminator::Return,
            ),
        ],
        [
            (RegisterId(0), bit(1)),
            (RegisterId(1), bit(64)),
            (RegisterId(2), bit(64)),
            (RegisterId(3), bit(64)),
        ],
    );
    let cfg = SirCfg::analyze_forward(&eu).unwrap();
    let state = StateSsa::analyze(&eu, &cfg, STABLE_REGION, None).unwrap();

    assert_eq!(state.slots[0].phi_blocks, [cfg.index[&BlockId(3)]]);
    let phi = state
        .accesses
        .iter()
        .find(|access| matches!(access.kind, MemoryAccessKind::Phi { .. }))
        .expect("the branch definitions must merge");
    let MemoryAccessKind::Phi { incoming } = &phi.kind else {
        unreachable!()
    };
    assert_eq!(incoming.len(), 2);
    assert!(incoming.iter().all(|(_, version)| matches!(
        state.accesses[version.0].kind,
        MemoryAccessKind::Def { .. }
    )));
    assert!(matches!(
        use_access(&state, RegisterId(3)).kind,
        MemoryAccessKind::Use { reaching, .. } if reaching == phi.id
    ));
}

#[test]
fn dynamic_store_kills_only_the_aliased_address() {
    let first = address(STABLE_REGION, 0);
    let second = address(STABLE_REGION, 1);
    let eu = unit(
        vec![block(
            0,
            vec![
                SIRInstruction::Store(
                    first,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    second,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    first,
                    SIROffset::Dynamic(RegisterId(4)),
                    1,
                    RegisterId(2),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(3), first, SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(5), second, SIROffset::Static(0), 8),
            ],
            SIRTerminator::Return,
        )],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(1)),
            (RegisterId(3), bit(8)),
            (RegisterId(4), bit(8)),
            (RegisterId(5), bit(8)),
        ],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();
    let state = StateSsa::analyze(&eu, &cfg, STABLE_REGION, None).unwrap();
    let first_slot = state
        .slots
        .iter()
        .position(|slot| slot.fragment.addr == first)
        .unwrap();
    let second_slot = state
        .slots
        .iter()
        .position(|slot| slot.fragment.addr == second)
        .unwrap();

    assert!(state.slots[first_slot].has_kill);
    assert!(!state.slots[second_slot].has_kill);
    assert!(matches!(
        use_access(&state, RegisterId(3)).kind,
        MemoryAccessKind::Use { reaching, .. }
            if matches!(
                state.accesses[reaching.0].kind,
                MemoryAccessKind::Kill { .. }
            )
    ));
    let MemoryAccessKind::Use { reaching, .. } = use_access(&state, RegisterId(3)).kind else {
        unreachable!()
    };
    let MemoryAccessKind::Kill {
        reaching: before_kill,
    } = state.accesses[reaching.0].kind
    else {
        unreachable!()
    };
    assert!(matches!(
        state.accesses[before_kill.0].kind,
        MemoryAccessKind::Def { .. }
    ));
    assert!(matches!(
        use_access(&state, RegisterId(5)).kind,
        MemoryAccessKind::Use { reaching, .. }
            if matches!(state.accesses[reaching.0].kind, MemoryAccessKind::Def { .. })
    ));
}

#[test]
fn disjoint_fragments_remain_independent() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![block(
            0,
            vec![
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    stable,
                    SIROffset::Static(8),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(3), stable, SIROffset::Static(8), 8),
            ],
            SIRTerminator::Return,
        )],
        (0..4).map(|register| (RegisterId(register), bit(8))),
    );
    let cfg = SirCfg::analyze(&eu).unwrap();
    let state = StateSsa::analyze(&eu, &cfg, STABLE_REGION, None).unwrap();

    assert_eq!(state.slots.len(), 2);
    assert!(state.slots.iter().all(|slot| !slot.has_kill));
    assert_ne!(state.slots[0].fragment, state.slots[1].fragment);
}

#[test]
fn loop_carried_state_gets_a_header_phi() {
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![
            block(
                0,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(1), Vec::new()),
            ),
            block(
                1,
                vec![SIRInstruction::Load(
                    RegisterId(2),
                    stable,
                    SIROffset::Static(0),
                    8,
                )],
                SIRTerminator::Branch {
                    cond: RegisterId(3),
                    true_block: (BlockId(2), Vec::new()),
                    false_block: (BlockId(3), Vec::new()),
                },
            ),
            block(
                2,
                vec![SIRInstruction::Store(
                    stable,
                    SIROffset::Static(0),
                    8,
                    RegisterId(1),
                    Vec::new(),
                    Vec::new(),
                )],
                SIRTerminator::Jump(BlockId(1), Vec::new()),
            ),
            block(3, Vec::new(), SIRTerminator::Return),
        ],
        [
            (RegisterId(0), bit(8)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(8)),
            (RegisterId(3), bit(1)),
        ],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();
    let state = StateSsa::analyze(&eu, &cfg, STABLE_REGION, None).unwrap();

    assert_eq!(state.slots[0].phi_blocks, [cfg.index[&BlockId(1)]]);
    let phi = state
        .accesses
        .iter()
        .find(|access| {
            access.block == Some(BlockId(1)) && matches!(access.kind, MemoryAccessKind::Phi { .. })
        })
        .unwrap();
    assert!(matches!(
        use_access(&state, RegisterId(2)).kind,
        MemoryAccessKind::Use { reaching, .. } if reaching == phi.id
    ));
}

#[test]
fn four_state_fragment_keeps_value_and_mask_atomic() {
    let working = address(WORKING_REGION, 0);
    let stable = address(STABLE_REGION, 0);
    let eu = unit(
        vec![block(
            0,
            vec![
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    vec![TriggerIdWithKind {
                        kind: DomainKind::Other,
                        id: 0,
                    }],
                    Vec::new(),
                ),
                SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(2), working, SIROffset::Static(0), 8),
            ],
            SIRTerminator::Return,
        )],
        [
            (RegisterId(0), RegisterType::Logic { width: 8 }),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
            (RegisterId(2), RegisterType::Logic { width: 8 }),
        ],
    );
    let cfg = SirCfg::analyze(&eu).unwrap();
    let state = StateSsa::analyze(&eu, &cfg, WORKING_REGION, None).unwrap();

    assert_eq!(state.slots.len(), 1);
    assert_eq!(
        state.slots[0].fragment.plane,
        StatePlane::FourStateValueAndMask
    );
    assert!(state.slots[0].has_effectful_store);
    assert!(
        !state.slots[0].escapes,
        "a stable-region read is independent"
    );
}
