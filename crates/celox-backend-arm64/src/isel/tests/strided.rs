//! Execute aggregate accesses against independently initialized physical lanes.
use super::*;
use celox_state_layout::{SparseWorkingLayout, UnpackedArrayLayout};
use num_bigint::BigUint;

fn set_bit(bytes: &mut [u8], bit: usize, value: bool) {
    bytes[bit / 8] = (bytes[bit / 8] & !(1 << (bit % 8))) | ((value as u8) << (bit % 8));
}

#[test]
fn aggregate_accesses_preserve_strided_elements_and_masks() {
    const OUTPUT: usize = 256;
    const SPARSE: usize = 512;
    const DIRTY: usize = 896;
    const SUMMARY: usize = 904;
    const ACTIVE: usize = 912;
    const STATE_SIZE: usize = 1024;
    for element_width in [1usize, 3, 7, 8, 17, 24, 33, 51, 57, 64, 65] {
        for four_state in [false, true] {
            for sparse in [false, true] {
                for packed_offset in [false, true] {
                    let width = element_width * 4;
                    let stride = element_width.div_ceil(8).next_power_of_two();
                    let plane_size = stride * 4;
                    let start = if packed_offset { element_width } else { 1 };
                    let count = element_width * 2 + usize::from(!packed_offset);
                    let absolute = AbsoluteAddr {
                        instance_id: InstanceId(0),
                        var_id: StateObjectId(0),
                    };
                    let output_absolute = AbsoluteAddr {
                        var_id: StateObjectId(1),
                        ..absolute
                    };
                    let stable = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
                    let destination = RegionedAbsoluteAddr::from_absolute_addr(
                        if sparse {
                            crate::SPARSE_WORKING_REGION
                        } else {
                            STABLE_REGION
                        },
                        absolute,
                    );
                    let output =
                        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_absolute);
                    let offset = |bit_offset| {
                        if packed_offset {
                            SIROffset::PackedElements {
                                bit_offset,
                                element_width,
                            }
                        } else {
                            SIROffset::Static(bit_offset)
                        }
                    };
                    let mut value = vec![0u8; count.div_ceil(8)];
                    let mut mask = value.clone();
                    for bit in 0..count {
                        let unknown = four_state && bit % 5 == 1;
                        set_bit(&mut value, bit, bit % 3 != 0 || unknown);
                        set_bit(&mut mask, bit, unknown);
                    }
                    let replacement = RegisterId(0);
                    let loaded = RegisterId(1);
                    let mut instructions = vec![
                        SIRInstruction::Imm(
                            replacement,
                            SIRValue::new_four_state(
                                BigUint::from_bytes_le(&value),
                                BigUint::from_bytes_le(&mask),
                            ),
                        ),
                        SIRInstruction::Store(
                            destination,
                            offset(start),
                            count,
                            replacement,
                            vec![],
                            vec![],
                        ),
                    ];
                    if sparse {
                        instructions.push(SIRInstruction::Commit(
                            destination,
                            stable,
                            SIROffset::Static(0),
                            width,
                            vec![],
                        ));
                    }
                    instructions.extend([
                        SIRInstruction::Load(loaded, stable, offset(0), width),
                        SIRInstruction::Store(
                            output,
                            SIROffset::Static(0),
                            width,
                            loaded,
                            vec![],
                            vec![],
                        ),
                    ]);
                    let unit = ExecutionUnit {
                        entry_block_id: SirBlockId(0),
                        blocks: [(
                            SirBlockId(0),
                            BasicBlock {
                                id: SirBlockId(0),
                                params: vec![],
                                instructions,
                                terminator: SIRTerminator::Return,
                            },
                        )]
                        .into_iter()
                        .collect(),
                        register_map: [(replacement, count), (loaded, width)]
                            .into_iter()
                            .map(|(register, width)| {
                                (
                                    register,
                                    if four_state {
                                        RegisterType::Logic { width }
                                    } else {
                                        RegisterType::Bit {
                                            width,
                                            signed: false,
                                        }
                                    },
                                )
                            })
                            .collect(),
                    };
                    let mut layout = layout_for(absolute, width, four_state);
                    layout.mode = MemoryLayoutMode::ElementStrided;
                    layout.unpacked_arrays.insert(
                        absolute,
                        UnpackedArrayLayout {
                            element_width,
                            element_count: 4,
                            element_stride: stride,
                            plane_size,
                        },
                    );
                    layout.offsets.insert(output_absolute, OUTPUT);
                    layout.widths.insert(output_absolute, width);
                    layout.is_4states.insert(output_absolute, four_state);
                    layout.total_size = SPARSE;
                    layout.working_base_offset = SPARSE;
                    layout.sparse_base_offset = SPARSE;
                    if sparse {
                        layout.sparse_offsets.insert(absolute, 0);
                        layout.sparse_layouts.insert(
                            absolute,
                            SparseWorkingLayout {
                                active_index: 0,
                                chunk_count: plane_size.div_ceil(8),
                                dirty_words_offset: DIRTY,
                                dirty_word_count: 1,
                                summary_words_offset: SUMMARY,
                                summary_word_count: 1,
                            },
                        );
                        layout.sparse_active_capacity = 1;
                    }
                    layout.sparse_active_bits_offset = ACTIVE;
                    layout.merged_total_size = STATE_SIZE;
                    layout.triggered_bits_offset = STATE_SIZE;
                    layout.scratch_base_offset = STATE_SIZE;
                    let result = crate::scalar::emit_prepared_eu(
                        &unit,
                        &layout,
                        four_state,
                        "strided_aggregate",
                        false,
                        None,
                        || false,
                    )
                    .unwrap();
                    assert!(!result.code.is_empty());

                    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
                    {
                        let jit = crate::jit_mem::JitCode::new(&result.code).unwrap();
                        let mut state = vec![0xa5; STATE_SIZE];
                        state[DIRTY..ACTIVE + 8].fill(0);
                        for plane in 0..if four_state { 2 } else { 1 } {
                            for bit in 0..width {
                                let initial = if plane == 0 {
                                    bit % 2 == 0
                                } else {
                                    bit % 7 == 0
                                };
                                let physical = (plane * plane_size + bit / element_width * stride)
                                    * 8
                                    + bit % element_width;
                                set_bit(&mut state, physical, initial);
                            }
                        }
                        assert_eq!(unsafe { jit.call(&mut state) }, 0);
                        for plane in 0..if four_state { 2 } else { 1 } {
                            for bit in 0..width {
                                let expected = if (start..start + count).contains(&bit) {
                                    let source = if plane == 0 { &value } else { &mask };
                                    (source[(bit - start) / 8] >> ((bit - start) % 8)) & 1
                                } else if plane == 0 {
                                    (bit % 2 == 0) as u8
                                } else {
                                    (bit % 7 == 0) as u8
                                };
                                let physical = (plane * plane_size + bit / element_width * stride)
                                    * 8
                                    + bit % element_width;
                                let gathered = (OUTPUT + plane * width.div_ceil(8)) * 8 + bit;
                                for location in [physical, gathered] {
                                    assert_eq!(
                                        (state[location / 8] >> (location % 8)) & 1,
                                        expected,
                                        "element_width={element_width}, four_state={four_state}, sparse={sparse}, packed_offset={packed_offset}, plane={plane}, bit={bit}, location={location}"
                                    );
                                }
                            }
                        }
                        assert_eq!(&state[DIRTY..ACTIVE + 8], &[0; ACTIVE + 8 - DIRTY]);
                    }
                }
            }
        }
    }
}
