use super::{
    AvailableStaticLoad, AvailableStaticLoads, KnownAddressValues, MAX_SCALAR_COALESCED_STORE_BITS,
    aggregate_static_offset, coalesce_static_loads as coalesce_static_loads_with_types,
    coalesce_static_stores as coalesce_static_stores_with_types, optimize_block,
    schedule_instructions, subsume_static_loads as subsume_static_loads_with_types,
};
use crate::HashMap;
use crate::ir::{
    BasicBlock, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue,
};

fn logic(width: usize) -> RegisterType {
    RegisterType::Logic { width }
}

#[test]
fn store_coalescing_preserves_each_sources_truncated_width() {
    for source_width in [8, 64, 129] {
        let mut instructions = vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(0xffu8)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(0xfeu8)),
            SIRInstruction::Store(7u32, SIROffset::Static(0), 1, RegisterId(0), vec![], vec![]),
            SIRInstruction::Store(7, SIROffset::Static(1), 2, RegisterId(1), vec![], vec![]),
        ];
        let mut registers: HashMap<_, _> = [
            (RegisterId(0), logic(source_width)),
            (RegisterId(1), logic(source_width)),
        ]
        .into_iter()
        .collect();
        verify(instructions.clone(), registers.clone());
        let mut counter = 1;
        assert!(coalesce_static_stores_with_types(
            &mut instructions,
            &mut registers,
            &mut counter,
            &HashMap::default()
        ));
        // Each Store originally discards high source bits. In particular,
        // Concat's operand widths must total three, not 2 * source_width.
        verify(instructions.clone(), registers.clone());
        let args = instructions
            .iter()
            .find_map(|inst| match inst {
                SIRInstruction::Concat(_, args) => Some(args),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            args.iter()
                .map(|arg| registers[arg].width())
                .collect::<Vec<_>>(),
            [2, 1]
        );
    }
}

fn subsume_static_loads(instructions: &mut [SIRInstruction<u32>]) -> bool {
    let register_map = instructions
        .iter()
        .filter_map(|inst| match inst {
            SIRInstruction::Load(dst, _, _, width) => Some((*dst, logic(*width))),
            SIRInstruction::Imm(dst, _) => Some((
                *dst,
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            )),
            _ => None,
        })
        .collect();
    subsume_static_loads_with_types(instructions, &register_map)
}

#[test]
fn high_fanout_schedule_preserves_repeated_data_and_memory_dependencies() {
    let mut instructions = vec![SIRInstruction::Load(
        RegisterId(0),
        7u32,
        SIROffset::Static(0),
        8,
    )];
    for register in 1..4097 {
        instructions.push(SIRInstruction::Binary(
            RegisterId(register),
            RegisterId(0),
            crate::ir::BinaryOp::Add,
            RegisterId(0),
        ));
    }
    // This edge is discovered both as a data dependency and as WAR.
    instructions.push(SIRInstruction::Store(
        7,
        SIROffset::Static(0),
        8,
        RegisterId(0),
        Vec::new(),
        Vec::new(),
    ));
    instructions.push(SIRInstruction::Load(
        RegisterId(4097),
        7,
        SIROffset::Static(0),
        8,
    ));
    let original_len = instructions.len();
    schedule_instructions(&mut instructions, 8);
    assert_eq!(instructions.len(), original_len);
    let load_before = instructions
        .iter()
        .position(|inst| matches!(inst, SIRInstruction::Load(RegisterId(0), ..)))
        .unwrap();
    let store = instructions
        .iter()
        .position(|inst| matches!(inst, SIRInstruction::Store(..)))
        .unwrap();
    let load_after = instructions
        .iter()
        .position(|inst| matches!(inst, SIRInstruction::Load(RegisterId(4097), ..)))
        .unwrap();
    assert!(load_before < store && store < load_after);
    for (index, instruction) in instructions.iter().enumerate() {
        if matches!(instruction, SIRInstruction::Binary(..)) {
            assert!(load_before < index);
        }
    }
}

