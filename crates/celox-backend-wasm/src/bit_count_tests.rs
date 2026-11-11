mod slice_tests;

use celox_design::StateObjectId as VarId;
use num_bigint::BigUint;
use wasmtime::{Engine, Linker, Memory, Module as WasmtimeModule, Store};

use super::*;
use celox_backend_cranelift::{CompileOptions, CraneliftOptions, JitEngine};
use celox_design::InstanceId;
use celox_sir::BasicBlock;

const OUTPUT_OFFSET: usize = 32;
const SLOT_BITS: usize = 128;

#[derive(Clone)]
struct CountCase {
    source_width: usize,
    destination_width: usize,
    value: BigUint,
    mask: BigUint,
    expected: [usize; 3],
}

fn address() -> AbsoluteAddr {
    AbsoluteAddr {
        instance_id: InstanceId(0),
        var_id: VarId::default(),
    }
}

fn layout(variable_width: usize, four_state: bool) -> MemoryLayout {
    let addr = address();
    let mut offsets = HashMap::default();
    offsets.insert(addr, OUTPUT_OFFSET);
    let mut widths = HashMap::default();
    widths.insert(addr, variable_width);
    let mut is_4states = HashMap::default();
    is_4states.insert(addr, true);
    let variable_bytes = get_byte_size(variable_width);
    let total_size = OUTPUT_OFFSET + variable_bytes * usize::from(four_state) + variable_bytes;
    let working_base_offset = (total_size + 7) & !7;

    MemoryLayout {
        trace: None,
        settle: None,
        four_state,
        mode: MemoryLayoutMode::Packed,
        unpacked_arrays: HashMap::default(),
        offsets,
        widths,
        is_4states,
        total_size,
        working_offsets: HashMap::default(),
        working_base_offset,
        sparse_offsets: HashMap::default(),
        sparse_base_offset: working_base_offset,
        sparse_layouts: HashMap::default(),
        sparse_active_bits_offset: working_base_offset,
        sparse_active_capacity: 0,
        merged_total_size: 65_536,
        triggered_bits_offset: working_base_offset,
        triggered_bits_total_size: 0,
        scratch_base_offset: working_base_offset,
        scratch_size: 0,
        runtime_event_capacity: 0,
        runtime_event_slot_size: 0,
        runtime_event_buffer_size: 0,
        runtime_event_site_layouts: Vec::new(),
        call_stack: None,
    }
}

fn execution_unit(cases: &[CountCase]) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut register_map = HashMap::default();
    let mut instructions = Vec::new();
    let mut next_reg = 0usize;
    let ops = [
        UnaryOp::PopCount,
        UnaryOp::CountLeadingZeros,
        UnaryOp::CountTrailingZeros,
    ];

    for (case_index, case) in cases.iter().enumerate() {
        let source = RegisterId(next_reg);
        next_reg += 1;
        register_map.insert(
            source,
            RegisterType::Logic {
                width: case.source_width,
            },
        );
        instructions.push(SIRInstruction::Imm(
            source,
            SIRValue::new_four_state(case.value.clone(), case.mask.clone()),
        ));

        for (op_index, op) in ops.iter().copied().enumerate() {
            let destination = RegisterId(next_reg);
            next_reg += 1;
            register_map.insert(
                destination,
                RegisterType::Logic {
                    width: case.destination_width,
                },
            );
            instructions.push(SIRInstruction::Unary(destination, op, source));
            let result_index = case_index * ops.len() + op_index;
            instructions.push(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                SIROffset::Static(result_index * SLOT_BITS),
                case.destination_width,
                destination,
                Vec::new(),
                Vec::new(),
            ));
        }
    }

    let block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions,
        terminator: SIRTerminator::Return,
    };
    ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: std::iter::once((BlockId(0), block)).collect(),
        register_map,
    }
}

