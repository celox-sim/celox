use super::*;
use celox_design::StateObjectId as VarId;

#[test]
fn planes_round_trip_run_time_four_state_values() {
    for width in [64usize, 128, 192] {
        for four_state in [false, true] {
            let [a, p, b] = [0, 1, 2].map(|id| AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId(id),
            });
            let address = |a| RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, a);
            let [loaded, planes, back] = std::array::from_fn(RegisterId);
            let unit = ExecutionUnit {
                entry_block_id: SirBlockId(0),
                blocks: [(
                    SirBlockId(0),
                    BasicBlock {
                        id: SirBlockId(0),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Load(loaded, address(a), SIROffset::Static(0), width),
                            SIRInstruction::Unary(planes, UnaryOp::ToPlanes, loaded),
                            SIRInstruction::Store(
                                address(p),
                                SIROffset::Static(0),
                                2 * width,
                                planes,
                                vec![],
                                vec![],
                            ),
                            SIRInstruction::Unary(back, UnaryOp::FromPlanes, planes),
                            SIRInstruction::Store(
                                address(b),
                                SIROffset::Static(0),
                                width,
                                back,
                                vec![],
                                vec![],
                            ),
                        ],
                        terminator: SIRTerminator::Return,
                    },
                )]
                .into_iter()
                .collect(),
                register_map: [
                    (loaded, RegisterType::Logic { width }),
                    (
                        planes,
                        RegisterType::Bit {
                            width: 2 * width,
                            signed: false,
                        },
                    ),
                    (back, RegisterType::Logic { width }),
                ]
                .into_iter()
                .collect(),
            };
            unit.verify();
            let bytes = width / 8;
            let planes_of = if four_state { 2 } else { 1 };
            let p_offset = bytes * planes_of;
            let b_offset = p_offset + 2 * bytes;
            let size = b_offset + bytes * planes_of;
            let mut layout = layout_for(a, width, four_state);
            layout.offsets.extend([(p, p_offset), (b, b_offset)]);
            layout.widths.extend([(p, 2 * width), (b, width)]);
            layout.is_4states.extend([(p, false), (b, four_state)]);
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
                four_state,
                "planes",
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
                let all = (BigUint::from(1u8) << width) - 1u8;
                // X where both planes are set, Z where only the mask is.
                let value =
                    BigUint::parse_bytes(b"f0e1d2c3b4a5968778695a4b3c2d1e0f", 16).unwrap() & &all;
                let mask =
                    BigUint::parse_bytes(b"00ff00000000f00f8000000000000003", 16).unwrap() & &all;
                let mut state = vec![0u8; size];
                let put = |state: &mut Vec<u8>, at: usize, bits: &BigUint| {
                    let le = bits.to_bytes_le();
                    state[at..at + le.len()].copy_from_slice(&le);
                };
                put(&mut state, 0, &value);
                if four_state {
                    put(&mut state, bytes, &mask);
                }
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                let read = |at: usize, count: usize| BigUint::from_bytes_le(&state[at..at + count]);
                let mask = if four_state { mask } else { BigUint::default() };
                let case = format!("width={width} four_state={four_state}");
                assert_eq!(
                    read(p_offset, 2 * bytes),
                    (&mask << width) | &value,
                    "{case}"
                );
                assert_eq!(read(b_offset, bytes), value, "{case}");
                if four_state {
                    assert_eq!(read(b_offset + bytes, bytes), mask, "{case}");
                }
            }
        }
    }
}
