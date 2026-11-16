//! The initialized memory image that the WebAssembly bridge instantiates.

fn write_memory_bit(memory: &mut [u8], byte: usize, bit: usize, value: bool) {
    let mask = 1u8 << bit;
    if value {
        memory[byte] |= mask;
    } else {
        memory[byte] &= !mask;
    }
}

fn byte_bit(bytes: &[u8], bit: usize) -> bool {
    bytes
        .get(bit / 8)
        .is_some_and(|byte| byte & (1u8 << (bit % 8)) != 0)
}

/// The complete initialized memory image of `program` laid out by `layout`.
pub(crate) fn initial_memory_bytes(
    program: &celox::LaidOutProgram,
    layout: &celox::MemoryLayout,
    four_state: bool,
) -> Vec<u8> {
    let mut memory = vec![0u8; layout.merged_total_size];

    if four_state {
        for (address, &offset) in &layout.offsets {
            if layout.is_4states.get(address).copied().unwrap_or(false) {
                let plane_size = layout.plane_size(address);
                memory[offset..offset + plane_size * 2].fill(0xff);
            }
        }
        for (address, &relative_offset) in &layout.working_offsets {
            if layout.is_4states.get(address).copied().unwrap_or(false) {
                let offset = layout.working_base_offset + relative_offset;
                let plane_size = layout.plane_size(address);
                memory[offset..offset + plane_size * 2].fill(0xff);
            }
        }
    }

    for initial in &program.design.initial_state {
        let Some(&offset) = layout.offsets.get(&initial.address) else {
            continue;
        };
        let width = layout.widths[&initial.address];
        let plane_size = layout.plane_size(&initial.address);
        let write_mask = four_state
            && layout
                .is_4states
                .get(&initial.address)
                .copied()
                .unwrap_or(false);

        match &initial.data {
            celox_design::InitialStateData::Packed {
                value,
                mask,
                written_mask,
            } => {
                let value = value.to_bytes_le();
                let mask = mask.to_bytes_le();
                let written_mask = written_mask.to_bytes_le();
                for bit in 0..width {
                    if !byte_bit(&written_mask, bit) {
                        continue;
                    }
                    let (byte, intra) = layout.map_static_bit_offset(&initial.address, bit);
                    let is_unknown = byte_bit(&mask, bit);
                    write_memory_bit(
                        &mut memory,
                        offset + byte,
                        intra,
                        byte_bit(&value, bit) && (write_mask || !is_unknown),
                    );
                    if write_mask {
                        write_memory_bit(
                            &mut memory,
                            offset + plane_size + byte,
                            intra,
                            is_unknown,
                        );
                    }
                }
            }
            celox_design::InitialStateData::Writes(runs) => {
                for run in runs {
                    // Two-state storage has no mask plane to hold an X or a
                    // Z: their bits are zero (IEEE 1800-2023 6.11.2, 21.4.2).
                    let run = if write_mask {
                        std::borrow::Cow::Borrowed(run)
                    } else {
                        run.two_state()
                    };
                    for relative_bit in 0..run.bit_width {
                        let bit = run.bit_offset + relative_bit;
                        if bit >= width {
                            break;
                        }
                        let (byte, intra) = layout.map_static_bit_offset(&initial.address, bit);
                        write_memory_bit(
                            &mut memory,
                            offset + byte,
                            intra,
                            byte_bit(&run.value_bytes, relative_bit),
                        );
                        if write_mask {
                            write_memory_bit(
                                &mut memory,
                                offset + plane_size + byte,
                                intra,
                                byte_bit(&run.mask_bytes, relative_bit),
                            );
                        }
                    }
                }
            }
        }
    }

    memory
}
