use super::super::test_programs::*;
use super::super::{RegallocResult, RegallocStrategy, is_cancellation, run_regalloc_for_codegen};
use crate::native::features::{StateBaseStrategy, X86Features};
use crate::native::mir::*;

const BASELINE: RegallocStrategy = RegallocStrategy::Baseline { spill_budget: 4096 };

/// Allocate with the baseline strategy, which must lower every phi.
fn allocate_baseline(func: &MFunction) -> (MFunction, RegallocResult) {
    let (allocated, result) = allocate(func, BASELINE);
    assert!(
        allocated.blocks.iter().all(|block| block.phis.is_empty()),
        "baseline allocation lowers every phi"
    );
    (allocated, result)
}

/// Allocate `func` with the baseline strategy and require the interpreter's
/// effects from its machine code.
fn assert_matches_reference(func: &MFunction, inputs: &[Vec<u8>], context: &str) {
    let (baseline, baseline_result) = allocate_baseline(func);
    for input in inputs {
        let expected = (0, interpret(func, input));
        assert_eq!(
            execute(&baseline, &baseline_result, input),
            expected,
            "baseline result differs ({context})\ninput MIR:\n{func}\nbaseline MIR:\n{baseline}"
        );
    }
}

#[test]
fn random_programs_match_reference_semantics() {
    for seed in 0..120 {
        for bmi2 in [true, false] {
            let func = random_function(seed, X86Features::for_test(bmi2));
            assert_matches_reference(
                &func,
                &random_inputs(seed),
                &format!("seed {seed}, bmi2 {bmi2}"),
            );
        }
    }
}

#[test]
fn random_programs_allocate_with_fifteen_registers() {
    for seed in 0..60 {
        let func = random_function(
            seed,
            X86Features::for_test_with_state_base(false, StateBaseStrategy::Gs),
        );
        allocate_baseline(&func);
    }
}

/// A straight-line function with more simultaneously live values than
/// registers, consumed in reverse order so every reload is exercised.
fn pressure_function(features: X86Features) -> MFunction {
    let mut builder = Builder::new();
    let values = (0..40)
        .map(|index| {
            let input = builder.input(index % INPUT_WORDS);
            builder.def(|dst| MInst::AddImm {
                dst,
                src: input,
                imm: index as i32,
            })
        })
        .collect::<Vec<_>>();
    let mut sum = values[39];
    for &value in values[..39].iter().rev() {
        sum = builder.def(|dst| MInst::Xor {
            dst,
            lhs: sum,
            rhs: value,
        });
        let shifted = builder.def(|dst| MInst::Shl {
            dst,
            lhs: sum,
            rhs: value,
        });
        sum = builder.def(|dst| MInst::Add {
            dst,
            lhs: sum,
            rhs: shifted,
        });
    }
    builder.output(0, sum);
    builder.push(MInst::Return);
    builder.finish(features)
}

#[test]
fn high_pressure_block_spills_and_reloads() {
    for bmi2 in [true, false] {
        let func = pressure_function(X86Features::for_test(bmi2));
        let (allocated, result) = allocate_baseline(&func);
        assert!(result.spill_frame_size > 0, "40 live values must spill");
        assert!(allocated.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::Load {
                base: BaseReg::StackFrame,
                ..
            }
        )));
        assert_matches_reference(&func, &random_inputs(7), &format!("bmi2 {bmi2}"));
    }
}

/// Values live across a division must leave RAX/RDX; values live across a
/// whole-file clobber must all leave their registers.
#[test]
fn clobbers_move_or_spill_live_values() {
    let mut builder = Builder::new();
    let values = (0..12)
        .map(|word| builder.input(word % INPUT_WORDS))
        .collect::<Vec<_>>();
    let divisor = builder.def(|dst| MInst::OrImm {
        dst,
        src: values[1],
        imm: 1,
    });
    let quotient = builder.def(|dst| MInst::UDiv {
        dst,
        lhs: values[0],
        rhs: divisor,
    });
    builder.output(0, quotient);
    builder.tables.push(vec![0; SparseCommitDescriptor::WORDS]);
    builder.push(MInst::SparseCommitWorklist {
        descriptor_table: ConstantTableId(0),
        active_bits_offset: 0,
        active_capacity: 1,
    });
    for (word, &value) in values.iter().enumerate() {
        builder.output(word + 1, value);
    }
    builder.push(MInst::Return);
    let func = builder.finish(X86Features::for_test(true));
    let (allocated, result) = allocate_baseline(&func);
    // Every value survives the whole-file clobber through a stack slot.
    assert!(result.spill_frame_size >= 8 * 12);
    assert!(allocated.verify_result().is_ok());
}

