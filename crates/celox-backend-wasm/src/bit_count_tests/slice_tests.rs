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
