#[path = "../examples/affine_support/mod.rs"]
#[allow(dead_code)]
mod affine_support;
#[path = "../examples/affine_veryl_support/mod.rs"]
#[allow(dead_code)]
mod frontend;
#[path = "../examples/affine_region_support/mod.rs"]
mod state;

use affine_support::address;
use celox_design::RegionedStateAddr;
use celox_sir::{
    BinaryOp, BlockId, DomainKind, ExecutionUnit, RegisterId, SIRBuilder, SIRInstruction,
    SIROffset, SIRTerminator, SIRValue, TriggerIdWithKind,
    affine::{CodegenOptions, MemoryObject, recover_independent_regions},
};
use num_bigint::BigUint;
use num_traits::ToPrimitive;
use state::Machine;

fn finish(mut builder: SIRBuilder<RegionedStateAddr>) -> ExecutionUnit<RegionedStateAddr> {
    let (blocks, register_map, _) = builder.drain();
    let unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    unit.verify();
    unit
}

fn stores(builder: &mut SIRBuilder<RegionedStateAddr>, n: usize, bias: RegisterId) {
    for i in 0..n {
        let input = builder.alloc_logic(32);
        let value = builder.alloc_logic(32);
        builder.emit(SIRInstruction::Load(
            input,
            address(0),
            SIROffset::Static(32 * i),
            32,
        ));
        builder.emit(SIRInstruction::Binary(value, input, BinaryOp::Add, bias));
        builder.emit(SIRInstruction::Store(
            address(1),
            SIROffset::Static(32 * i),
            32,
            value,
            vec![],
            vec![],
        ));
    }
}

#[test]
fn splicing_preserves_branch_arguments_commits_events_and_capture_order() {
    let n = 19;
    let mut builder = SIRBuilder::new_with(91); // sparse register numbering
    let bias = builder.alloc_logic(32);
    builder.emit(SIRInstruction::Imm(bias, SIRValue::new(3u32)));
    let cond = builder.alloc_bit(1, false);
    builder.emit(SIRInstruction::Load(
        cond,
        address(2),
        SIROffset::Static(0),
        1,
    ));
    let yes = builder.new_block();
    let no = builder.new_block();
    let result = builder.alloc_logic(32);
    let join = builder.new_block_with(vec![result]);
    builder.seal_block(SIRTerminator::Branch {
        cond,
        true_block: (yes, vec![]),
        false_block: (no, vec![]),
    });
    builder.switch_to_block(yes);
    stores(&mut builder, n, bias);
    builder.emit(SIRInstruction::Commit(
        address(1),
        address(0),
        SIROffset::Static(0),
        32 * n,
        vec![TriggerIdWithKind {
            kind: DomainKind::Other,
            id: 3,
        }],
    ));
    builder.emit(SIRInstruction::RuntimeEvent {
        site_id: 4,
        args: vec![bias],
    });
    stores(&mut builder, n, bias);
    builder.emit(SIRInstruction::Store(
        address(2),
        SIROffset::Static(8),
        8,
        bias,
        vec![],
        vec![7],
    ));
    builder.seal_block(SIRTerminator::Jump(join, vec![bias]));
    builder.switch_to_block(no);
    stores(&mut builder, n, bias);
    builder.seal_block(SIRTerminator::Jump(join, vec![bias]));
    builder.switch_to_block(join);
    builder.emit(SIRInstruction::CombCaptureEvent {
        site_id: 7,
        args: vec![result],
        fatal_error_code: None,
        consume_enabled: true,
    });
    builder.seal_block(SIRTerminator::Return);
    let original = finish(builder);
    let objects = (0..3)
        .map(|id| {
            (
                address(id),
                MemoryObject {
                    element_width: 32,
                    elements: if id == 2 { 1 } else { n },
                },
            )
        })
        .collect();
    let found = recover_independent_regions(&original, &objects, &Default::default()).unwrap();
    assert_eq!(found.len(), 3);
    assert_eq!(found.store_count(), 3 * n);
    assert!(found.rejections.is_empty());
    for (unroll, selected) in [
        (1, vec![0, 1, 2]),
        (7, vec![0]),
        (32, vec![1, 2]),
        (7, vec![]),
    ] {
        let candidate = found
            .lower_selected_with_options(
                &selected,
                &Default::default(),
                &CodegenOptions {
                    unroll,
                    ..Default::default()
                },
            )
            .unwrap();
        for four_state in [false, true] {
            for seed in 1..=8 {
                let mut expected = Machine::new(&objects, seed, four_state && seed > 4);
                expected.values.insert(address(2), SIRValue::new(seed & 1));
                let initial = expected.clone();
                let mut actual = initial.clone();
                celox::execute_unit(&original, &mut expected, &[], four_state).unwrap();
                celox::execute_unit(&candidate, &mut actual, &[], four_state).unwrap();
                assert_eq!(actual, expected);
                if seed <= 4 {
                    let increment = if seed & 1 == 1 { 6u32 } else { 3 };
                    for i in 0..n {
                        let input = ((&initial.values[&address(0)].payload >> (32 * i))
                            & state::mask(32))
                        .to_u32()
                        .unwrap();
                        let output = ((&actual.values[&address(1)].payload >> (32 * i))
                            & state::mask(32))
                        .to_u32()
                        .unwrap();
                        assert_eq!(output, input.wrapping_add(increment));
                    }
                }
            }
        }
    }
    assert!(
        found
            .lower_with_options(
                &Default::default(),
                &CodegenOptions {
                    max_instructions: 1,
                    ..Default::default()
                }
            )
            .is_err()
    );
    original.verify();
}

