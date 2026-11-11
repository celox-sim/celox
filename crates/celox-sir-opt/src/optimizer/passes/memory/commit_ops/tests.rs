use super::*;
use celox_design::StateObjectId as VarId;
use num_bigint::BigUint;

fn addr(region: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region,
        instance_id: InstanceId(0),
        var_id: VarId::from_raw(0),
    }
}

fn forwarding_eu(read_old_after: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let stable = addr(STABLE_REGION);
    let working = addr(WORKING_REGION);
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Store(
                    working,
                    SIROffset::Static(0),
                    8,
                    RegisterId(0),
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
            ],
            terminator: if read_old_after {
                SIRTerminator::Jump(BlockId(1), Vec::new())
            } else {
                SIRTerminator::Return
            },
        },
    );
    if read_old_after {
        blocks.insert(
            BlockId(1),
            BasicBlock {
                id: BlockId(1),
                params: Vec::new(),
                instructions: vec![SIRInstruction::Load(
                    RegisterId(1),
                    stable,
                    SIROffset::Static(0),
                    8,
                )],
                terminator: SIRTerminator::Return,
            },
        );
    }
    let mut register_map = HashMap::default();
    register_map.insert(RegisterId(0), RegisterType::Logic { width: 8 });
    register_map.insert(RegisterId(1), RegisterType::Logic { width: 8 });
    ExecutionUnit {
        blocks,
        entry_block_id: BlockId(0),
        register_map,
    }
}

#[test]
fn forwarding_remains_enabled_when_no_old_stable_read_follows() {
    let mut eu = forwarding_eu(false);
    let hazards = direct_stable_store_hazards(&eu);
    assert!(hazards.is_empty());
    inline_commit_forwarding_with_hazards(&mut eu, &hazards);
    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(matches!(
        instructions.as_slice(),
        [SIRInstruction::Store(addr, ..)] if addr.region == STABLE_REGION
    ));
}

#[test]
fn forwarding_crosses_a_stable_read_after_publication() {
    let mut eu = forwarding_eu(true);
    let hazards = direct_stable_store_hazards(&eu);
    assert!(hazards.is_empty());
    inline_commit_forwarding_with_hazards(&mut eu, &hazards);
    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(matches!(
        instructions.as_slice(),
        [SIRInstruction::Store(a, ..)] if a.region == STABLE_REGION
    ));
}

#[test]
fn forwarding_preserves_working_commit_when_old_stable_is_read_before_it() {
    let stable = addr(STABLE_REGION);
    let working = addr(WORKING_REGION);
    let mut eu = forwarding_eu(false);
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions.insert(
        1,
        SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
    );

    let hazards = direct_stable_store_hazards(&eu);
    assert!(hazards.overlaps(stable.absolute_addr(), 0, 8));
    inline_commit_forwarding_with_hazards(&mut eu, &hazards);
    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(matches!(instructions[0], SIRInstruction::Store(a, ..) if a == working));
    assert!(matches!(instructions[2], SIRInstruction::Commit(..)));
}

#[test]
fn forwarding_ignores_a_later_read_of_a_disjoint_bit_range() {
    let stable = addr(STABLE_REGION);
    let mut eu = forwarding_eu(false);
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions.insert(
        1,
        SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(8), 8),
    );

    let hazards = direct_stable_store_hazards(&eu);
    assert!(!hazards.overlaps(addr(STABLE_REGION).absolute_addr(), 0, 8));
    inline_commit_forwarding_with_hazards(&mut eu, &hazards);
    let instructions = &eu.blocks[&BlockId(0)].instructions;
    assert!(
        matches!(instructions[0], SIRInstruction::Store(addr, ..) if addr.region == STABLE_REGION)
    );
    assert!(matches!(instructions[1], SIRInstruction::Load(..)));
}

#[test]
fn sparse_write_makes_an_old_stable_read_hazardous_until_publication() {
    let stable = addr(STABLE_REGION);
    let sparse = addr(SPARSE_WORKING_REGION);
    let mut eu = forwarding_eu(false);
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
        SIRInstruction::Store(
            sparse,
            SIROffset::Element {
                index: RegisterId(1),
                element_width: 8,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 8),
        SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 64, Vec::new()),
    ];
    eu.register_map
        .insert(RegisterId(2), RegisterType::Logic { width: 8 });

    let hazards = direct_stable_store_hazards(&eu);
    assert!(hazards.contains_addr(stable.absolute_addr()));
}

