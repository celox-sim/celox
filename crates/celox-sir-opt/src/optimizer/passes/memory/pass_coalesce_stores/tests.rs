use super::*;

#[test]
fn coalescing_preserves_truncation_of_wide_sources() {
    for source_width in [8, 64, 129] {
        let registers = [RegisterId(0), RegisterId(1)]
            .into_iter()
            .map(|reg| {
                (
                    reg,
                    RegisterType::Logic {
                        width: source_width,
                    },
                )
            })
            .collect();
        let mut eu = make_eu(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xffu8)),
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xfeu8)),
                SIRInstruction::Store(
                    make_addr(1),
                    SIROffset::Static(0),
                    1,
                    RegisterId(0),
                    vec![],
                    vec![],
                ),
                SIRInstruction::Store(
                    make_addr(1),
                    SIROffset::Static(1),
                    2,
                    RegisterId(1),
                    vec![],
                    vec![],
                ),
            ],
            registers,
        );
        eu.verify_result().unwrap();
        CoalesceStoresPass::default().run(&mut eu, &PassOptions::default());
        eu.verify_result().unwrap();
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        let stores = instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Store(..)))
            .count();
        assert_eq!(stores, 1);
        let args = instructions
            .iter()
            .find_map(|inst| match inst {
                SIRInstruction::Concat(_, args) => Some(args),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            args.iter()
                .map(|arg| eu.register_map[arg].width())
                .collect::<Vec<_>>(),
            [2, 1]
        );
    }
}

fn make_var_id(n: u32) -> celox_design::StateObjectId {
    celox_design::StateObjectId(n)
}

fn make_addr(var_id: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr {
        region: 0,
        instance_id: InstanceId(0),
        var_id: make_var_id(var_id),
    }
}

fn make_eu(
    instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: HashMap<RegisterId, RegisterType>,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut blocks = HashMap::default();
    blocks.insert(
        BlockId(0),
        BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        },
    );
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    }
}

#[test]
fn overlapping_store_seals_the_open_run() {
    let addr = make_addr(0);
    for middle in [
        SIRInstruction::Store(addr, SIROffset::Static(3), 1, RegisterId(1), vec![], vec![]),
        SIRInstruction::Commit(make_addr(1), addr, SIROffset::Static(3), 1, vec![]),
    ] {
        let instructions = vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
            SIRInstruction::Store(addr, SIROffset::Static(2), 2, RegisterId(0), vec![], vec![]),
            middle,
            SIRInstruction::Store(addr, SIROffset::Static(1), 1, RegisterId(1), vec![], vec![]),
        ];
        let registers = [
            (RegisterId(0), RegisterType::Logic { width: 2 }),
            (RegisterId(1), RegisterType::Logic { width: 1 }),
        ]
        .into_iter()
        .collect();
        let mut eu = make_eu(instructions.clone(), registers);
        CoalesceStoresPass::default().run(&mut eu, &PassOptions::default());
        assert_eq!(eu.blocks[&BlockId(0)].instructions, instructions);
        eu.verify();
    }
}

#[test]
fn test_basic_contiguous_coalescing() {
    // 3 contiguous 1-bit stores to bits 0, 1, 2 of the same variable
    let addr = make_addr(0);
    let mut register_map = HashMap::default();
    for i in 0..3 {
        register_map.insert(
            RegisterId(i),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(1),
            1,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(2),
            1,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        ),
    ];

    let mut eu = make_eu(instructions, register_map);
    let options = PassOptions::default();
    CoalesceStoresPass::default().run(&mut eu, &options);

    let block = eu.blocks.get(&BlockId(0)).unwrap();
    // Should be 2 instructions: Concat + wide Store
    assert_eq!(block.instructions.len(), 2);
    match &block.instructions[0] {
        SIRInstruction::Concat(dst, args) => {
            assert_eq!(args.len(), 3);
            // MSB first: r2, r1, r0
            assert_eq!(args[0], RegisterId(2));
            assert_eq!(args[1], RegisterId(1));
            assert_eq!(args[2], RegisterId(0));
            // Verify the concat register has correct width
            let reg_type = eu.register_map.get(dst).unwrap();
            assert_eq!(reg_type.width(), 3);
        }
        other => panic!("Expected Concat, got {:?}", other),
    }
    match &block.instructions[1] {
        SIRInstruction::Store(_, SIROffset::Static(0), 3, _, triggers, _) => {
            assert!(triggers.is_empty());
        }
        other => panic!("Expected wide Store, got {:?}", other),
    }
}

#[test]
fn partitions_a_long_run_at_the_configured_memory_width() {
    let addr = make_addr(0);
    let register_map = (0..4)
        .map(|index| {
            (
                RegisterId(index),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            )
        })
        .collect();
    let instructions = (0..4)
        .map(|index| {
            SIRInstruction::Store(
                addr,
                SIROffset::Static(index * 64),
                64,
                RegisterId(index),
                vec![],
                vec![],
            )
        })
        .collect();
    let mut eu = make_eu(instructions, register_map);

    CoalesceStoresPass {
        element_widths: Arc::default(),
        max_store_width: 128,
    }
    .run(&mut eu, &PassOptions::default());

    let widths = eu.blocks[&BlockId(0)]
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(_, _, width, ..) => Some(*width),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(widths, vec![128, 128]);
}

