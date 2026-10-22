use super::*;
use celox_sir::{FloatBinaryOp as F, FloatPrecision as P, FloatUnaryOp as U};

fn float_unit(
    op: SIRInstruction<RegionedAbsoluteAddr>,
    precision: P,
) -> (ExecutionUnit<RegionedAbsoluteAddr>, MemoryLayout) {
    let absolute = AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId::default(),
    };
    let address = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, absolute);
    let mut unit = scalar_store_unit(address, false);
    let width = match precision {
        P::F32 => 32,
        P::F64 => 64,
    };
    for ty in unit.register_map.values_mut() {
        *ty = RegisterType::Bit {
            width,
            signed: false,
        };
    }
    let result_width = match &op {
        SIRInstruction::Binary(
            _,
            _,
            BinaryOp::Float {
                op: F::Eq | F::Ne | F::Lt | F::Le | F::Gt | F::Ge,
                ..
            },
            _,
        )
        | SIRInstruction::Unary(_, UnaryOp::Float { op: U::ToBool, .. }, _) => 1,
        SIRInstruction::Unary(
            _,
            UnaryOp::Float {
                op: U::Convert { to: P::F32 },
                ..
            },
            _,
        ) => 32,
        SIRInstruction::Unary(
            _,
            UnaryOp::Float {
                op: U::Convert { to: P::F64 },
                ..
            },
            _,
        ) => 64,
        _ => width,
    };
    unit.register_map.insert(
        RegisterId(2),
        RegisterType::Bit {
            width: result_width,
            signed: false,
        },
    );
    let block = unit.blocks.get_mut(&SirBlockId(0)).unwrap();
    block.instructions[0] = SIRInstruction::Imm(
        RegisterId(0),
        SIRValue::new(match precision {
            P::F32 => 1.5f32.to_bits() as u64,
            P::F64 => 1.5f64.to_bits(),
        }),
    );
    block.instructions[1] = SIRInstruction::Imm(
        RegisterId(1),
        SIRValue::new(match precision {
            P::F32 => 2.0f32.to_bits() as u64,
            P::F64 => 2.0f64.to_bits(),
        }),
    );
    block.instructions[2] = op;
    block.instructions[3] = SIRInstruction::Store(
        address,
        SIROffset::Static(0),
        result_width,
        RegisterId(2),
        Vec::new(),
        Vec::new(),
    );
    (unit, layout_for(absolute, 64, false))
}

#[test]
fn basic_float_operations_select_native_mir_and_emit_without_helpers() {
    for precision in [P::F32, P::F64] {
        for op in [
            F::Add,
            F::Sub,
            F::Mul,
            F::Div,
            F::Eq,
            F::Ne,
            F::Lt,
            F::Le,
            F::Gt,
            F::Ge,
        ] {
            let (unit, layout) = float_unit(
                SIRInstruction::Binary(
                    RegisterId(2),
                    RegisterId(0),
                    BinaryOp::Float { op, precision },
                    RegisterId(1),
                ),
                precision,
            );
            let mir = lower_execution_unit(&unit, &layout, false);
            assert!(
                mir.blocks.iter().flat_map(|b| &b.insts).any(
                    |i| matches!(i, MInst::FloatBinary { op: selected, .. } if *selected == op)
                )
            );
            assert!(
                !mir.blocks
                    .iter()
                    .flat_map(|b| &b.insts)
                    .any(|i| matches!(i, MInst::CallExtern { .. }))
            );
            let code = crate::scalar::emit_prepared_eu(
                &unit,
                &layout,
                false,
                "eval_comb",
                false,
                None,
                || false,
            )
            .unwrap();
            let opcode = match op {
                F::Add => 0x1e21_2800,
                F::Sub => 0x1e21_3800,
                F::Mul => 0x1e21_0800,
                F::Div => 0x1e21_1800,
                _ => 0x1e21_2000,
            } | if precision == P::F64 { 0x0040_0000 } else { 0 };
            assert!(
                code.code
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|word| u32::from_le_bytes(*word) == opcode),
                "missing {op:?}.{precision:?} opcode"
            );
            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            {
                let jit = crate::jit_mem::JitCode::new(&code.code).unwrap();
                let mut state = vec![0u8; layout.merged_total_size];
                assert_eq!(unsafe { jit.call(&mut state) }, 0);
                let a = if precision == P::F32 {
                    1.5f32.to_bits() as u64
                } else {
                    1.5f64.to_bits()
                };
                let b = if precision == P::F32 {
                    2.0f32.to_bits() as u64
                } else {
                    2.0f64.to_bits()
                };
                assert_eq!(
                    u64::from_le_bytes(state[..8].try_into().unwrap()),
                    celox_design::eval_float_binary(op, precision, a, b)
                );
            }
        }
        for op in [
            U::Neg,
            U::ToBool,
            U::Convert { to: P::F32 },
            U::Convert { to: P::F64 },
        ] {
            let (unit, layout) = float_unit(
                SIRInstruction::Unary(
                    RegisterId(2),
                    UnaryOp::Float { op, precision },
                    RegisterId(0),
                ),
                precision,
            );
            let mir = lower_execution_unit(&unit, &layout, false);
            assert!(
                mir.blocks
                    .iter()
                    .flat_map(|b| &b.insts)
                    .any(|i| matches!(i, MInst::FloatUnary { .. }))
            );
            let code = crate::scalar::emit_prepared_eu(
                &unit,
                &layout,
                false,
                "eval_comb",
                false,
                None,
                || false,
            )
            .unwrap();
            assert!(!code.code.is_empty());
        }
    }
}

#[test]
fn complex_float_operations_keep_helper_calls() {
    for op in [F::Rem, F::Pow] {
        let (unit, layout) = float_unit(
            SIRInstruction::Binary(
                RegisterId(2),
                RegisterId(0),
                BinaryOp::Float {
                    op,
                    precision: P::F64,
                },
                RegisterId(1),
            ),
            P::F64,
        );
        let mir = lower_execution_unit(&unit, &layout, false);
        assert!(mir.blocks.iter().flat_map(|b| &b.insts).any(|i| matches!(i, MInst::CallExtern { func, .. } if *func == celox_sir::float::FLOAT_HELPER_ID)));
    }
}