#[test]
fn rescheduling_preserves_store_and_bounded_load_priority() {
    let mut instructions = vec![
        SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
        SIRInstruction::Load(RegisterId(1), 1u32, SIROffset::Static(0), 8),
        SIRInstruction::Store(
            2u32,
            SIROffset::Static(0),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Unary(RegisterId(2), crate::ir::UnaryOp::Ident, RegisterId(1)),
        SIRInstruction::Load(RegisterId(3), 3u32, SIROffset::Static(0), 8),
    ];

    schedule_instructions(&mut instructions, 1);

    assert!(matches!(
        instructions[0],
        SIRInstruction::Load(RegisterId(1), ..)
    ));
    assert!(matches!(
        instructions[1],
        SIRInstruction::Imm(RegisterId(0), ..)
    ));
    assert!(matches!(instructions[2], SIRInstruction::Store(2, ..)));
    assert!(matches!(
        instructions[3],
        SIRInstruction::Unary(RegisterId(2), _, RegisterId(1))
    ));
    assert!(matches!(
        instructions[4],
        SIRInstruction::Load(RegisterId(3), ..)
    ));
}

#[test]
fn known_values_invalidate_only_overlapping_static_ranges() {
    let mut values = KnownAddressValues::default();
    values.insert(SIROffset::Static(0), (RegisterId(0), 128));
    values.insert(SIROffset::Static(160), (RegisterId(1), 32));
    values.insert(SIROffset::Dynamic(RegisterId(9)), (RegisterId(2), 32));

    values.invalidate_store(&SIROffset::Static(96), 32);

    assert_eq!(values.get(&SIROffset::Static(0)), None);
    assert_eq!(
        values.get(&SIROffset::Static(160)),
        Some((RegisterId(1), 32))
    );
    assert_eq!(values.get(&SIROffset::Dynamic(RegisterId(9))), None);
}

#[test]
fn dynamic_store_invalidates_every_known_range() {
    let mut values = KnownAddressValues::default();
    values.insert(SIROffset::Static(0), (RegisterId(0), 32));
    values.insert(
        SIROffset::PackedElements {
            bit_offset: 32,
            element_width: 8,
        },
        (RegisterId(1), 32),
    );

    values.invalidate_store(&SIROffset::Dynamic(RegisterId(9)), 32);

    assert_eq!(values.get(&SIROffset::Static(0)), None);
    assert_eq!(
        values.get(&SIROffset::PackedElements {
            bit_offset: 32,
            element_width: 8,
        }),
        None
    );
    assert_eq!(values.max_static_width, 0);
}

#[test]
fn available_load_index_matches_naive_range_updates() {
    let original = (0..12usize)
        .map(|index| {
            let start = index * 11 % 70;
            let width = index * 13 % 31 + 1;
            AvailableStaticLoad {
                dst: RegisterId(index),
                load_offset: start,
                load_width: width,
                valid_start: start,
                valid_end: start + width,
            }
        })
        .collect::<Vec<_>>();
    let sort = |loads: &mut Vec<AvailableStaticLoad>| {
        loads.sort_by_key(|load| {
            (
                load.dst,
                load.valid_start,
                load.valid_end,
                load.load_offset,
                load.load_width,
            )
        });
    };

    let mut untouched = AvailableStaticLoads::default();
    for &load in &original {
        untouched.insert(load);
    }
    for start in 0..96 {
        for width in 0..24 {
            let end = start + width;
            let mut expected = original
                .iter()
                .filter(|load| load.valid_start <= start && end <= load.valid_end)
                .copied()
                .collect::<Vec<_>>();
            let mut actual = untouched
                .containing(start, end)
                .copied()
                .collect::<Vec<_>>();
            sort(&mut expected);
            sort(&mut actual);
            assert_eq!(actual, expected, "containing range {start}..{end}");
        }
    }

    for write_start in 0..96 {
        for write_width in 0..24 {
            let write_end = write_start + write_width;
            let mut expected = Vec::new();
            for &load in &original {
                if write_start >= write_end
                    || write_start >= load.valid_end
                    || load.valid_start >= write_end
                {
                    expected.push(load);
                    continue;
                }
                if load.valid_start < write_start {
                    expected.push(AvailableStaticLoad {
                        valid_end: write_start,
                        ..load
                    });
                }
                if write_end < load.valid_end {
                    expected.push(AvailableStaticLoad {
                        valid_start: write_end,
                        ..load
                    });
                }
            }

            let mut indexed = AvailableStaticLoads::default();
            for &load in &original {
                indexed.insert(load);
            }
            indexed.subtract_write(write_start, write_end);
            let mut actual = indexed
                .by_valid_start
                .into_values()
                .flatten()
                .collect::<Vec<_>>();
            sort(&mut expected);
            sort(&mut actual);
            assert_eq!(
                actual, expected,
                "subtracting write range {write_start}..{write_end}"
            );
        }
    }
}

fn verify(
    instructions: Vec<SIRInstruction<u32>>,
    registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
) {
    let block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions,
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: [(BlockId(0), block)].into_iter().collect(),
        register_map: registers.into_iter().collect(),
    };
    unit.verify_result().unwrap();
}