#[test]
fn test_interleaved_stores_different_vars() {
    // Stores to var0 interleaved with stores to var1 — both should coalesce
    let addr0 = make_addr(0);
    let addr1 = make_addr(1);
    let mut register_map = HashMap::default();
    for i in 0..4 {
        register_map.insert(
            RegisterId(i),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let instructions = vec![
        SIRInstruction::Store(
            addr0,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr1,
            SIROffset::Static(0),
            1,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr0,
            SIROffset::Static(1),
            1,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr1,
            SIROffset::Static(1),
            1,
            RegisterId(3),
            Vec::new(),
            Vec::new(),
        ),
    ];

    let mut eu = make_eu(instructions, register_map);
    let options = PassOptions::default();
    CoalesceStoresPass::default().run(&mut eu, &options);

    let block = eu.blocks.get(&BlockId(0)).unwrap();
    // 4 instructions: Concat+Store for addr0, Concat+Store for addr1
    assert_eq!(block.instructions.len(), 4);
}

#[test]
fn test_seal_on_load() {
    // Store bit 0, Load from same addr, Store bit 1 — should NOT coalesce
    let addr = make_addr(0);
    let mut register_map = HashMap::default();
    for i in 0..3 {
        register_map.insert(
            RegisterId(i),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 1),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(1),
            1,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
    ];

    let mut eu = make_eu(instructions, register_map);
    let options = PassOptions::default();
    CoalesceStoresPass::default().run(&mut eu, &options);

    let block = eu.blocks.get(&BlockId(0)).unwrap();
    // No coalescing should happen — 3 original instructions remain
    assert_eq!(block.instructions.len(), 3);
}

#[test]
fn test_non_contiguous_stores_not_coalesced() {
    // Stores at offset 0 and offset 2 (gap at 1) — should NOT coalesce
    let addr = make_addr(0);
    let mut register_map = HashMap::default();
    for i in 0..2 {
        register_map.insert(
            RegisterId(i),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(2),
            1,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
    ];

    let mut eu = make_eu(instructions, register_map);
    let options = PassOptions::default();
    CoalesceStoresPass::default().run(&mut eu, &options);

    let block = eu.blocks.get(&BlockId(0)).unwrap();
    assert_eq!(block.instructions.len(), 2);
}

#[test]
fn test_stores_with_triggers_not_coalesced() {
    let addr = make_addr(0);
    let mut register_map = HashMap::default();
    for i in 0..2 {
        register_map.insert(
            RegisterId(i),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let trigger = TriggerIdWithKind {
        id: 0,
        kind: DomainKind::ClockPosedge,
    };

    let instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            vec![trigger],
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(1),
            1,
            RegisterId(1),
            vec![trigger],
            Vec::new(),
        ),
    ];

    let mut eu = make_eu(instructions, register_map);
    let options = PassOptions::default();
    CoalesceStoresPass::default().run(&mut eu, &options);

    let block = eu.blocks.get(&BlockId(0)).unwrap();
    // Triggered stores should not be coalesced
    assert_eq!(block.instructions.len(), 2);
}

#[test]
fn test_partial_contiguous_run() {
    // 4 stores: bits 0,1,2 are contiguous, bit 5 is separate
    let addr = make_addr(0);
    let mut register_map = HashMap::default();
    for i in 0..4 {
        register_map.insert(
            RegisterId(i),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
    }

    let instructions = vec![
        SIRInstruction::Store(
            addr,
            SIROffset::Static(0),
            1,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(1),
            1,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(2),
            1,
            RegisterId(2),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Store(
            addr,
            SIROffset::Static(5),
            1,
            RegisterId(3),
            Vec::new(),
            Vec::new(),
        ),
    ];

    let mut eu = make_eu(instructions, register_map);
    let options = PassOptions::default();
    CoalesceStoresPass::default().run(&mut eu, &options);

    let block = eu.blocks.get(&BlockId(0)).unwrap();
    // 3 instructions: Concat + wide Store (for bits 0-2) + original Store(bit 5)
    assert_eq!(block.instructions.len(), 3);
}

#[test]
fn test_unpacked_cross_element_run_uses_packed_elements() {
    let addr = make_addr(0);
    let register_map = (0..4)
        .map(|index| {
            (
                RegisterId(index),
                RegisterType::Bit {
                    width: 6,
                    signed: false,
                },
            )
        })
        .collect();
    let instructions = (0..4)
        .map(|index| {
            SIRInstruction::Store(
                addr,
                SIROffset::Static(index * 6),
                6,
                RegisterId(index),
                vec![],
                vec![],
            )
        })
        .collect();
    let mut eu = make_eu(instructions, register_map);
    let pass = CoalesceStoresPass {
        element_widths: Arc::new([(addr, 12usize)].into_iter().collect()),
        max_store_width: 64,
    };

    pass.run(&mut eu, &PassOptions::default());

    let stores = eu.blocks[&BlockId(0)]
        .instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(
                _,
                SIROffset::PackedElements {
                    bit_offset,
                    element_width,
                },
                width,
                ..,
            ) => Some((*bit_offset, *element_width, *width)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores, vec![(0, 12, 24)]);
}
