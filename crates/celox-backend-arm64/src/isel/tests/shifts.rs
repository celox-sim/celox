//! Compile on every host and execute the emitted code on AArch64 (also under QEMU).
use super::*;

#[test]
fn four_state_shifts_preserve_z_and_unknown_counts() {
    for width in [8, 64, 65, 128, 256] {
        for result_width in [width, 4] {
            for op in [BinaryOp::Shl, BinaryOp::Shr] {
                for count_width in [16, 128] {
                    for constant in [None, Some(0), Some(1), Some(64)] {
                        check_shift(width, result_width, op, count_width, constant);
                    }
                }
            }
        }
    }
}

#[test]
fn four_state_arithmetic_shifts_preserve_partial_sign_and_wider_fill() {
    for width in [63, 65, 129, 193] {
        for result_width in [width, width + 64, 4] {
            for count_width in [16, 129, 257] {
                for constant in [None, Some(1), Some(64), Some(65), Some(512)] {
                    check_shift(width, result_width, BinaryOp::Sar, count_width, constant);
                }
            }
        }
    }
}

fn check_shift(
    width: usize,
    result_width: usize,
    op: BinaryOp,
    count_width: usize,
    constant: Option<u16>,
) {
    let input = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(0),
    };
    let count = AbsoluteAddr {
        var_id: StateObjectId(1),
        ..input
    };
    let output = AbsoluteAddr {
        var_id: StateObjectId(2),
        ..input
    };
    let address = |absolute| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let lhs = RegisterId(0);
    let rhs = RegisterId(1);
    let result = RegisterId(2);
    let unit = ExecutionUnit {
        entry_block_id: SirBlockId(0),
        blocks: [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions: vec![
                    SIRInstruction::Load(lhs, address(input), SIROffset::Static(0), width),
                    if let Some(amount) = constant {
                        SIRInstruction::Imm(rhs, SIRValue::new(amount))
                    } else {
                        SIRInstruction::Load(rhs, address(count), SIROffset::Static(0), count_width)
                    },
                    SIRInstruction::Binary(result, lhs, op, rhs),
                    SIRInstruction::Store(
                        address(output),
                        SIROffset::Static(0),
                        result_width,
                        result,
                        vec![],
                        vec![],
                    ),
                ],
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect(),
        register_map: [(lhs, width), (rhs, count_width), (result, result_width)]
            .into_iter()
            .map(|(register, width)| (register, RegisterType::Logic { width }))
            .collect(),
    };
    let mut layout = layout_for(input, width, true);
    for (absolute, offset, width) in [(count, 128, count_width), (output, 256, result_width)] {
        layout.offsets.insert(absolute, offset);
        layout.widths.insert(absolute, width);
        layout.is_4states.insert(absolute, true);
    }
    layout.total_size = 384;
    layout.working_base_offset = 384;
    layout.sparse_base_offset = 384;
    layout.sparse_active_bits_offset = 384;
    layout.merged_total_size = 384;
    layout.triggered_bits_offset = 384;
    layout.scratch_base_offset = 384;
    let image =
        crate::scalar::emit_prepared_eu(&unit, &layout, true, "shift_xz", false, None, || false)
            .unwrap();
    assert!(!image.code.is_empty());

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        use num_bigint::BigUint;
        let jit = crate::jit_mem::JitCode::new(&image.code).unwrap();
        let limit = (BigUint::from(1u8) << result_width) - 1u8;
        let patterns = if matches!(op, BinaryOp::Sar) {
            vec![(0xaau8, 0u8), (0x55, 0), (0xaa, 0xff), (0x55, 0xff)]
        } else {
            vec![(0xaa, 0xff)]
        };
        for (upper, unknown) in patterns {
            let payload = (BigUint::from(upper) << (width - 8))
                | BigUint::from(if width == 8 { 0 } else { 0x55u8 });
            let mask = BigUint::from(unknown) << (width - 8);
            let mut counts: Vec<_> = [0, 1, width / 2, width - 1, 64, width, width + 1]
                .into_iter()
                .map(|amount| (BigUint::from(amount), BigUint::default()))
                .collect();
            // Include both X and Z counts, and an unknown bit in the high word.
            for bit in [0, count_width - 1] {
                let unknown = BigUint::from(1u8) << bit;
                counts.push((BigUint::from(2u8), unknown.clone()));
                counts.push((&unknown | BigUint::from(2u8), unknown));
            }
            for (amount, count_mask) in counts {
                let mut state = vec![0u8; 384];
                let mut write = |offset: usize, value: &BigUint| {
                    let bytes = value.to_bytes_le();
                    state[offset..offset + bytes.len()].copy_from_slice(&bytes);
                };
                write(0, &payload);
                write(width.div_ceil(8), &mask);
                write(128, &amount);
                write(128 + count_width.div_ceil(8), &count_mask);
                let expected = if constant.is_none() && count_mask != BigUint::default() {
                    (limit.clone(), limit.clone())
                } else {
                    let amount = constant.map(usize::from).unwrap_or_else(|| {
                        amount.to_u64_digits().first().copied().unwrap_or(0) as usize
                    });
                    let shift = |value: &BigUint| {
                        if matches!(op, BinaryOp::Shl) {
                            (value << amount) & &limit
                        } else if matches!(op, BinaryOp::Sar) {
                            use num_bigint::BigInt;
                            // Sign-fill comes from the declared source width, even
                            // when a direct SIR consumer narrows or widens it.
                            let signed = if value.bit((width - 1) as u64) {
                                BigInt::from(value.clone()) - (BigInt::from(1u8) << width)
                            } else {
                                BigInt::from(value.clone())
                            };
                            ((signed >> amount.min(width.max(result_width)))
                                & BigInt::from(limit.clone()))
                            .to_biguint()
                            .unwrap()
                        } else {
                            (value >> amount) & &limit
                        }
                    };
                    (shift(&payload), shift(&mask))
                };
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                let bytes = result_width.div_ceil(8);
                let actual = (
                    BigUint::from_bytes_le(&state[256..256 + bytes]) & &limit,
                    BigUint::from_bytes_le(&state[256 + bytes..256 + 2 * bytes]) & &limit,
                );
                assert_eq!(
                    actual, expected,
                    "{op:?} width={width} result={result_width} count_width={count_width} constant={constant:?} amount={amount:x} mask={count_mask:x}"
                );
            }
        }
    }
}
