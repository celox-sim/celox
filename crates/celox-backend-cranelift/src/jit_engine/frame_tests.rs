use super::*;
use celox_state_layout::{LayoutInput, LayoutSource, MemoryLayoutMode};
use cranelift::codegen::ir::{StackSlotData, StackSlotKind};

struct EmptyLayout;
impl LayoutSource<crate::AbsoluteAddr> for EmptyLayout {
    fn layout_input(&self, _: MemoryLayoutMode) -> LayoutInput<crate::AbsoluteAddr> {
        LayoutInput {
            state_objects: vec![],
            working_addresses: vec![],
            sparse_addresses: vec![],
            unpacked_arrays: Default::default(),
            requirements: Default::default(),
            ff_referenced_addresses: Default::default(),
            num_events: 0,
            runtime_event_sites: vec![],
            lane_writers: Default::default(),
            access_rank: Default::default(),
            call_stack: None,
        }
    }
}

struct CostLayout;
impl LayoutSource<crate::AbsoluteAddr> for CostLayout {
    fn layout_input(&self, mode: MemoryLayoutMode) -> LayoutInput<crate::AbsoluteAddr> {
        let mut input = EmptyLayout.layout_input(mode);
        input.state_objects = (0..3)
            .map(|id| celox_state_layout::StateObjectLayout {
                address: crate::AbsoluteAddr {
                    instance_id: crate::InstanceId(0),
                    var_id: celox_design::StateObjectId(id),
                },
                width: 1024,
                is_4state: true,
            })
            .collect();
        input
    }
}

fn translated_values(
    engine: &mut JitEngine,
    unit: &crate::ExecutionUnit<RegionedAbsoluteAddr>,
) -> usize {
    let mut ctx = engine.module.make_context();
    define_simulation_function(&mut engine.module, &mut ctx);
    let mut builder_ctx = FunctionBuilderContext::new();
    let builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
    engine
        .translator
        .translate_units(std::slice::from_ref(unit), builder);
    ctx.func.dfg.num_values()
}

