use super::*;

#[test]
fn wide_slices_preserve_each_value_and_mask_chunk() {
    let value = BigUint::parse_bytes(
        concat!(
            "f123456789abcdef",
            "0000000000000001",
            "8040201008040201",
            "0102040810204080"
        )
        .as_bytes(),
        16,
    )
    .unwrap();
    let mask = (BigUint::from(1u8) << 64usize) | (BigUint::from(1u8) << 192usize);
    for four_state in [false, true] {
        for offset in [0, 1, 63, 64, 65, 128] {
            for (width, destination_width) in [(65, 65), (128, 128), (129, 192), (145, 145)] {
                let block = BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(
                            RegisterId(0),
                            SIRValue::new_four_state(value.clone(), mask.clone()),
                        ),
                        SIRInstruction::Slice(RegisterId(1), RegisterId(0), offset, width),
                        SIRInstruction::Store(
                            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                            SIROffset::Static(0),
                            destination_width,
                            RegisterId(1),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                };
                let unit = ExecutionUnit {
                    entry_block_id: BlockId(0),
                    blocks: [(BlockId(0), block)].into_iter().collect(),
                    register_map: [
                        (RegisterId(0), RegisterType::Logic { width: 320 }),
                        (
                            RegisterId(1),
                            RegisterType::Logic {
                                width: destination_width,
                            },
                        ),
                    ]
                    .into_iter()
                    .collect(),
                };
                let memory = run_wasm(&unit, &layout(destination_width, four_state), four_state);
                let slice_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                assert_eq!(
                    read_bits(&memory, 0, destination_width),
                    (&value >> offset) & &slice_mask,
                    "value: offset={offset}, width={width}, destination_width={destination_width}"
                );
                if four_state {
                    assert_eq!(
                        read_bits_at(
                            &memory,
                            OUTPUT_OFFSET + get_byte_size(destination_width),
                            0,
                            destination_width
                        ),
                        (&mask >> offset) & &slice_mask,
                        "mask: offset={offset}, width={width}, destination_width={destination_width}"
                    );
                }
            }
        }
    }
}

#[test]
fn planes_move_value_and_mask_words_both_ways() {
    for width in [64, 128] {
        let all = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        // X where both planes are set, Z where only the mask is.
        let value = BigUint::parse_bytes(b"f0e1d2c3b4a5968778695a4b3c2d1e0f", 16).unwrap() & &all;
        let mask = BigUint::parse_bytes(b"00ff00000000f00f8000000000000003", 16).unwrap() & &all;
        for four_state in [false, true] {
            for round_trip in [false, true] {
                let destination_width = if round_trip { width } else { 2 * width };
                let mut instructions = vec![
                    SIRInstruction::Imm(
                        RegisterId(0),
                        SIRValue::new_four_state(value.clone(), mask.clone()),
                    ),
                    SIRInstruction::Unary(RegisterId(1), UnaryOp::ToPlanes, RegisterId(0)),
                ];
                if round_trip {
                    instructions.push(SIRInstruction::Unary(
                        RegisterId(2),
                        UnaryOp::FromPlanes,
                        RegisterId(1),
                    ));
                }
                instructions.push(SIRInstruction::Store(
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                    SIROffset::Static(0),
                    destination_width,
                    RegisterId(if round_trip { 2 } else { 1 }),
                    Vec::new(),
                    Vec::new(),
                ));
                let unit = ExecutionUnit {
                    entry_block_id: BlockId(0),
                    blocks: [(
                        BlockId(0),
                        BasicBlock {
                            id: BlockId(0),
                            params: Vec::new(),
                            instructions,
                            terminator: SIRTerminator::Return,
                        },
                    )]
                    .into_iter()
                    .collect(),
                    register_map: [
                        (RegisterId(0), RegisterType::Logic { width }),
                        (
                            RegisterId(1),
                            RegisterType::Bit {
                                width: 2 * width,
                                signed: false,
                            },
                        ),
                        (RegisterId(2), RegisterType::Logic { width }),
                    ]
                    .into_iter()
                    .collect(),
                };
                unit.verify();
                let memory = run_wasm(&unit, &layout(destination_width, four_state), four_state);
                let (expected_value, expected_mask) = match (round_trip, four_state) {
                    (false, true) => ((&mask << width) | &value, BigUint::default()),
                    // Two-state registers hold no mask plane.
                    (false, false) => (value.clone(), BigUint::default()),
                    (true, true) => (value.clone(), mask.clone()),
                    (true, false) => (value.clone(), BigUint::default()),
                };
                let case = format!("width={width} four_state={four_state} round_trip={round_trip}");
                assert_eq!(
                    read_bits(&memory, 0, destination_width),
                    expected_value,
                    "{case}"
                );
                if four_state {
                    assert_eq!(
                        read_bits_at(
                            &memory,
                            OUTPUT_OFFSET + get_byte_size(destination_width),
                            0,
                            destination_width
                        ),
                        expected_mask,
                        "{case}"
                    );
                }
            }
        }
    }
}
