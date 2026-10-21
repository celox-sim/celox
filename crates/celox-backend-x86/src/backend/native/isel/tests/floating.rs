use super::*;
use crate::native::features::{StateBaseStrategy, X86Features};
use celox_sir::{FloatBinaryOp as F, FloatPrecision as P, FloatUnaryOp as U};

#[test]
fn float_helpers_preserve_live_registers_and_narrow_mux_results() {
    const LIVE: usize = 24;
    const BASE: usize = 64;
    for four_state in [false, true] {
        for strategy in [StateBaseStrategy::R15, StateBaseStrategy::Gs] {
            if strategy == StateBaseStrategy::Gs && X86Features::detect().state_base() != strategy {
                continue;
            }
            let abs = AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::default(),
            };
            let addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, abs);
            let mut layout = empty_layout();
            layout.four_state = four_state;
            layout.offsets.insert(abs, BASE);
            layout.widths.insert(abs, (LIVE * 2 + 1) * 64);
            layout.is_4states.insert(abs, false);
            layout.total_size = BASE + (LIVE * 2 + 1) * 8;
            layout.merged_total_size = layout.total_size;
            layout.scratch_base_offset = layout.total_size;
            let bit = |width| RegisterType::Bit {
                width,
                signed: false,
            };
            let mut types = (0..LIVE)
                .map(|i| (RegisterId(i), bit(64)))
                .collect::<HashMap<_, _>>();
            let mut insts = (0..LIVE)
                .map(|i| SIRInstruction::Load(RegisterId(i), addr, SIROffset::Static(i * 64), 64))
                .collect::<Vec<_>>();
            let sum = RegisterId(LIVE);
            let narrow = RegisterId(LIVE + 1);
            let condition = RegisterId(LIVE + 2);
            let fallback = RegisterId(LIVE + 3);
            let result = RegisterId(LIVE + 4);
            types.extend([
                (sum, bit(64)),
                (narrow, bit(3)),
                (condition, bit(1)),
                (fallback, bit(3)),
                (result, bit(3)),
            ]);
            insts.extend([
                SIRInstruction::Binary(
                    sum,
                    RegisterId(0),
                    BinaryOp::Float {
                        op: F::Add,
                        precision: P::F64,
                    },
                    RegisterId(1),
                ),
                SIRInstruction::Unary(
                    narrow,
                    UnaryOp::Float {
                        op: U::ToUnsigned {
                            width: 3,
                            round: true,
                        },
                        precision: P::F64,
                    },
                    sum,
                ),
                SIRInstruction::Unary(
                    condition,
                    UnaryOp::Float {
                        op: U::ToBool,
                        precision: P::F64,
                    },
                    RegisterId(0),
                ),
                SIRInstruction::Imm(fallback, SIRValue::new(7u8)),
                SIRInstruction::Mux(result, condition, narrow, fallback),
            ]);
            for i in 0..LIVE {
                insts.push(SIRInstruction::Store(
                    addr,
                    SIROffset::Static((LIVE + i) * 64),
                    64,
                    RegisterId(i),
                    vec![],
                    vec![],
                ));
            }
            insts.push(SIRInstruction::Store(
                addr,
                SIROffset::Static(LIVE * 2 * 64),
                3,
                result,
                vec![],
                vec![],
            ));
            let unit = ExecutionUnit {
                entry_block_id: SirBlockId(0),
                blocks: [(
                    SirBlockId(0),
                    BasicBlock {
                        id: SirBlockId(0),
                        params: vec![],
                        instructions: insts,
                        terminator: SIRTerminator::Return,
                    },
                )]
                .into_iter()
                .collect(),
                register_map: types,
            };
            unit.verify();
            let mut function = lower_execution_unit(&unit, &layout, four_state);
            function.target_features = X86Features::for_test_with_state_base(false, strategy);
            mir_legalize::legalize(&mut function);
            mir_opt::optimize(&mut function);
            assert_eq!(
                function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.insts)
                    .filter(|inst| matches!(inst, MInst::CallExtern { .. }))
                    .count(),
                3
            );
            let allocation = regalloc::run_regalloc(&mut function).unwrap();
            assert!(
                allocation.spill_frame_size > 0,
                "fixture must exercise spill preservation"
            );
            mir_opt::post_regalloc_peephole(&mut function, &allocation.assignment);
            let emitted = emit::emit(
                &function,
                &allocation.assignment,
                allocation.spill_frame_size,
            )
            .unwrap();
            let jit = JitCode::new(&emitted.code).unwrap();
            let mut memory = vec![0u8; emitted.required_state_size as usize];
            let table = [celox_sir::float::evaluate as *const () as usize];
            let pointer = table.as_ptr().wrapping_add(1) as u64;
            let header = celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET;
            memory[header..header + 8].copy_from_slice(&pointer.to_le_bytes());
            let mut values = (0..LIVE)
                .map(|i| 0x0101_0101_0000_0000u64.wrapping_mul(i as u64 + 1) + i as u64)
                .collect::<Vec<_>>();
            values[0] = 1.5f64.to_bits();
            values[1] = 2.25f64.to_bits();
            for (i, value) in values.iter().enumerate() {
                memory[BASE + i * 8..BASE + i * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            assert_eq!(unsafe { jit.call(&mut memory) }, 0);
            for (i, value) in values.iter().enumerate() {
                assert_eq!(
                    &memory[BASE + (LIVE + i) * 8..BASE + (LIVE + i) * 8 + 8],
                    &value.to_le_bytes(),
                    "{strategy:?} {four_state} live r{i}"
                );
            }
            assert_eq!(
                memory[BASE + LIVE * 2 * 8] & 7,
                4,
                "{strategy:?} {four_state} narrow rounded mux"
            );
        }
    }
}
