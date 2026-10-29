use super::*;
use crate::{OptLevel, OptimizeOptions};

fn address(index: u32) -> RegionedAbsoluteAddr {
    RegionedAbsoluteAddr::from_absolute_addr(
        STABLE_REGION,
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: celox_design::StateObjectId(index),
        },
    )
}

/// Two invocations separated by a return, entered through a dynamic resume
/// dispatch. The first contains constant arithmetic and an untaken branch;
/// the second must read shared state again and preserve every observable write.
fn process(four_state: bool) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut b = SIRBuilder::new();
    let start = b.new_block();
    let taken = b.new_block();
    let untaken = b.new_block();
    let resumed = b.new_block();
    let done = b.new_block();
    let resume = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Load(
        resume,
        address(0),
        SIROffset::Static(0),
        8,
    ));
    b.seal_block(SIRTerminator::Switch {
        selector: resume,
        cases: vec![
            SIRSwitchCase {
                value: 0u8.into(),
                target: start,
            },
            SIRSwitchCase {
                value: 1u8.into(),
                target: resumed,
            },
        ],
        default: done,
    });
    b.switch_to_block(start);
    let ty = if four_state {
        RegisterType::Logic { width: 8 }
    } else {
        RegisterType::Bit {
            width: 8,
            signed: false,
        }
    };
    let lhs = b.alloc_reg(ty.clone());
    let rhs = b.alloc_reg(ty.clone());
    let sum = b.alloc_reg(ty.clone());
    let dead = b.alloc_reg(ty.clone());
    b.emit(SIRInstruction::Imm(lhs, SIRValue::new(2u8)));
    b.emit(SIRInstruction::Imm(rhs, SIRValue::new(3u8)));
    b.emit(SIRInstruction::Binary(sum, lhs, BinaryOp::Add, rhs));
    b.emit(SIRInstruction::Binary(dead, lhs, BinaryOp::Mul, rhs));
    b.seal_block(SIRTerminator::Branch {
        cond: sum,
        true_block: (taken, Vec::new()),
        false_block: (untaken, Vec::new()),
    });
    b.switch_to_block(taken);
    b.emit(SIRInstruction::Store(
        address(1),
        SIROffset::Static(0),
        8,
        sum,
        Vec::new(),
        Vec::new(),
    ));
    // Save the continuation before returning to the scheduler.
    let next = b.alloc_bit(8, false);
    b.emit(SIRInstruction::Imm(next, SIRValue::new(1u8)));
    b.emit(SIRInstruction::Store(
        address(0),
        SIROffset::Static(0),
        8,
        next,
        Vec::new(),
        Vec::new(),
    ));
    b.seal_block(SIRTerminator::Return);
    b.switch_to_block(untaken);
    b.emit(SIRInstruction::RuntimeEvent {
        site_id: 99,
        args: Vec::new(),
    });
    b.seal_block(SIRTerminator::Error(99));
    b.switch_to_block(resumed);
    let before = b.alloc_reg(ty.clone());
    let after = b.alloc_reg(ty.clone());
    let unused_result = b.alloc_bit(32, false);
    b.emit(SIRInstruction::Load(
        before,
        address(1),
        SIROffset::Static(0),
        8,
    ));
    b.emit(SIRInstruction::Store(
        address(2),
        SIROffset::Static(0),
        8,
        before,
        Vec::new(),
        vec![7],
    ));
    b.emit(SIRInstruction::RuntimeEvent {
        site_id: 0,
        args: vec![before],
    });
    b.emit(SIRInstruction::ExternCall {
        dst: Some(unused_result),
        func: 0,
        args: Vec::new(),
    });
    b.emit(SIRInstruction::Load(
        after,
        address(1),
        SIROffset::Static(0),
        8,
    ));
    b.emit(SIRInstruction::Store(
        address(2),
        SIROffset::Static(0),
        8,
        after,
        Vec::new(),
        vec![7],
    ));
    b.seal_block(SIRTerminator::Return);
    b.switch_to_block(done);
    b.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = b.drain();
    ExecutionUnit {
        blocks,
        register_map,
        entry_block_id: BlockId(0),
    }
}