#[test]
fn operation_estimates_cover_actual_unoptimized_clif_values() {
    use crate::{
        BinaryOp, BlockId, HashMap, RegisterId, RegisterType, SIRInstruction, SIROffset,
        SIRTerminator, UnaryOp,
    };
    let mut violations = Vec::new();
    for four_state in [false, true] {
        let layout = MemoryLayout::build(&CostLayout, four_state, MemoryLayoutMode::Packed);
        let mut engine = JitEngine::new(
            layout,
            &CompileOptions {
                four_state,
                emit_triggers: false,
                cranelift: Default::default(),
            },
        )
        .unwrap();
        for width in [1, 8, 64, 65, 128, 257, 512] {
            for condition_width in [1, 65, 257] {
                let mut instructions = Vec::new();
                let mut register_map = HashMap::default();
                for (id, bits) in [(0, width), (1, width), (2, condition_width)] {
                    let address = crate::AbsoluteAddr {
                        instance_id: crate::InstanceId(0),
                        var_id: celox_design::StateObjectId(id as u32),
                    };
                    instructions.push(SIRInstruction::Load(
                        RegisterId(id),
                        RegionedAbsoluteAddr::from_absolute_addr(crate::STABLE_REGION, address),
                        SIROffset::Static(0),
                        bits,
                    ));
                    register_map.insert(RegisterId(id), RegisterType::Logic { width: bits });
                }
                let base = crate::ExecutionUnit {
                    entry_block_id: BlockId(0),
                    blocks: [(
                        BlockId(0),
                        crate::BasicBlock {
                            id: BlockId(0),
                            params: vec![],
                            instructions,
                            terminator: SIRTerminator::Return,
                        },
                    )]
                    .into_iter()
                    .collect(),
                    register_map,
                };
                let baseline = translated_values(&mut engine, &base);
                let mut operations = vec![(
                    SIRInstruction::Mux(RegisterId(3), RegisterId(2), RegisterId(0), RegisterId(1)),
                    width,
                )];
                if condition_width == 1 {
                    for op in [BinaryOp::And, BinaryOp::Or, BinaryOp::Xor] {
                        operations.push((
                            SIRInstruction::Binary(RegisterId(3), RegisterId(0), op, RegisterId(1)),
                            width,
                        ));
                    }
                    for op in [
                        UnaryOp::Ident,
                        UnaryOp::BitNot,
                        UnaryOp::ToTwoState,
                        UnaryOp::Minus,
                        UnaryOp::LogicNot,
                        UnaryOp::Or,
                        UnaryOp::And,
                        UnaryOp::Xor,
                        UnaryOp::PopCount,
                        UnaryOp::CountLeadingZeros,
                        UnaryOp::CountTrailingZeros,
                    ] {
                        let result_width = if matches!(
                            op,
                            UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::And | UnaryOp::Xor
                        ) {
                            1
                        } else {
                            width
                        };
                        operations.push((
                            SIRInstruction::Unary(RegisterId(3), op, RegisterId(0)),
                            result_width,
                        ));
                    }
                }
                for (instruction, result_width) in operations {
                    let mut unit = base.clone();
                    unit.register_map.insert(
                        RegisterId(3),
                        RegisterType::Logic {
                            width: result_width,
                        },
                    );
                    let estimate = crate::cost_model::estimate_clif_cost(
                        &instruction,
                        &unit.register_map,
                        four_state,
                    );
                    unit.blocks
                        .get_mut(&BlockId(0))
                        .unwrap()
                        .instructions
                        .push(instruction.clone());
                    let actual = translated_values(&mut engine, &unit) - baseline;
                    if estimate < actual {
                        violations.push(format!(
                                "{instruction:?}, width={width}, condition_width={condition_width}, four_state={four_state}: estimate {estimate} < actual {actual}"
                            ));
                    }
                }
            }
        }
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn outgoing_spills_only_reference_definitions_dominating_the_exit() {
    use crate::{
        BasicBlock, BlockId, HashMap, RegisterId, RegisterType, SIRInstruction, SIROffset,
        SIRTerminator, SIRValue,
    };
    for four_state in [false, true] {
        let address = crate::AbsoluteAddr {
            instance_id: crate::InstanceId(0),
            var_id: celox_design::StateObjectId(0),
        };
        let region = RegionedAbsoluteAddr::from_absolute_addr(crate::STABLE_REGION, address);
        let branch = |condition, yes, no| SIRTerminator::Branch {
            cond: RegisterId(condition),
            true_block: (BlockId(yes), vec![]),
            false_block: (BlockId(no), vec![]),
        };
        let blocks: HashMap<_, _> = [
            (
                0,
                (0..3)
                    .map(|index| {
                        SIRInstruction::Load(RegisterId(index), region, SIROffset::Static(index), 1)
                    })
                    .collect(),
                branch(0, 2, 1),
            ),
            (1, vec![], branch(2, 0, 4)),
            (
                2,
                vec![SIRInstruction::Imm(RegisterId(3), SIRValue::new(77u8))],
                branch(1, 3, 1),
            ),
            (
                3,
                vec![SIRInstruction::Store(
                    region,
                    SIROffset::Static(0),
                    8,
                    RegisterId(3),
                    vec![],
                    vec![],
                )],
                SIRTerminator::Jump(BlockId(4), vec![]),
            ),
            (4, vec![], SIRTerminator::Return),
        ]
        .into_iter()
        .map(|(id, instructions, terminator)| {
            (
                BlockId(id),
                BasicBlock {
                    id: BlockId(id),
                    params: vec![],
                    instructions,
                    terminator,
                },
            )
        })
        .collect();
        let unit = crate::ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map: (0..4)
                .map(|id| {
                    (
                        RegisterId(id),
                        RegisterType::Logic {
                            width: if id == 3 { 8 } else { 1 },
                        },
                    )
                })
                .collect(),
        };
        let threshold = crate::cost_model::estimate_eu_cost(&unit, four_state)
            - crate::cost_model::estimate_clif_cost(
                &unit.blocks[&BlockId(3)].instructions[0],
                &unit.register_map,
                four_state,
            )
            - 3;
        let plan = crate::tail_call_split::split_multi_block_with_threshold(
            &[unit],
            four_state,
            threshold,
            threshold,
        )
        .unwrap();
        let first = &plan.chunks[0];
        assert!(
            first.eu.blocks.contains_key(&BlockId(1)) && first.eu.blocks.contains_key(&BlockId(2))
        );
        assert!(
            first
                .outgoing_spills
                .iter()
                .any(|slot| slot.reg_id == RegisterId(3))
        );
        assert!(
            !first.outgoing_spills_at[&BlockId(1)]
                .iter()
                .any(|slot| slot.reg_id == RegisterId(3))
        );
        assert!(
            first.outgoing_spills_at[&BlockId(2)]
                .iter()
                .any(|slot| slot.reg_id == RegisterId(3))
        );
        let layout = MemoryLayout::build(&CostLayout, four_state, MemoryLayoutMode::Packed)
            .with_backend_scratch(plan.scratch_bytes);
        let mut engine = JitEngine::new(
            layout.clone(),
            &CompileOptions {
                four_state,
                emit_triggers: false,
                cranelift: crate::CraneliftOptions::fast_compile(),
            },
        )
        .unwrap();
        let entry = engine
            .compile_spilled_chunks(&plan, None, None, None)
            .unwrap();
        let run: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(entry) };
        for input in [0u8, 1, 2, 3] {
            let mut memory = vec![0u8; layout.merged_total_size];
            let offset = layout.offsets[&address];
            memory[offset] = input;
            assert_eq!(unsafe { run(memory.as_mut_ptr()) }, 0);
            assert_eq!(memory[offset], if input == 3 { 77 } else { input });
        }
    }
}

#[test]
fn excessive_actual_frame_returns_an_error_even_without_splitting() {
    for four_state in [false, true] {
        let mut options = CompileOptions {
            four_state,
            emit_triggers: false,
            cranelift: crate::CraneliftOptions::fast_compile(),
        };
        options.cranelift.tail_call_split = false;
        let layout = MemoryLayout::build(&EmptyLayout, four_state, MemoryLayoutMode::Packed);
        let mut engine = JitEngine::new(layout, &options).unwrap();
        let mut ctx = engine.module.make_context();
        define_simulation_function(&mut engine.module, &mut ctx);
        let mut builder_ctx = FunctionBuilderContext::new();
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut builder_ctx);
            let entry = builder.create_block();
            builder.append_block_params_for_function_params(entry);
            builder.switch_to_block(entry);
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                STACK_FRAME_LIMIT * 2,
                3,
            ));
            let value = builder.ins().iconst(types::I64, 7);
            let pointer_type = engine.module.target_config().pointer_type();
            builder.ins().stack_store(pointer_type, value, slot, 0);
            let loaded = builder.ins().stack_load(pointer_type, types::I64, slot, 0);
            builder.ins().return_(&[loaded]);
            builder.seal_all_blocks();
            builder.finalize(engine.module.target_config());
        }
        let id = engine
            .module
            .declare_anonymous_function(&ctx.func.signature)
            .unwrap();
        let error = engine
            .optimize_and_define(&mut ctx, id, "oversized frame", None, None, None)
            .unwrap_err();
        assert!(
            matches!(error, CraneliftError::StackFrameTooLarge { bytes, limit, .. } if bytes > limit && limit == STACK_FRAME_LIMIT)
        );
    }
}
