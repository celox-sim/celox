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
    for (absolute, offset, width) in [(count, 128, count_width), (output, 192, result_width)] {
        layout.offsets.insert(absolute, offset);
        layout.widths.insert(absolute, width);
        layout.is_4states.insert(absolute, true);
    }
    layout.total_size = 256;
    layout.working_base_offset = 256;
    layout.sparse_base_offset = 256;
    layout.sparse_active_bits_offset = 256;
    layout.merged_total_size = 256;
    layout.triggered_bits_offset = 256;
    layout.scratch_base_offset = 256;
    let image =
        crate::scalar::emit_prepared_eu(&unit, &layout, true, "shift_xz", false, None, || false)
            .unwrap();
    assert!(!image.code.is_empty());

    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        use num_bigint::BigUint;
        let jit = crate::jit_mem::JitCode::new(&image.code).unwrap();
        let limit = (BigUint::from(1u8) << result_width) - 1u8;
        let payload = (BigUint::from(0xaau8) << (width - 8))
            | BigUint::from(if width == 8 { 0 } else { 0x55u8 });
        let mask = BigUint::from(0xffu8) << (width - 8);
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
            let mut state = vec![0u8; 256];
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
                    } else {
                        (value >> amount) & &limit
                    }
                };
                (shift(&payload), shift(&mask))
            };
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            let bytes = result_width.div_ceil(8);
            let actual = (
                BigUint::from_bytes_le(&state[192..192 + bytes]) & &limit,
                BigUint::from_bytes_le(&state[192 + bytes..192 + 2 * bytes]) & &limit,
            );
            assert_eq!(
                actual, expected,
                "{op:?} width={width} result={result_width} count_width={count_width} constant={constant:?} amount={amount:x} mask={count_mask:x}"
            );
        }
    }
}