#[test]
fn many_contained_static_loads_become_slices_of_one_full_load() {
    let mut instructions = vec![SIRInstruction::Load(
        RegisterId(0),
        7,
        SIROffset::Static(0),
        128,
    )];
    let mut registers = vec![(RegisterId(0), logic(128))];
    for index in 0..1_018usize {
        let width = index % 127 + 1;
        let dst = RegisterId(index + 1);
        instructions.push(SIRInstruction::Load(dst, 7, SIROffset::Static(0), width));
        registers.push((dst, logic(width)));
    }

    assert!(subsume_static_loads(&mut instructions));
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        1
    );
    assert_eq!(
        instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Slice(..)))
            .count(),
        1_018
    );
    verify(instructions, registers);
}

#[test]
fn covering_wide_load_does_not_disable_word_load_coalescing() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 128),
        SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(65), 3),
        SIRInstruction::Load(RegisterId(2), 7, SIROffset::Static(66), 2),
    ];
    let mut register_map = [
        (RegisterId(0), logic(128)),
        (RegisterId(1), logic(3)),
        (RegisterId(2), logic(2)),
    ]
    .into_iter()
    .collect();
    let mut reg_counter = 2;

    coalesce_static_loads_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        false,
        &HashMap::default(),
    );

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
            .count(),
        2
    );
    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Load(_, 7, SIROffset::Static(64), 64)
    )));
    assert!(instructions.iter().all(|instruction| !matches!(
        instruction,
        SIRInstruction::Load(RegisterId(1) | RegisterId(2), _, _, _)
    )));
    verify(instructions, register_map);
}

#[test]
fn scalar_partial_load_coalescing_stays_within_the_accessed_span() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 16),
        SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(16), 2),
        SIRInstruction::Load(RegisterId(2), 7, SIROffset::Static(18), 14),
    ];
    let mut register_map = [
        (RegisterId(0), logic(16)),
        (RegisterId(1), logic(2)),
        (RegisterId(2), logic(14)),
    ]
    .into_iter()
    .collect();
    let mut reg_counter = 2;

    coalesce_static_loads_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        false,
        &HashMap::default(),
    );

    assert!(instructions.iter().any(|instruction| matches!(
        instruction,
        SIRInstruction::Load(_, 7, SIROffset::Static(0), 32)
    )));
    assert!(instructions.iter().all(|instruction| !matches!(
        instruction,
        SIRInstruction::Load(_, 7, SIROffset::Static(0), 64)
    )));
    verify(instructions, register_map);
}