fn run_cranelift(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
) -> Vec<u8> {
    let options = CompileOptions {
        four_state,
        emit_triggers: false,
        cranelift: CraneliftOptions::default(),
    };
    let mut engine = JitEngine::new(layout.clone(), &options).expect("create JIT engine");
    let code = engine
        .compile_units(std::slice::from_ref(unit), None, None, None)
        .expect("compile count operations");
    let function: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(code) };
    let mut memory = vec![0u8; layout.merged_total_size];
    let status = unsafe { function(memory.as_mut_ptr()) };
    assert_eq!(status, 0);
    memory
}

fn run_wasm(
    unit: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
) -> Vec<u8> {
    let wasm = compile_units(std::slice::from_ref(unit), layout, four_state, false);
    let engine = Engine::default();
    let module = WasmtimeModule::new(&engine, &wasm.bytes).expect("compile Wasm module");
    let mut store = Store::new(&engine, ());
    let memory =
        Memory::new(&mut store, wasmtime::MemoryType::new(1, None)).expect("create Wasm memory");
    let mut linker = Linker::new(&engine);
    linker
        .define(&mut store, "env", "memory", memory)
        .expect("define Wasm memory");
    let instance = linker
        .instantiate(&mut store, &module)
        .expect("instantiate Wasm module");
    let run = instance
        .get_typed_func::<(), i64>(&mut store, "run")
        .expect("find Wasm entry point");
    assert_eq!(run.call(&mut store, ()).expect("execute Wasm"), 0);
    memory.data(&store).to_vec()
}

#[test]
fn float_integer_conversions_are_portable_without_host_imports() {
    use celox_sir::{FloatPrecision as P, FloatUnaryOp as U};
    let double_round = (1u64 << 63) + (1u64 << 39) + 1;
    assert_ne!(
        (double_round as f32).to_bits(),
        ((double_round as f64) as f32).to_bits()
    );
    for four_state in [false, true] {
        let mut cases = Vec::new();
        for p in [P::F32, P::F64] {
            for width in [1, 3, 8, 32, 64] {
                for value in [0, 1, u64::MAX, double_round, 1u64 << 63] {
                    for op in [U::FromSigned { width }, U::FromUnsigned { width }] {
                        let mask = if four_state { value & 0x24 } else { 0 };
                        cases.push((op, p, width, value, mask));
                    }
                }
            }
            let encode = |x: f64| {
                if p == P::F32 {
                    (x as f32).to_bits() as u64
                } else {
                    x.to_bits()
                }
            };
            let half = encode(0.5);
            let mut inputs = [
                0.0,
                -0.0,
                0.5,
                -0.5,
                1.5,
                -1.5,
                1.75,
                -1.75,
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::MAX,
                -f64::MAX,
                9_223_372_036_854_775_808.0,
                18_446_744_073_709_551_616.0,
                18_446_744_073_709_555_712.0,
                -18_446_744_073_709_555_712.0,
            ]
            .into_iter()
            .map(encode)
            .collect::<Vec<_>>();
            inputs.extend([
                half - 1,
                half + 1,
                if p == P::F32 {
                    f32::MAX.to_bits() as u64
                } else {
                    f64::MAX.to_bits()
                },
            ]);
            for &value in &inputs {
                for op in [
                    U::Neg,
                    U::ToBool,
                    U::Convert { to: P::F32 },
                    U::Convert { to: P::F64 },
                ] {
                    cases.push((op, p, p.width(), value, 0));
                }
            }
            for width in [1, 3, 8, 32, 64] {
                for round in [false, true] {
                    for &value in &inputs {
                        for op in [U::ToSigned { width, round }, U::ToUnsigned { width, round }] {
                            cases.push((op, p, p.width(), value, 0));
                        }
                    }
                }
            }
        }
        let mut register_map = HashMap::default();
        let mut instructions = Vec::new();
        let mut expected = Vec::new();
        for (index, (op, p, width, value, mask)) in cases.into_iter().enumerate() {
            let source = RegisterId(index * 2);
            let dst = RegisterId(index * 2 + 1);
            let float_op = UnaryOp::Float { op, precision: p };
            let result_width = float_op.result_width(width);
            let integer_input = matches!(op, U::FromSigned { .. } | U::FromUnsigned { .. });
            let width_mask = if width == 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            let value = value & width_mask;
            let mask = mask & width_mask;
            register_map.insert(
                source,
                if four_state && integer_input {
                    RegisterType::Logic { width }
                } else {
                    RegisterType::Bit {
                        width,
                        signed: false,
                    }
                },
            );
            register_map.insert(
                dst,
                RegisterType::Bit {
                    width: result_width,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(
                source,
                if four_state && integer_input {
                    SIRValue::new_four_state(value, mask)
                } else {
                    SIRValue::new(value)
                },
            ));
            instructions.push(SIRInstruction::Unary(dst, float_op, source));
            instructions.push(SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                SIROffset::Static(index * 64),
                result_width,
                dst,
                vec![],
                vec![],
            ));
            expected.push((
                result_width,
                celox_design::eval_float_unary(op, p, value & !mask),
            ));
        }
        let block = BasicBlock {
            id: BlockId(0),
            params: vec![],
            instructions,
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: std::iter::once((BlockId(0), block)).collect(),
            register_map,
        };
        unit.verify();
        let layout = layout(expected.len() * 64, four_state);
        // run_wasm binds only memory, so this also rejects accidental helper imports.
        let output = run_wasm(&unit, &layout, four_state);
        for (index, (width, value)) in expected.into_iter().enumerate() {
            assert_eq!(
                read_bits_at(&output, OUTPUT_OFFSET, index * 64, width),
                BigUint::from(value),
                "case {index}, four_state={four_state}"
            );
        }
    }
}

