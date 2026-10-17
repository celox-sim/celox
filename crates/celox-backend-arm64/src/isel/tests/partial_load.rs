//! A rounded wide load must not import padding or neighbouring value/mask bytes.
use super::*;

#[test]
fn rounded_wide_value_loads_keep_adjacent_bits_out_of_comparisons() {
    check(false);
}
#[test]
fn rounded_wide_mask_loads_keep_adjacent_bits_out_of_comparisons() {
    check(true);
}

fn check(four_state: bool) {
    for width in [100usize, 65, 72, 73, 80, 81, 96, 97, 113, 127, 128] {
        let input = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: StateObjectId(0),
        };
        let output = AbsoluteAddr {
            var_id: StateObjectId(1),
            ..input
        };
        let address = |absolute| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
        let loaded = RegisterId(0);
        let zero = RegisterId(1);
        let equal = RegisterId(2);
        let widened = RegisterId(3);
        let unit = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Load(loaded, address(input), SIROffset::Static(0), width),
                        SIRInstruction::Imm(zero, SIRValue::new(0u8)),
                        SIRInstruction::Unary(widened, UnaryOp::Ident, zero),
                        SIRInstruction::Binary(equal, loaded, BinaryOp::Eq, widened),
                        SIRInstruction::Store(
                            address(output),
                            SIROffset::Static(0),
                            1,
                            equal,
                            vec![],
                            vec![],
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [(loaded, width), (zero, 8), (widened, width), (equal, 1)]
                .into_iter()
                .map(|(r, width)| {
                    (
                        r,
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
        let mut layout = layout_for(input, width, four_state);
        layout.offsets.insert(output, 64);
        layout.widths.insert(output, 1);
        layout.is_4states.insert(output, four_state);
        layout.total_size = 96;
        layout.working_base_offset = 96;
        layout.sparse_base_offset = 96;
        layout.sparse_active_bits_offset = 96;
        layout.merged_total_size = 96;
        layout.triggered_bits_offset = 96;
        layout.scratch_base_offset = 96;
        let image = crate::scalar::emit_prepared_eu(
            &unit,
            &layout,
            four_state,
            "wide_load",
            false,
            None,
            || false,
        )
        .unwrap();
        assert!(!image.code.is_empty());
        #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
        {
            let jit = crate::jit_mem::JitCode::new(&image.code).unwrap();
            let mut state = vec![0xa5u8; 96];
            for bit in 0..width {
                state[bit / 8] &= !(1 << (bit % 8));
                if four_state {
                    state[width.div_ceil(8) + bit / 8] &= !(1 << (bit % 8));
                }
            }
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(state[64] & 1, 1, "width={width} four_state={four_state}");
            if four_state {
                assert_eq!(state[65] & 1, 0, "width={width} mask");
            }
            let high = width - 1;
            state[high / 8] |= 1 << (high % 8);
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(state[64] & 1, 0, "width={width} last value bit");
            if four_state {
                assert_eq!(state[65] & 1, 0, "width={width} known mismatch");
                state[width.div_ceil(8) + high / 8] |= 1 << (high % 8);
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                assert_eq!(state[64] & 1, 1, "width={width} X value");
                assert_eq!(state[65] & 1, 1, "width={width} last mask bit");
            }
        }
    }
}