#[test]
fn sparse_publication_closes_the_old_stable_read_interval() {
    let stable = addr(STABLE_REGION);
    let sparse = addr(SPARSE_WORKING_REGION);
    let mut eu = forwarding_eu(false);
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
        SIRInstruction::Store(
            sparse,
            SIROffset::Element {
                index: RegisterId(1),
                element_width: 8,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 64, Vec::new()),
        SIRInstruction::Load(RegisterId(2), stable, SIROffset::Static(0), 8),
    ];
    eu.register_map
        .insert(RegisterId(2), RegisterType::Logic { width: 8 });

    assert!(direct_stable_store_hazards(&eu).is_empty());
}

#[test]
fn stable_write_competing_with_an_unpublished_sparse_write_is_hazardous() {
    let stable = addr(STABLE_REGION);
    let sparse = addr(SPARSE_WORKING_REGION);
    let mut eu = forwarding_eu(false);
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions = vec![
        SIRInstruction::Store(
            sparse,
            SIROffset::Static(0),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            stable,
            SIROffset::Static(0),
            8,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Commit(sparse, stable, SIROffset::Static(0), 64, Vec::new()),
    ];

    assert!(direct_stable_store_hazards(&eu).overlaps(stable.absolute_addr(), 0, 8));
}

#[test]
fn switch_predecessor_prevents_commit_sinking() {
    let stable = addr(STABLE_REGION);
    let working = addr(WORKING_REGION);
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Switch {
                selector: RegisterId(0),
                cases: vec![SIRSwitchCase {
                    value: BigUint::from(0u8),
                    target: BlockId(2),
                }],
                default: BlockId(1),
            },
        },
    );
    blocks.insert(
        BlockId(1),
        BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Store(
                working,
                SIROffset::Static(0),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            )],
            terminator: SIRTerminator::Jump(BlockId(2), Vec::new()),
        },
    );
    let commit = SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new());
    blocks.insert(
        BlockId(2),
        BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: vec![commit.clone()],
            terminator: SIRTerminator::Return,
        },
    );
    let mut eu = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map: [
            (
                RegisterId(0),
                RegisterType::Bit {
                    width: 1,
                    signed: false,
                },
            ),
            (RegisterId(1), RegisterType::Logic { width: 8 }),
        ]
        .into_iter()
        .collect(),
    };

    optimize_commit_sinking(&mut eu);

    assert_eq!(eu.blocks[&BlockId(2)].instructions, vec![commit]);
    assert!(matches!(
        eu.blocks[&BlockId(1)].instructions.as_slice(),
        [SIRInstruction::Store(address, ..)] if address.region == WORKING_REGION
    ));
}

/// Whether inline forwarding still publishes the staged store directly
/// when a call between it and its publication reads `reads` and writes
/// `writes`.
fn forwards_across_call(
    reads: Vec<RegionedAbsoluteAddr>,
    writes: Vec<RegionedAbsoluteAddr>,
) -> bool {
    let mut eu = forwarding_eu(false);
    eu.blocks.get_mut(&BlockId(0)).unwrap().instructions.insert(
        1,
        SIRInstruction::Call {
            function: celox_sir::FunctionId(0),
            frame: RegisterId(0),
            args: Vec::new(),
            outputs: Vec::new(),
            dst: None,
            effects: std::sync::Arc::new(celox_sir::CallEffects {
                reads,
                writes,
                ..Default::default()
            }),
        },
    );
    let hazards = direct_stable_store_hazards(&eu);
    inline_commit_forwarding_with_hazards(&mut eu, &hazards);
    !eu.blocks[&BlockId(0)]
        .instructions
        .iter()
        .any(|instruction| matches!(instruction, SIRInstruction::Commit(..)))
}

#[test]
fn a_call_observing_or_replacing_staged_state_keeps_its_publication() {
    let (stable, working) = (addr(STABLE_REGION), addr(WORKING_REGION));
    // Reading the old STABLE value, or writing it, observes the early
    // publication; reading or writing the staged value sees what the
    // redirected store no longer stages.
    assert!(!forwards_across_call(vec![stable], Vec::new()));
    assert!(!forwards_across_call(Vec::new(), vec![stable]));
    assert!(!forwards_across_call(vec![working], Vec::new()));
    assert!(!forwards_across_call(Vec::new(), vec![working]));
    let other = RegionedAbsoluteAddr {
        var_id: VarId::from_raw(1),
        ..stable
    };
    assert!(forwards_across_call(vec![other], vec![other]));
}
