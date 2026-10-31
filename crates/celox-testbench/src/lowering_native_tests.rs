//! Execute lowered expressions through the HDL native pipeline.
use super::*;
use celox_backend_x86::native::{emit, jit_mem::JitCode, scalar_pipeline};
use celox_design::{InstanceId, RegionedStateAddr, STABLE_REGION, StateAddr, StateObjectId};
use celox_state_layout::{
    LayoutInput, LayoutSource, MemoryLayout, MemoryLayoutMode, StateObjectLayout,
};

struct Objects(Vec<StateObjectLayout<StateAddr>>);
impl LayoutSource<StateAddr> for Objects {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<StateAddr> {
        LayoutInput {
            state_objects: self.0.clone(),
            working_addresses: vec![],
            sparse_addresses: vec![],
            unpacked_arrays: Default::default(),
            requirements: Default::default(),
            ff_referenced_addresses: Default::default(),
            num_events: 0,
            runtime_event_sites: vec![],
            lane_writers: Default::default(),
            access_rank: Default::default(),
        }
    }
}
fn address(id: u32) -> StateAddr {
    StateAddr {
        instance_id: InstanceId(0),
        var_id: StateObjectId(id),
    }
}
pub(crate) fn evaluate(ops: Vec<ExprOpcode<StateLocation<u32>>>, input: &[u8]) -> BigUint {
    evaluate_with_state(ops, input).0
}
fn evaluate_with_state(
    ops: Vec<ExprOpcode<StateLocation<u32>>>,
    input: &[u8],
) -> (BigUint, Vec<u8>) {
    let mut builder = SIRBuilder::new();
    let result = Expressions {
        builder: &mut builder,
    }
    .compile(&ExprBytecode::new(ops))
    .unwrap();
    let width = builder.register(&result.register).width();
    builder.emit(Inst::Store(
        0,
        SIROffset::Static(0),
        width,
        result.register,
        vec![],
        vec![],
    ));
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    let unit = celox_sir::ExecutionUnit {
        entry_block_id: celox_sir::BlockId(0),
        blocks,
        register_map,
    }
    .into_map_addr(&mut |id| RegionedStateAddr::from_absolute_addr(STABLE_REGION, address(id)));
    let source = Objects(vec![
        StateObjectLayout {
            address: address(0),
            width,
            is_4state: false,
        },
        StateObjectLayout {
            address: address(1),
            width: (input.len() * 8).max(64),
            is_4state: false,
        },
    ]);
    let layout = MemoryLayout::build(&source, false, MemoryLayoutMode::Packed);
    let prepared =
        scalar_pipeline::prepare_scalar_eu(&unit, &layout, false, "testbench-expression").unwrap();
    let code = emit::emit(
        &prepared.function,
        &prepared.allocation,
        prepared.spill_frame_size,
    )
    .unwrap();
    let jit = JitCode::new(&code.code).unwrap();
    let mut state = vec![0u128; (code.required_state_size as usize).div_ceil(16)];
    // The aligned allocation includes the complete native spill arena. Both
    // semantic objects were allocated by the ordinary HDL state layout.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(state.as_mut_ptr().cast::<u8>(), state.len() * 16)
    };
    let table = [celox_sir::float::evaluate as *const () as usize];
    let pointer = table.as_ptr().wrapping_add(1) as u64;
    let header = celox_state_layout::STATE_HEADER_EXTERN_FUNCTIONS_ADDR_OFFSET;
    bytes[header..header + 8].copy_from_slice(&pointer.to_le_bytes());
    let offset = layout.offsets[&address(1)];
    bytes[offset..offset + input.len()].copy_from_slice(input);
    unsafe {
        (jit.fn_ptr)(bytes.as_mut_ptr());
    }
    let offset = layout.offsets[&address(0)];
    (
        BigUint::from_bytes_le(&bytes[offset..offset + width.div_ceil(8)]),
        bytes[layout.offsets[&address(1)]..layout.offsets[&address(1)] + input.len()].to_vec(),
    )
}
fn operation(op: Op, width: usize, signed: bool) -> ExprOpcode<StateLocation<u32>> {
    ExprOpcode::TypedBinOp {
        op,
        lhs_width: width,
        rhs_width: width,
        result_width: width,
        lhs_signed: signed,
        rhs_signed: signed,
    }
}
#[test]
fn wide_native_multiply_preserves_high_limbs() {
    let lhs = (BigUint::from(1u8) << 171) + BigUint::from(17u8);
    let rhs = (BigUint::from(1u8) << 67) + BigUint::from(3u8);
    let expected = (&lhs * &rhs) & ((BigUint::from(1u8) << 256) - BigUint::from(1u8));
    assert_eq!(
        evaluate(
            vec![
                ExprOpcode::ConstWide(lhs),
                ExprOpcode::ConstWide(rhs),
                operation(Op::Mul, 256, false)
            ],
            &[]
        ),
        expected
    );
}
#[test]
fn native_signed_division_and_comparison() {
    assert_eq!(
        evaluate(
            vec![
                ExprOpcode::ConstU64(0xf9),
                ExprOpcode::ConstU64(2),
                operation(Op::Div, 8, true)
            ],
            &[]
        ),
        BigUint::from(0xfdu8)
    );
    assert_eq!(
        evaluate(
            vec![
                ExprOpcode::ConstU64(0xf9),
                ExprOpcode::ConstU64(2),
                operation(Op::Less, 8, true)
            ],
            &[]
        ),
        BigUint::from(1u8)
    );
}
#[test]
fn native_float_multiply() {
    assert_eq!(
        evaluate(
            vec![
                ExprOpcode::ConstU64(1.5f64.to_bits()),
                ExprOpcode::ConstU64((-2.0f64).to_bits()),
                ExprOpcode::FloatBinary {
                    op: celox_design::FloatBinaryOp::Mul,
                    precision: celox_design::FloatPrecision::F64
                }
            ],
            &[]
        ),
        BigUint::from((-3.0f64).to_bits())
    );
}
#[test]
fn bit_selection_does_not_read_the_next_object() {
    for shift in [4, 8, 63, 64, 65, u64::MAX] {
        let expected = if shift == 4 { 0xau8 } else { 0 };
        assert_eq!(
            evaluate(
                vec![
                    ExprOpcode::ConstU64(shift),
                    ExprOpcode::LoadBitSelect {
                        location: StateLocation {
                            address: 1,
                            byte_offset: 0
                        },
                        base_byte_size: 1,
                        select_width: 8
                    }
                ],
                &[0xa5, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
            ),
            BigUint::from(expected)
        );
    }
}
#[test]
fn out_of_bounds_string_index_returns_empty_handle() {
    assert_eq!(
        evaluate(
            vec![
                ExprOpcode::ConstU64(u64::MAX),
                ExprOpcode::LoadString {
                    location: StateLocation {
                        address: 1,
                        byte_offset: 0
                    },
                    elements: 1
                }
            ],
            &[0xff; 8]
        ),
        BigUint::ZERO
    );
}

#[test]
fn native_typed_arithmetic_matches_expression_contract() {
    use crate::value::eval_typed_binop;
    for width in [1usize, 8, 33, 64, 65, 129] {
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        for (lhs, rhs) in [
            (mask.clone(), BigUint::from(2u8)),
            (BigUint::from(7u8), BigUint::ZERO),
        ] {
            for signed in [false, true] {
                for op in [
                    Op::Add,
                    Op::Sub,
                    Op::Mul,
                    Op::Div,
                    Op::Rem,
                    Op::Eq,
                    Op::Ne,
                    Op::Less,
                    Op::LessEq,
                    Op::Greater,
                    Op::GreaterEq,
                    Op::BitAnd,
                    Op::BitOr,
                    Op::BitXor,
                    Op::BitNand,
                    Op::BitNor,
                    Op::BitXnor,
                    Op::LogicAnd,
                    Op::LogicOr,
                    Op::LogicShiftL,
                    Op::LogicShiftR,
                    Op::ArithShiftR,
                ] {
                    let expected = eval_typed_binop(
                        crate::TestbenchValue::Wide(lhs.clone()),
                        op,
                        crate::TestbenchValue::Wide(rhs.clone()),
                        width,
                        width,
                        width,
                        signed,
                        signed,
                    )
                    .to_biguint();
                    let actual = evaluate(
                        vec![
                            ExprOpcode::ConstWide(lhs.clone()),
                            ExprOpcode::ConstWide(rhs.clone()),
                            operation(op, width, signed),
                        ],
                        &[],
                    );
                    assert_eq!(
                        actual, expected,
                        "{op:?}, width={width}, signed={signed}, lhs={lhs}, rhs={rhs}"
                    );
                }
            }
        }
    }
}

#[test]
fn conditional_kernel_writes_only_the_selected_branch() {
    let loc = |byte_offset| StateLocation {
        address: 1,
        byte_offset,
    };
    let ops = vec![
        ExprOpcode::ConstU64(100),
        ExprOpcode::LoadU64 {
            location: loc(0),
            byte_size: 1,
            mask: 1,
        },
        ExprOpcode::Ternary {
            then_len: 4,
            else_len: 4,
        },
        ExprOpcode::ConstU64(11),
        ExprOpcode::StoreU64 {
            location: loc(1),
            byte_size: 1,
        },
        ExprOpcode::ConstU64(20),
        ExprOpcode::Resize {
            source_width: 8,
            target_width: 129,
            signed: false,
        },
        ExprOpcode::ConstU64(22),
        ExprOpcode::StoreU64 {
            location: loc(2),
            byte_size: 1,
        },
        ExprOpcode::ConstU64(30),
        ExprOpcode::Resize {
            source_width: 8,
            target_width: 129,
            signed: false,
        },
        ExprOpcode::BinOp(Op::Add),
    ];
    for condition in [0, 1] {
        let (value, state) = evaluate_with_state(ops.clone(), &[condition, 0, 0]);
        assert_eq!(
            value,
            BigUint::from(if condition == 1 { 120u64 } else { 130 })
        );
        assert_eq!(
            state,
            if condition == 1 {
                vec![1, 11, 0]
            } else {
                vec![0, 0, 22]
            }
        );
    }
}

#[test]
fn string_handles_use_the_shared_heap_width() {
    let location = StateLocation {
        address: 1,
        byte_offset: 0,
    };
    let input = [0x12, 0, 0, 0, 0x34, 0, 0, 0];
    for (index, expected) in [(0, 0x12u8), (1, 0x34), (2, 0)] {
        assert_eq!(
            evaluate(
                vec![
                    ExprOpcode::ConstU64(index * celox_design::HANDLE_WIDTH as u64),
                    ExprOpcode::LoadString {
                        location,
                        elements: 2
                    },
                ],
                &input
            ),
            BigUint::from(expected)
        );
    }
    let (_, state) = evaluate_with_state(
        vec![
            ExprOpcode::ConstU64(0x56),
            ExprOpcode::StoreString { location },
            ExprOpcode::ConstU64(0),
        ],
        &input,
    );
    assert_eq!(state, [0x56, 0, 0, 0, 0x34, 0, 0, 0]);
}