#[test]
fn existing_counter_and_axi_units_preserve_full_state_and_effects() {
    for (code, top) in [
        (include_str!("../testdata/veryl/top_n1000.veryl"), "Top"),
        (
            include_str!("fixtures/bitslice/axi_lite_reg_file.veryl"),
            "AxiLiteRegFile",
        ),
    ] {
        for four_state in [false, true] {
            let (program, trace, _) = frontend::compile_top(code, top, four_state);
            let objects = frontend::objects(&program);
            let pre = &trace.pre_optimized_sir.as_ref().unwrap().sir;
            let reset =
                RegionedStateAddr::from_absolute_addr(0, program.get_addr(&[], &["rst"]).unwrap());
            let units = pre
                .eval_comb
                .iter()
                .chain(pre.eval_apply_ffs.values().flatten())
                .chain(pre.eval_comb_apply_ffs.values().flatten())
                .chain(pre.eval_only_ffs.values().flatten())
                .chain(pre.apply_ffs.values().flatten());
            let mut count = 0;
            for unit in units {
                let found =
                    recover_independent_regions(unit, &objects, &Default::default()).unwrap();
                if found.is_empty() {
                    continue;
                }
                count += 1;
                for unroll in [7, 32] {
                    let candidate = found
                        .lower_with_options(
                            &Default::default(),
                            &CodegenOptions {
                                unroll,
                                ..Default::default()
                            },
                        )
                        .unwrap();
                    candidate.verify();
                    for seed in 0..8 {
                        let mut initial = Machine::new(&objects, seed + 1, four_state && seed >= 4);
                        initial.values.insert(reset, SIRValue::new(seed % 2));
                        let mut expected = initial.clone();
                        let mut actual = initial;
                        celox::execute_unit(unit, &mut expected, &[], four_state).unwrap();
                        celox::execute_unit(&candidate, &mut actual, &[], four_state).unwrap();
                        assert_eq!(
                            actual, expected,
                            "{top} four_state={four_state} seed={seed}"
                        );
                    }
                }
            }
            assert_eq!(count, 6, "{top}: expected six alternative execution units");
        }
    }
}