#[test]
fn unpacked_array_load_coalescing_uses_explicit_packed_elements() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 12),
        SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(12), 12),
    ];
    let mut register_map = [(RegisterId(0), logic(12)), (RegisterId(1), logic(12))]
        .into_iter()
        .collect();
    let mut reg_counter = 1;

    coalesce_static_loads_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        false,
        &[(7u32, 12usize)].into_iter().collect(),
    );

    assert_eq!(
        instructions,
        vec![
            SIRInstruction::Load(
                RegisterId(2),
                7,
                SIROffset::PackedElements {
                    bit_offset: 0,
                    element_width: 12,
                },
                24,
            ),
            SIRInstruction::Slice(RegisterId(0), RegisterId(2), 0, 12),
            SIRInstruction::Slice(RegisterId(1), RegisterId(2), 12, 12),
        ]
    );
    verify(instructions, register_map);
}

#[test]
fn four_state_load_coalescing_extracts_with_slices() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 5),
        SIRInstruction::Load(RegisterId(1), 7, SIROffset::Static(4), 1),
    ];
    let mut register_map = [(RegisterId(0), logic(5)), (RegisterId(1), logic(1))]
        .into_iter()
        .collect();
    let mut reg_counter = 1;

    coalesce_static_loads_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        true,
        &HashMap::default(),
    );

    assert_eq!(
        instructions
            .iter()
            .filter(|instruction| matches!(instruction, SIRInstruction::Slice(..)))
            .count(),
        2
    );
    assert!(
        instructions
            .iter()
            .all(|instruction| !matches!(instruction, SIRInstruction::Binary(..)))
    );
    verify(instructions, register_map);
}

#[test]
fn naturally_aligned_native_loads_skip_coalescing() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 7u32, SIROffset::Static(0), 32),
        SIRInstruction::Load(RegisterId(1), 7u32, SIROffset::Static(32), 32),
    ];
    let original = instructions.clone();
    let mut register_map = [(RegisterId(0), logic(32)), (RegisterId(1), logic(32))]
        .into_iter()
        .collect();
    let mut reg_counter = 1;

    coalesce_static_loads_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        false,
        &HashMap::default(),
    );

    assert_eq!(instructions, original);
    assert_eq!(reg_counter, 1);
}

#[test]
fn store_coalescing_preserves_intervening_overlapping_writes() {
    for intervening_offset in [
        SIROffset::Static(3),
        SIROffset::PackedElements {
            bit_offset: 3,
            element_width: 1,
        },
        SIROffset::Dynamic(RegisterId(2)),
        SIROffset::Element {
            index: RegisterId(2),
            element_width: 2,
            bit_offset: 1,
            dynamic_bit_offset: None,
        },
    ] {
        let mut instructions = vec![
            SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
            SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
            SIRInstruction::Imm(RegisterId(2), SIRValue::new(1u8)),
            SIRInstruction::Store(7u32, SIROffset::Static(2), 2, RegisterId(0), vec![], vec![]),
            SIRInstruction::Store(7, intervening_offset, 1, RegisterId(1), vec![], vec![]),
            SIRInstruction::Store(7, SIROffset::Static(1), 1, RegisterId(1), vec![], vec![]),
        ];
        let original = instructions.clone();
        let mut registers = [
            (RegisterId(0), logic(2)),
            (RegisterId(1), logic(1)),
            (RegisterId(2), logic(32)),
        ]
        .into_iter()
        .collect();
        let mut counter = 2;
        // Merging the writes at bits 1 and 2..3 would move the latter
        // past the intervening write of bit 3, erasing its new value.
        assert!(!coalesce_static_stores_with_types(
            &mut instructions,
            &mut registers,
            &mut counter,
            &HashMap::default()
        ));
        assert_eq!(instructions, original);
    }
}