fn optimize_process(
    process: ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    optimize_options: OptimizeOptions,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let mut sir = SirProgram {
        processes: vec![process],
        eval_comb: Vec::new(),
        eval_apply_ffs: crate::HashMap::default(),
        eval_comb_apply_ffs: crate::HashMap::default(),
        merge_comb_apply: true,
        eval_comb_stateful: None,
        eval_only_ffs: crate::HashMap::default(),
        apply_ffs: crate::HashMap::default(),
        parallel: None,
    };
    let design = celox_design::ElaboratedDesign::default();
    let runtime_schema = celox_design::RuntimeSchema::default();
    let mut layout_requirements = celox_state_layout::LayoutRequirements::default();
    let mut context = OptimizationContext {
        sir: &mut sir,
        design: &design,
        runtime_schema: &runtime_schema,
        layout_requirements: &mut layout_requirements,
    };
    run(
        &mut context,
        &PassOptions {
            four_state,
            optimize_options,
            ..PassOptions::default()
        },
    );
    sir.processes.remove(0)
}

#[test]
fn processes_simplify_constants_without_changing_resume_dispatch_or_effects() {
    for four_state in [false, true] {
        let original = process(four_state);
        original.verify_result().unwrap();
        for opt in [
            OptimizeOptions::default(),
            OptimizeOptions::new(OptLevel::O2),
            OptimizeOptions::none().enable(SirPass::ControlFlowSimplify),
        ] {
            let mut opt = opt;
            opt.diagnostics.verify_passes = true;
            let optimized = optimize_process(original.clone(), four_state, opt);
            optimized.verify_result().unwrap();
            assert!(optimized.blocks.len() < original.blocks.len());
            assert_eq!(optimized.blocks[&BlockId(0)], original.blocks[&BlockId(0)]);
            // Both live continuation bodies retain their effects and order.
            assert_eq!(optimized.blocks[&BlockId(2)], original.blocks[&BlockId(2)]);
            assert_eq!(optimized.blocks[&BlockId(4)], original.blocks[&BlockId(4)]);
            assert!(!optimized.blocks.contains_key(&BlockId(3)));
            let instructions = &optimized.blocks[&BlockId(1)].instructions;
            assert_eq!(instructions.len(), 1);
            assert!(
                matches!(&instructions[0], SIRInstruction::Imm(_, value) if *value == SIRValue::new(5u8))
            );
        }
    }
}

#[test]
fn process_passes_respect_o0_and_individual_selectors() {
    let original = process(false);
    for opt in [
        OptimizeOptions::none(),
        OptimizeOptions::none().enable(SirPass::StoreLoadForwarding),
        OptimizeOptions::default()
            .disable(SirPass::ControlFlowSimplify)
            .disable(SirPass::Gvn),
    ] {
        assert_eq!(optimize_process(original.clone(), false, opt), original);
    }
    let optimized = optimize_process(
        original.clone(),
        false,
        OptimizeOptions::none().enable(SirPass::Gvn),
    );
    // DCE alone removes the unused multiply but leaves the dynamic dispatch,
    // branch, both branch arms, and all observable operations intact.
    assert_eq!(optimized.blocks.len(), original.blocks.len());
    assert_eq!(
        optimized.blocks[&BlockId(1)].instructions.len(),
        original.blocks[&BlockId(1)].instructions.len() - 1
    );
    assert!(
        optimized.blocks[&BlockId(1)]
            .instructions
            .iter()
            .all(|instruction| !matches!(
                instruction,
                SIRInstruction::Binary(_, _, BinaryOp::Mul, _)
            ))
    );
    assert_eq!(
        optimized.blocks[&BlockId(1)].terminator,
        original.blocks[&BlockId(1)].terminator
    );
    for index in [0, 2, 3, 4, 5] {
        assert_eq!(
            optimized.blocks[&BlockId(index)],
            original.blocks[&BlockId(index)]
        );
    }
}