#[test]
fn counter_native_matches_interpreter_through_reset_and_overflow() {
    for four_state in [false, true] {
        let (program, trace, _) = frontend::compile_top(
            include_str!("../testdata/veryl/top_n1000.veryl"),
            "Top",
            four_state,
        );
        let objects = frontend::objects(&program);
        let clk = program.get_addr(&[], &["clk"]).unwrap();
        let reset =
            RegionedStateAddr::from_absolute_addr(0, program.get_addr(&[], &["rst"]).unwrap());
        let counter =
            RegionedStateAddr::from_absolute_addr(0, program.get_addr(&[], &["cnt"]).unwrap());
        let pre = &trace.pre_optimized_sir.as_ref().unwrap().sir.eval_apply_ffs[&clk][0];
        let found = recover_independent_regions(pre, &objects, &Default::default()).unwrap();
        assert_eq!(found.len(), 2);
        for unroll in [16, 32] {
            let candidate = found
                .lower_with_options(
                    &Default::default(),
                    &CodegenOptions {
                        unroll,
                        ..Default::default()
                    },
                )
                .unwrap();
            #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
            let (layout, native_objects, mut native, mut baseline) = {
                use celox_state_layout::{MemoryLayout, MemoryLayoutMode};
                let layout = MemoryLayout::build(&program, four_state, MemoryLayoutMode::Packed);
                let native_objects = state::native::unit_objects(pre, &objects, &layout);
                let optimized = state::optimize_clock_candidate(
                    trace.pre_optimized_sir.as_ref().unwrap(),
                    &program,
                    clk,
                    candidate.clone(),
                    four_state,
                );
                state::native::unit_objects(&optimized, &objects, &layout);
                let native = affine_support::native::Executable::new(&optimized, &layout);
                let baseline = affine_support::native::Executable::new(
                    &program.sir.eval_apply_ffs[&clk][0],
                    &layout,
                );
                (layout, native_objects, native, baseline)
            };
            for unknown in [false, true].into_iter().filter(|v| !*v || four_state) {
                let mut expected = Machine::new(&objects, 271, unknown);
                if !unknown {
                    expected.values.get_mut(&counter).unwrap().payload = state::mask(32_000);
                }
                let mut actual = expected.clone();
                // Includes overflow, reset, repeated state transitions and all reset encodings.
                for (rst, rst_mask) in [
                    (1, 0),
                    (1, 0),
                    (0, 0),
                    (1, 0),
                    (1, u32::from(unknown)),
                    (0, u32::from(unknown)),
                    (1, 0),
                ] {
                    let reset_value = SIRValue::new_four_state(rst as u32, rst_mask);
                    expected.values.insert(reset, reset_value.clone());
                    actual.values.insert(reset, reset_value);
                    let old = expected.values[&counter].clone();
                    #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
                    {
                        let input = expected.restricted(&native_objects);
                        state::native::initialize(&mut native, &layout, &input);
                        state::native::initialize(&mut baseline, &layout, &input);
                    }
                    celox::execute_unit(pre, &mut expected, &[], four_state).unwrap();
                    celox::execute_unit(&candidate, &mut actual, &[], four_state).unwrap();
                    assert_eq!(actual, expected);
                    if !unknown {
                        let mut oracle = BigUint::default();
                        for i in 0usize..1000 {
                            let value = ((&old.payload >> (32 * i)) & state::mask(32))
                                .to_u32()
                                .unwrap();
                            oracle |=
                                BigUint::from(if rst == 0 { 0 } else { value.wrapping_add(1) })
                                    << (32 * i);
                        }
                        assert_eq!(expected.values[&counter].payload, oracle);
                    }
                    #[cfg(all(feature = "host-runtime", target_arch = "x86_64"))]
                    {
                        native.run();
                        baseline.run();
                        let output = expected.restricted(&native_objects);
                        state::native::assert_stable(&native, &layout, &output);
                        state::native::assert_stable(&baseline, &layout, &output);
                        assert_eq!(
                            &native.state[..layout.total_size.div_ceil(8)],
                            &baseline.state[..layout.total_size.div_ceil(8)]
                        );
                    }
                }
            }
        }
    }
}