#[test]
fn float_native_arithmetic_preserves_ieee_bits() {
    use celox_sir::{FloatBinaryOp as F, FloatPrecision as P};
    for precision in [P::F32, P::F64] {
        for four_state in [false, true] {
            for (op, a, b, expected) in [
                (F::Add, 1.25, 2.5, 3.75),
                (F::Sub, 2.5, 1.25, 1.25),
                (F::Add, f64::NAN, 1.0, f64::NAN),
                (F::Eq, f64::NAN, 1.0, 0.0),
                (F::Ne, f64::NAN, 1.0, 1.0),
                (F::Lt, f64::NAN, 1.0, 0.0),
                (F::Le, -0.0, 0.0, 1.0),
                (F::Gt, -0.0, 0.0, 0.0),
                (F::Ge, -0.0, 0.0, 1.0),
                (F::Div, 1.0, 0.0, f64::INFINITY),
                (F::Mul, -0.0, 2.0, -0.0),
            ] {
                let bits = |x: f64| {
                    if precision == P::F32 {
                        (x as f32).to_bits() as u64
                    } else {
                        x.to_bits()
                    }
                };
                let width = precision.width();
                let register_map = (0..3)
                    .map(|i| {
                        (
                            RegisterId(i),
                            RegisterType::Bit {
                                width,
                                signed: false,
                            },
                        )
                    })
                    .collect();
                let block = BasicBlock {
                    id: BlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Imm(RegisterId(0), SIRValue::new(BigUint::from(bits(a)))),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new(BigUint::from(bits(b)))),
                        SIRInstruction::Binary(
                            RegisterId(2),
                            RegisterId(0),
                            BinaryOp::Float { op, precision },
                            RegisterId(1),
                        ),
                        SIRInstruction::Store(
                            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                            SIROffset::Static(0),
                            width,
                            RegisterId(2),
                            vec![],
                            vec![],
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                };
                let unit = ExecutionUnit {
                    entry_block_id: BlockId(0),
                    blocks: std::iter::once((BlockId(0), block)).collect(),
                    register_map,
                };
                let layout = layout(width, four_state);
                let options = CompileOptions {
                    four_state,
                    emit_triggers: false,
                    cranelift: CraneliftOptions::default(),
                };
                let mut engine = JitEngine::new(layout.clone(), &options).unwrap();
                let mut clif = String::new();
                engine
                    .compile_units(std::slice::from_ref(&unit), Some(&mut clif), None, None)
                    .unwrap();
                assert!(
                    !clif.contains("call_indirect"),
                    "basic FP operation used a helper: {clif}"
                );
                for output in [
                    run_cranelift(&unit, &layout, four_state),
                    run_wasm(&unit, &layout, four_state),
                ] {
                    let actual = read_bits_at(&output, OUTPUT_OFFSET, 0, width)
                        .to_u64_digits()
                        .first()
                        .copied()
                        .unwrap_or(0);
                    if expected.is_nan() {
                        assert!(if precision == P::F32 {
                            f32::from_bits(actual as u32).is_nan()
                        } else {
                            f64::from_bits(actual).is_nan()
                        });
                    } else {
                        let comparison =
                            matches!(op, F::Eq | F::Ne | F::Lt | F::Le | F::Gt | F::Ge);
                        assert_eq!(
                            actual,
                            if comparison {
                                expected as u64
                            } else {
                                bits(expected)
                            }
                        );
                    }
                }
            }
        }
    }
}