/// Two loop-carried phis exchange values every iteration; their homes are
/// written as one parallel copy.
#[test]
fn loop_carried_phi_swap_is_a_parallel_copy() {
    let mut builder = Builder::new();
    let a = builder.input(0);
    let b = builder.input(1);
    let zero = builder.imm(0);
    let (header, body, exit) = (builder.block(), builder.block(), builder.block());
    builder.push(MInst::Jump { target: header });
    let counter = builder.phi(header, vec![(BlockId(0), zero)]);
    let left = builder.phi(header, vec![(BlockId(0), a)]);
    let right = builder.phi(header, vec![(BlockId(0), b)]);
    builder.enter(header);
    builder.push(MInst::Jump { target: body });
    builder.enter(body);
    let next = builder.def(|dst| MInst::AddImm {
        dst,
        src: counter,
        imm: 1,
    });
    let again = builder.def(|dst| MInst::CmpImm {
        dst,
        lhs: next,
        imm: 4,
        kind: CmpKind::LtU,
    });
    for (node, source) in builder.blocks[header.0 as usize]
        .phis
        .iter_mut()
        .zip([next, right, left])
    {
        node.sources.push((body, source));
    }
    builder.push(MInst::Branch {
        cond: again,
        true_bb: header,
        false_bb: exit,
    });
    builder.enter(exit);
    builder.output(0, left);
    builder.output(1, right);
    builder.push(MInst::Return);
    let func = builder.finish(X86Features::for_test(true));
    let (allocated, result) = allocate_baseline(&func);
    let mut input = vec![0u8; STATE_BYTES];
    input[..8].copy_from_slice(&11u64.to_le_bytes());
    input[8..16].copy_from_slice(&22u64.to_le_bytes());
    let (status, state) = execute(&allocated, &result, &input);
    assert_eq!(status, 0);
    let word = |index: usize| {
        let offset = (INPUT_WORDS + index) * 8;
        u64::from_le_bytes(state[offset..offset + 8].try_into().unwrap())
    };
    // Four body iterations run three swaps, leaving the pair exchanged.
    assert_eq!((word(0), word(1)), (22, 11));
}

#[test]
fn cancelled_baseline_allocation_reports_cancellation() {
    let func = random_function(3, X86Features::for_test(true));
    let mut allocated = func.clone();
    let error = match run_regalloc_for_codegen(
        &mut allocated,
        "baseline_cancelled",
        None,
        &verified(),
        false,
        BASELINE,
        || true,
    ) {
        Ok(_) => panic!("a cancelled allocation must not return a result"),
        Err(error) => error,
    };
    assert!(is_cancellation(&error));
}

/// A loop whose carried phis rotate: every source is another phi of the
/// header, more of them than registers, so some go through scratch slots.
#[test]
fn wide_phi_rotation_reads_every_source_before_writing_homes() {
    const CARRIED: usize = 20;
    let mut builder = Builder::new();
    let initial = (0..CARRIED)
        .map(|index| {
            let input = builder.input(index % INPUT_WORDS);
            builder.def(|dst| MInst::AddImm {
                dst,
                src: input,
                imm: index as i32,
            })
        })
        .collect::<Vec<_>>();
    let zero = builder.imm(0);
    let (header, body, exit) = (builder.block(), builder.block(), builder.block());
    builder.push(MInst::Jump { target: header });
    let counter = builder.phi(header, vec![(BlockId(0), zero)]);
    let carried = initial
        .iter()
        .map(|&value| builder.phi(header, vec![(BlockId(0), value)]))
        .collect::<Vec<_>>();
    builder.enter(header);
    builder.push(MInst::Jump { target: body });
    builder.enter(body);
    let next = builder.def(|dst| MInst::AddImm {
        dst,
        src: counter,
        imm: 1,
    });
    let again = builder.def(|dst| MInst::CmpImm {
        dst,
        lhs: next,
        imm: 5,
        kind: CmpKind::LtU,
    });
    let sources = std::iter::once(next)
        .chain((0..CARRIED).map(|index| carried[(index + 1) % CARRIED]))
        .collect::<Vec<_>>();
    for (node, source) in builder.blocks[header.0 as usize]
        .phis
        .iter_mut()
        .zip(sources)
    {
        node.sources.push((body, source));
    }
    builder.push(MInst::Branch {
        cond: again,
        true_bb: header,
        false_bb: exit,
    });
    builder.enter(exit);
    for (word, &value) in carried.iter().enumerate().take(OUTPUT_WORDS) {
        builder.output(word, value);
    }
    builder.push(MInst::Return);
    for bmi2 in [true, false] {
        let func = builder_clone(&builder).finish(X86Features::for_test(bmi2));
        assert_matches_reference(&func, &random_inputs(11), "phi rotation");
    }
}

/// Many phis at one join, each fed by a different constant per arm.
#[test]
fn many_join_phis_store_every_row() {
    const PHIS: usize = 32;
    let mut builder = Builder::new();
    let selector = builder.input(0);
    let cond = builder.def(|dst| MInst::AndImm {
        dst,
        src: selector,
        imm: 1,
    });
    let (left, right, join) = (builder.block(), builder.block(), builder.block());
    builder.push(MInst::Branch {
        cond,
        true_bb: left,
        false_bb: right,
    });
    let mut arms = Vec::new();
    for (arm, base) in [(left, 1u64), (right, 101)] {
        builder.enter(arm);
        let values = (0..PHIS as u64)
            .map(|index| builder.imm(base + index))
            .collect::<Vec<_>>();
        builder.push(MInst::Jump { target: join });
        arms.push(values);
    }
    let merged = (0..PHIS)
        .map(|index| builder.phi(join, vec![(left, arms[0][index]), (right, arms[1][index])]))
        .collect::<Vec<_>>();
    builder.enter(join);
    let mut sum = merged[0];
    for &value in &merged[1..] {
        sum = builder.def(|dst| MInst::Add {
            dst,
            lhs: sum,
            rhs: value,
        });
    }
    builder.output(0, sum);
    builder.push(MInst::Return);
    let func = builder.finish(X86Features::for_test(true));
    let mut inputs = random_inputs(5);
    inputs[0][0] = 0;
    inputs[1][0] = 1;
    assert_matches_reference(&func, &inputs, "join phis");
}