#[test]
fn store_coalescing_keeps_disjoint_intervening_writes_optimizable() {
    let mut instructions = vec![
        SIRInstruction::Imm(RegisterId(0), SIRValue::new(0u8)),
        SIRInstruction::Imm(RegisterId(1), SIRValue::new(1u8)),
        SIRInstruction::Store(7u32, SIROffset::Static(2), 2, RegisterId(0), vec![], vec![]),
        SIRInstruction::Store(7, SIROffset::Static(5), 1, RegisterId(1), vec![], vec![]),
        SIRInstruction::Store(7, SIROffset::Static(1), 1, RegisterId(1), vec![], vec![]),
    ];
    let mut registers = [(RegisterId(0), logic(2)), (RegisterId(1), logic(1))]
        .into_iter()
        .collect();
    let mut counter = 1;
    assert!(coalesce_static_stores_with_types(
        &mut instructions,
        &mut registers,
        &mut counter,
        &HashMap::default()
    ));
    verify(instructions, registers);
}

#[test]
fn store_coalescing_marks_cross_element_transfer_explicitly() {
    let mut instructions = (0..4)
        .map(|index| SIRInstruction::Imm(RegisterId(index), SIRValue::new(index as u8)))
        .chain((0..4).map(|index| {
            SIRInstruction::Store(
                7u32,
                SIROffset::Static(index * 6),
                6,
                RegisterId(index),
                vec![],
                vec![],
            )
        }))
        .collect::<Vec<_>>();
    let mut register_map = (0..4).map(|index| (RegisterId(index), logic(6))).collect();
    let mut reg_counter = 3;

    assert!(coalesce_static_stores_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        &[(7u32, 12usize)].into_iter().collect(),
    ));

    let stores = instructions
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
    verify(instructions, register_map);
}

#[test]
fn naturally_aligned_native_stores_skip_coalescing() {
    let mut instructions = vec![
        SIRInstruction::Store(
            7u32,
            SIROffset::Static(0),
            32,
            RegisterId(0),
            vec![],
            vec![],
        ),
        SIRInstruction::Store(
            7u32,
            SIROffset::Static(32),
            32,
            RegisterId(1),
            vec![],
            vec![],
        ),
    ];
    let original = instructions.clone();
    let mut register_map = [(RegisterId(0), logic(32)), (RegisterId(1), logic(32))]
        .into_iter()
        .collect();
    let mut reg_counter = 1;

    assert!(!coalesce_static_stores_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        &HashMap::default(),
    ));
    assert_eq!(instructions, original);
    assert_eq!(reg_counter, 1);
}

#[test]
fn scalar_store_coalescing_stops_at_64_bits() {
    let mut instructions = (0..8)
        .map(|index| SIRInstruction::Imm(RegisterId(index), SIRValue::new(index as u8)))
        .chain((0..8).map(|index| {
            SIRInstruction::Store(
                7u32,
                SIROffset::Static(index * 10),
                10,
                RegisterId(index),
                vec![],
                vec![],
            )
        }))
        .collect::<Vec<_>>();
    let mut register_map = (0..8).map(|index| (RegisterId(index), logic(10))).collect();
    let mut reg_counter = 7;

    assert!(coalesce_static_stores_with_types(
        &mut instructions,
        &mut register_map,
        &mut reg_counter,
        &[(7u32, 10usize)].into_iter().collect(),
    ));

    let stores = instructions
        .iter()
        .filter_map(|instruction| match instruction {
            SIRInstruction::Store(_, _, width, ..) => Some(*width),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stores, vec![60, 20]);
    assert!(
        stores
            .iter()
            .all(|width| *width <= MAX_SCALAR_COALESCED_STORE_BITS)
    );
    verify(instructions, register_map);
}

#[test]
fn packed_element_aggregate_requires_complete_aligned_elements() {
    assert_eq!(
        aggregate_static_offset(27, 54, Some(27)),
        Some(SIROffset::PackedElements {
            bit_offset: 27,
            element_width: 27,
        })
    );
    assert_eq!(
        aggregate_static_offset(774, 63, Some(27)),
        None,
        "a partial first and last element is not an array-copy operation"
    );
    assert_eq!(
        aggregate_static_offset(774, 9, Some(27)),
        Some(SIROffset::Static(774)),
        "an ordinary access within one element remains static"
    );
    assert_eq!(
        aggregate_static_offset(0, 64, Some(1)),
        Some(SIROffset::PackedElements {
            bit_offset: 0,
            element_width: 1,
        })
    );
}

#[test]
fn wide_cross_chunk_logic_load_preserves_destination_and_verifies() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(100), 256),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(163), 129),
    ];

    assert!(subsume_static_loads(&mut instructions));
    assert_eq!(
        instructions[1],
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 63, 129)
    );
    verify(
        instructions,
        [(RegisterId(0), logic(256)), (RegisterId(1), logic(129))],
    );
}