fn read_bits_at(memory: &[u8], base_offset: usize, bit_offset: usize, width: usize) -> BigUint {
    assert!(bit_offset.is_multiple_of(8));
    let byte_offset = base_offset + bit_offset / 8;
    BigUint::from_bytes_le(&memory[byte_offset..byte_offset + get_byte_size(width)])
        & ((BigUint::from(1u8) << width) - BigUint::from(1u8))
}

fn read_bits(memory: &[u8], bit_offset: usize, width: usize) -> BigUint {
    read_bits_at(memory, OUTPUT_OFFSET, bit_offset, width)
}

#[test]
fn shifted_scalar_stores_preserve_spill_bits_and_neighbors() {
    // A single source chunk can cover nine destination bytes. In particular,
    // OptimizeBlocks coalesces four 15-bit array elements into a 60-bit store
    // at bit 15; a u64 read-modify-write loses its final three bits.
    for four_state in [false, true] {
        for (offset, width) in [
            (0, 64),
            (1, 63),
            (1, 64),
            (7, 58),
            (7, 64),
            (15, 60),
            (0, 65),
            (0, 98),
            (0, 127),
            (8, 98),
        ] {
            for (packed_elements, source_logic) in
                [(false, false), (false, true), (true, false), (true, true)]
            {
                let initial = 0xa55a_3cc3_f00f_6996_9669_0ff0_c33c_5aa5u128;
                let initial_mask = 0x0ff0_9669_5aa5_c33c_3cc3_a55a_6996_f00fu128;
                let field_mask = (1u128 << width) - 1;
                let value = 0xd9e7_b3f5_a6c8_912fu128 & field_mask;
                let mask = if source_logic {
                    0xa55a_9669_3cc3_f00fu128 & field_mask
                } else {
                    0
                };
                let destination =
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address());
                let store_offset = if packed_elements {
                    SIROffset::PackedElements {
                        bit_offset: offset,
                        element_width: 15,
                    }
                } else {
                    SIROffset::Static(offset)
                };
                let block = BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Imm(
                            RegisterId(0),
                            SIRValue::new_four_state(initial, initial_mask),
                        ),
                        SIRInstruction::Store(
                            destination,
                            SIROffset::Static(0),
                            128,
                            RegisterId(0),
                            Vec::new(),
                            Vec::new(),
                        ),
                        SIRInstruction::Imm(RegisterId(1), SIRValue::new_four_state(value, mask)),
                        SIRInstruction::Store(
                            destination,
                            store_offset,
                            width,
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
                        (RegisterId(0), RegisterType::Logic { width: 128 }),
                        (
                            RegisterId(1),
                            if source_logic {
                                RegisterType::Logic { width }
                            } else {
                                RegisterType::Bit {
                                    width,
                                    signed: false,
                                }
                            },
                        ),
                    ]
                    .into_iter()
                    .collect(),
                };
                let memory = run_wasm(&unit, &layout(128, four_state), four_state);
                let replace =
                    |old: u128, new: u128| (old & !(field_mask << offset)) | (new << offset);
                assert_eq!(
                    read_bits(&memory, 0, 128),
                    BigUint::from(replace(initial, value)),
                    "offset={offset}, width={width}, four_state={four_state}, packed_elements={packed_elements}"
                );
                if four_state {
                    assert_eq!(
                        read_bits_at(&memory, OUTPUT_OFFSET + 16, 0, 128),
                        BigUint::from(replace(initial_mask, mask)),
                        "mask at offset={offset}, width={width}, packed_elements={packed_elements}"
                    );
                }
            }
        }
    }
}

