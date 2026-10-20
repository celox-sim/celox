//! Compile on every host and execute the emitted code on AArch64.
use super::*;

#[test]
fn wide_reduction_and_respects_source_width_and_partial_words() {
    for width in [1usize, 63, 64, 65, 127, 128, 129, 981, 1024, 2051] {
        // Also check that widening the one-bit result keeps the high bits clear.
        for result_width in [1, 193] {
            let input = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: StateObjectId(0),
            };
            let output = AbsoluteAddr {
                var_id: StateObjectId(1),
                ..input
            };
            let address =
                |absolute| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
            let loaded = RegisterId(0);
            let reduced = RegisterId(1);
            let result = RegisterId(2);
            let unit = ExecutionUnit {
                entry_block_id: SirBlockId(0),
                blocks: [(
                    SirBlockId(0),
                    BasicBlock {
                        id: SirBlockId(0),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Load(
                                loaded,
                                address(input),
                                SIROffset::Static(0),
                                width,
                            ),
                            SIRInstruction::Unary(reduced, UnaryOp::And, loaded),
                            SIRInstruction::Unary(result, UnaryOp::Ident, reduced),
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
                register_map: [(loaded, width), (reduced, 1), (result, result_width)]
                    .into_iter()
                    .map(|(register, width)| {
                        (
                            register,
                            RegisterType::Bit {
                                width,
                                signed: false,
                            },
                        )
                    })
                    .collect(),
            };
            let output_offset = width.div_ceil(64) * 8;
            let size = output_offset + result_width.div_ceil(64) * 8;
            let mut layout = layout_for(input, width, false);
            layout.offsets.insert(output, output_offset);
            layout.widths.insert(output, result_width);
            layout.is_4states.insert(output, false);
            layout.total_size = size;
            layout.working_base_offset = size;
            layout.sparse_base_offset = size;
            layout.sparse_active_bits_offset = size;
            layout.merged_total_size = size;
            layout.triggered_bits_offset = size;
            layout.scratch_base_offset = size;
            let image = crate::scalar::emit_prepared_eu(
                &unit,
                &layout,
                false,
                "reduction_and",
                false,
                None,
                || false,
            )
            .unwrap();
            assert!(!image.code.is_empty());

            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            {
                use num_bigint::BigUint;
                let jit = crate::jit_mem::JitCode::new(&image.code).unwrap();
                let ones = (BigUint::from(1u8) << width) - 1u8;
                let mut state = vec![0u8; size];
                let mut check = |value: &BigUint, expected: u8| {
                    state.fill(0);
                    let bytes = value.to_bytes_le();
                    state[..bytes.len()].copy_from_slice(&bytes);
                    // Ensure the entire result, including widened high bits, is written.
                    state[output_offset..].fill(0xff);
                    assert_eq!(unsafe { jit.call(&mut state) }, 0);
                    let end = output_offset + result_width.div_ceil(8);
                    let actual = BigUint::from_bytes_le(&state[output_offset..end])
                        & ((BigUint::from(1u8) << result_width) - 1u8);
                    assert_eq!(
                        actual,
                        BigUint::from(expected),
                        "width={width} result_width={result_width} input={value:x}"
                    );
                };
                check(&ones, 1);
                for bit in 0..width {
                    check(&(&ones ^ (BigUint::from(1u8) << bit)), 0);
                }
                check(&ones, 1);
            }
        }
    }
}