#[test]
fn overlapping_static_store_blocks_subsumption() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::Store(
            1,
            SIROffset::Static(32),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(28), 16),
    ];

    assert!(!subsume_static_loads(&mut instructions));
    assert!(matches!(instructions[2], SIRInstruction::Load(..)));
    verify(
        instructions,
        [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
    );
}

#[test]
fn nonoverlapping_part_survives_static_store_to_same_wide_load() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::Store(
            1,
            SIROffset::Static(64),
            8,
            RegisterId(0),
            Vec::new(),
            Vec::new(),
        ),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
    ];

    assert!(subsume_static_loads(&mut instructions));
    assert_eq!(
        instructions[2],
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 0, 16)
    );
    verify(
        instructions,
        [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
    );
}

#[test]
fn overlapping_commit_blocks_subsumption() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::Commit(2, 1, SIROffset::Static(32), 8, Vec::new()),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(28), 16),
    ];

    assert!(!subsume_static_loads(&mut instructions));
    assert!(matches!(instructions[2], SIRInstruction::Load(..)));
    verify(
        instructions,
        [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
    );
}

#[test]
fn dynamic_memory_access_is_a_conservative_barrier() {
    let mut instructions = vec![
        SIRInstruction::Imm(RegisterId(2), SIRValue::new(0u8)),
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::Load(RegisterId(3), 9, SIROffset::Dynamic(RegisterId(2)), 8),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
    ];

    assert!(!subsume_static_loads(&mut instructions));
    assert!(matches!(instructions[3], SIRInstruction::Load(..)));
    verify(
        instructions,
        [
            (RegisterId(0), logic(128)),
            (RegisterId(1), logic(16)),
            (
                RegisterId(2),
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
            ),
            (RegisterId(3), logic(8)),
        ],
    );
}

#[test]
fn event_is_a_conservative_barrier() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::RuntimeEvent {
            site_id: 3,
            args: Vec::new(),
        },
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
    ];

    assert!(!subsume_static_loads(&mut instructions));
    assert!(matches!(instructions[2], SIRInstruction::Load(..)));
    verify(
        instructions,
        [(RegisterId(0), logic(128)), (RegisterId(1), logic(16))],
    );
}

#[test]
fn overflowing_static_range_is_left_unchanged_without_panicking() {
    let offset = usize::MAX - 3;
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(offset), 8),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(offset), 4),
    ];

    assert!(!subsume_static_loads(&mut instructions));
    assert!(
        instructions
            .iter()
            .all(|inst| matches!(inst, SIRInstruction::Load(..)))
    );
    verify(
        instructions,
        [(RegisterId(0), logic(8)), (RegisterId(1), logic(4))],
    );
}

#[test]
fn exact_static_load_is_left_for_existing_alias_elimination() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 128),
    ];

    assert!(!subsume_static_loads(&mut instructions));
    assert!(matches!(instructions[1], SIRInstruction::Load(..)));
    verify(
        instructions,
        [(RegisterId(0), logic(128)), (RegisterId(1), logic(128))],
    );
}