fn assert_results(cases: &[CountCase], memory: &[u8]) {
    for (case_index, case) in cases.iter().enumerate() {
        for (op_index, expected) in case.expected.iter().copied().enumerate() {
            let result_index = case_index * 3 + op_index;
            assert_eq!(
                read_bits(memory, result_index * SLOT_BITS, case.destination_width),
                BigUint::from(expected),
                "case {case_index}, operation {op_index}",
            );
        }
    }
}

#[test]
fn bit_counts_match_for_narrow_and_wide_two_state_values() {
    let cases = vec![
        CountCase {
            source_width: 1,
            destination_width: 1,
            value: BigUint::from(0u8),
            mask: BigUint::from(0u8),
            expected: [0, 1, 1],
        },
        CountCase {
            source_width: 5,
            destination_width: 3,
            value: BigUint::from(0b0_0100u8),
            mask: BigUint::from(0u8),
            expected: [1, 2, 2],
        },
        CountCase {
            source_width: 63,
            destination_width: 6,
            value: (BigUint::from(1u8) << 62) | (BigUint::from(1u8) << 7),
            mask: BigUint::from(0u8),
            expected: [2, 0, 7],
        },
        CountCase {
            source_width: 64,
            destination_width: 7,
            value: BigUint::from(0u8),
            mask: BigUint::from(0u8),
            expected: [0, 64, 64],
        },
        CountCase {
            source_width: 65,
            destination_width: 7,
            value: (BigUint::from(1u8) << 64) | (BigUint::from(1u8) << 40),
            mask: BigUint::from(0u8),
            expected: [2, 0, 40],
        },
        CountCase {
            source_width: 70,
            destination_width: 80,
            value: BigUint::from(0u8),
            mask: BigUint::from(0u8),
            expected: [0, 70, 70],
        },
        CountCase {
            source_width: 130,
            destination_width: 8,
            value: (BigUint::from(1u8) << 128)
                | (BigUint::from(1u8) << 65)
                | (BigUint::from(1u8) << 3),
            mask: BigUint::from(0u8),
            expected: [3, 1, 3],
        },
    ];
    let variable_width = cases.len() * 3 * SLOT_BITS;
    let layout = layout(variable_width, false);
    let unit = execution_unit(&cases);

    assert_results(&cases, &run_cranelift(&unit, &layout, false));
    assert_results(&cases, &run_wasm(&unit, &layout, false));
}

#[test]
fn bit_counts_propagate_unknown_as_full_destination_mask() {
    let cases = vec![
        CountCase {
            source_width: 5,
            destination_width: 3,
            value: BigUint::from(0b0_0100u8),
            mask: BigUint::from(0u8),
            expected: [1, 2, 2],
        },
        CountCase {
            source_width: 5,
            destination_width: 3,
            value: BigUint::from(0u8),
            mask: BigUint::from(1u8) << 2,
            expected: [0, 0, 0],
        },
        CountCase {
            source_width: 70,
            destination_width: 80,
            value: BigUint::from(0u8),
            mask: BigUint::from(1u8) << 66,
            expected: [0, 0, 0],
        },
    ];
    let variable_width = cases.len() * 3 * SLOT_BITS;
    let layout = layout(variable_width, true);
    let unit = execution_unit(&cases);
    let value_bytes = get_byte_size(variable_width);

    for memory in [
        run_cranelift(&unit, &layout, true),
        run_wasm(&unit, &layout, true),
    ] {
        for (case_index, case) in cases.iter().enumerate() {
            let expected_mask = if case.mask == BigUint::from(0u8) {
                BigUint::from(0u8)
            } else {
                (BigUint::from(1u8) << case.destination_width) - BigUint::from(1u8)
            };
            for (op_index, expected_count) in case.expected.iter().copied().enumerate() {
                let result_index = case_index * 3 + op_index;
                let bit_offset = result_index * SLOT_BITS;
                let expected_value = if expected_mask == BigUint::from(0u8) {
                    BigUint::from(expected_count)
                } else {
                    expected_mask.clone()
                };
                assert_eq!(
                    read_bits(&memory, bit_offset, case.destination_width),
                    expected_value,
                    "case {case_index}, operation {op_index} value",
                );
                assert_eq!(
                    read_bits_at(
                        &memory,
                        OUTPUT_OFFSET + value_bytes,
                        bit_offset,
                        case.destination_width,
                    ),
                    expected_mask,
                    "case {case_index}, operation {op_index} mask",
                );
            }
        }
    }
}

#[test]
fn wide_sar_uses_the_lhs_width_for_sign_fill() {
    let lhs = RegisterId(0);
    let rhs = RegisterId(1);
    let dst = RegisterId(2);
    let mut register_map = HashMap::default();
    register_map.insert(
        lhs,
        RegisterType::Bit {
            width: 65,
            signed: true,
        },
    );
    register_map.insert(
        rhs,
        RegisterType::Bit {
            width: 8,
            signed: false,
        },
    );
    register_map.insert(
        dst,
        RegisterType::Bit {
            width: 128,
            signed: true,
        },
    );
    let block = BasicBlock {
        id: BlockId(0),
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(
                lhs,
                SIRValue::new_four_state(
                    BigUint::from(1u8) << 64usize,
                    BigUint::from(1u8) << 64usize,
                ),
            ),
            SIRInstruction::Imm(rhs, SIRValue::new(1u8)),
            SIRInstruction::Binary(dst, lhs, BinaryOp::Sar, rhs),
            SIRInstruction::Store(
                RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address()),
                SIROffset::Static(0),
                128,
                dst,
                Vec::new(),
                Vec::new(),
            ),
        ],
        terminator: SIRTerminator::Return,
    };
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks: std::iter::once((BlockId(0), block)).collect(),
        register_map,
    };
    let two_state_layout = layout(128, false);
    // IEEE 1800-2023 11.4.10: the 65-bit logical sign extends through
    // bit 127 before shifting. Every output bit from 63 upward is one.
    let expected =
        ((BigUint::from(1u8) << 128usize) - 1u8) ^ ((BigUint::from(1u8) << 63usize) - 1u8);

    let cranelift = read_bits(&run_cranelift(&unit, &two_state_layout, false), 0, 128);
    let wasm = read_bits(&run_wasm(&unit, &two_state_layout, false), 0, 128);
    assert_eq!(cranelift, expected);
    assert_eq!(wasm, expected);

    let four_state_layout = layout(128, true);
    for memory in [
        run_cranelift(&unit, &four_state_layout, true),
        run_wasm(&unit, &four_state_layout, true),
    ] {
        assert_eq!(read_bits(&memory, 0, 128), expected);
        assert_eq!(
            read_bits_at(&memory, OUTPUT_OFFSET + get_byte_size(128), 0, 128),
            expected,
        );
    }
}