#[test]
fn logic_and_bit_loads_are_not_subsumed_across_value_plane_kinds() {
    let mut instructions = vec![
        SIRInstruction::Load(RegisterId(0), 1, SIROffset::Static(0), 128),
        SIRInstruction::Load(RegisterId(1), 1, SIROffset::Static(0), 16),
    ];
    let register_map = [
        (
            RegisterId(0),
            RegisterType::Bit {
                width: 128,
                signed: false,
            },
        ),
        (RegisterId(1), logic(16)),
    ]
    .into_iter()
    .collect::<HashMap<_, _>>();

    assert!(!subsume_static_loads_with_types(
        &mut instructions,
        &register_map
    ));
    assert!(matches!(instructions[1], SIRInstruction::Load(..)));
    verify(instructions, register_map);
}

#[test]
fn optimize_block_pipeline_applies_subsumption_and_remains_valid() {
    let mut block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Load(RegisterId(0), 1u32, SIROffset::Static(0), 128),
            SIRInstruction::Load(RegisterId(1), 1u32, SIROffset::Static(17), 31),
        ],
        terminator: SIRTerminator::Return,
    };
    let mut register_map = [(RegisterId(0), logic(128)), (RegisterId(1), logic(31))]
        .into_iter()
        .collect::<HashMap<_, _>>();
    let mut replacements = HashMap::default();
    let mut reg_counter = 1;

    optimize_block(
        &mut block,
        &mut register_map,
        &mut replacements,
        &mut reg_counter,
        true,
        false,
        &HashMap::default(),
    );

    assert!(replacements.is_empty());
    assert_eq!(
        block.instructions[1],
        SIRInstruction::Slice(RegisterId(1), RegisterId(0), 17, 31)
    );
    verify(block.instructions, register_map);
}

#[test]
fn exact_load_alias_requires_full_register_type_match() {
    let mut block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Load(RegisterId(0), 1u32, SIROffset::Static(0), 8),
            SIRInstruction::Load(RegisterId(1), 1u32, SIROffset::Static(0), 8),
        ],
        terminator: SIRTerminator::Return,
    };
    let mut register_map = [
        (
            RegisterId(0),
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        ),
        (RegisterId(1), logic(8)),
    ]
    .into_iter()
    .collect::<HashMap<_, _>>();
    let mut replacements = HashMap::default();
    let mut reg_counter = 1;

    optimize_block(
        &mut block,
        &mut register_map,
        &mut replacements,
        &mut reg_counter,
        true,
        false,
        &HashMap::default(),
    );

    assert!(replacements.is_empty());
    assert_eq!(
        block
            .instructions
            .iter()
            .filter(|inst| matches!(inst, SIRInstruction::Load(..)))
            .count(),
        2
    );
    verify(block.instructions, register_map);
}

#[test]
fn commit_forwarding_rejects_bit_source_but_accepts_logic_source() {
    let optimize = |source_type: RegisterType| {
        let mut block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(RegisterId(0), 1u32, SIROffset::Static(0), 8),
                SIRInstruction::Commit(1u32, 2u32, SIROffset::Static(0), 8, Vec::new()),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut register_map = [(RegisterId(0), source_type)]
            .into_iter()
            .collect::<HashMap<_, _>>();
        let mut replacements = HashMap::default();
        let mut reg_counter = 0;
        optimize_block(
            &mut block,
            &mut register_map,
            &mut replacements,
            &mut reg_counter,
            true,
            false,
            &HashMap::default(),
        );
        verify(block.instructions.clone(), register_map);
        block.instructions
    };

    let bit_instructions = optimize(RegisterType::Bit {
        width: 8,
        signed: false,
    });
    assert!(matches!(bit_instructions[1], SIRInstruction::Commit(..)));

    let logic_instructions = optimize(logic(8));
    assert!(matches!(
        logic_instructions[1],
        SIRInstruction::Store(2, SIROffset::Static(0), 8, RegisterId(0), _, _)
    ));
}
