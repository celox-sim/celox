/// Sampled copies no unit reads are dropped from the runtime
/// schema, even though the frontend lists every copy among the
/// testbench read roots for the host refresh that writes them.
#[cfg(test)]
#[test]
fn unread_sample_copies_are_pruned_from_the_runtime_schema() {
    // The sampled copy of `dat` is read only by the check's monitor
    // process. Two-state mode drops that vacuous monitor, so the
    // copy must be pruned even though the frontend lists every
    // sampled copy among the testbench read roots (the host writes
    // the copies there; that bookkeeping is not a read). In
    // four-state mode the monitor stays and keeps the copy.
    let source = r#"`timescale 1ns/1ns
module Top;
 bit clk;
 logic [7:0] dat;
 always #1 clk = ~clk;
 always @(posedge clk) dat <= dat + 8'd1;
 check: assert property (@(posedge clk) ((^dat) !== 1'bx));
endmodule"#;
    let shadows = |four_state| {
        let (program, ..) = crate::Simulator::from_sv_sources(
            vec![(source, std::path::Path::new("prune-shadows.sv"))],
            "Top",
        )
        .four_state(four_state)
        .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, true, false)
        .unwrap();
        program.runtime().runtime_schema.sample_shadows.len()
    };
    assert_eq!(shadows(false), 0);
    assert!(shadows(true) > 0);
}

/// The traced build lays out its program itself, so it must prune
/// the same unread sampled copies as the laid-out path above.
#[cfg(test)]
#[test]
fn unread_sample_copies_are_pruned_in_traced_builds() {
    // A sampled copy no unit reads is pruned; the copy an output
    // reads stays.
    let shadows = |body: &str| {
        let source =
            format!("module Top(input logic [7:0] dat, output logic [7:0] q);\n{body}\nendmodule");
        crate::Simulator::from_sv_sources(
            vec![(
                source.as_str(),
                std::path::Path::new("prune-shadows-traced.sv"),
            )],
            "Top",
        )
        .build_with_trace()
        .unwrap()
        .sampled_copy_count()
    };
    assert_eq!(shadows(" always_comb begin $sampled(dat); q = dat; end"), 0);
    assert!(shadows(" always_comb q = $sampled(dat);") > 0);
}

/// Lane units that call functions share the call stack, so the lane
/// plan orders them, on one lane or by a wait; the call-free units of
/// the same kernel still spread over several lanes.
#[cfg(test)]
#[test]
fn lane_plans_order_the_units_that_call() {
    let source = r#"module Top(input logic clk, input logic [31:0] a, input logic [31:0] b,
           output logic [31:0] q0, output logic [31:0] q1, output logic [31:0] q2,
           output logic [31:0] q3, output logic [31:0] c0, output logic [31:0] c1);
    function automatic logic [31:0] mix(input logic [31:0] x, input logic [31:0] y);
        return (x ^ (y << 3)) + (x >> 2);
    endfunction
    always_ff @(posedge clk) q0 <= a + b;
    always_ff @(posedge clk) q1 <= a * 3;
    always_ff @(posedge clk) q2 <= b - a;
    always_ff @(posedge clk) q3 <= a ^ b;
    always_ff @(posedge clk) c0 <= mix(a, b);
    always_ff @(posedge clk) c1 <= mix(b, a);
endmodule"#;
    let (program, ..) =
        crate::Simulation::from_sv_sources(vec![(source, std::path::Path::new("lanes.sv"))], "Top")
            .force_out_of_line_calls()
            .threads(4)
            .parallel_partition(crate::ParallelPartition::Always)
            .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
            .unwrap();
    assert_eq!(program.sir.functions.len(), 1);
    for codegen in [
        celox_sir_opt::parallel::CodegenFootprint::Native,
        celox_sir_opt::parallel::CodegenFootprint::Cranelift,
    ] {
        let (_, planned) = crate::backend::lanes::plan_lane_kernels(&program, codegen).unwrap();
        assert!(!planned.is_empty(), "{codegen:?}: no kernel runs on lanes");
        for kernel in &planned {
            let units = kernel.units();
            let calls = |item: usize| {
                units[item].blocks.values().any(|block| {
                    block.instructions.iter().any(|instruction| {
                        matches!(instruction, crate::ir::SIRInstruction::Call { .. })
                    })
                })
            };
            let task_of = |item: usize| {
                kernel
                    .tasks
                    .iter()
                    .position(|task| task.items.contains(&item))
                    .expect("every item is in a task")
            };
            // The tasks each task runs after: earlier tasks of its lane,
            // and those its waits cover, transitively.
            let before = |task: usize| {
                let mut seen = std::collections::BTreeSet::new();
                let mut work = vec![task];
                while let Some(task) = work.pop() {
                    let current = &kernel.tasks[task];
                    for (other, candidate) in kernel.tasks.iter().enumerate() {
                        let earlier =
                            candidate.lane == current.lane && candidate.sequence < current.sequence;
                        let awaited = current.waits.iter().any(|wait| {
                            wait.lane == candidate.lane && candidate.sequence < wait.completed_tasks
                        });
                        if (earlier || awaited) && seen.insert(other) {
                            work.push(other);
                        }
                    }
                }
                seen
            };
            let calling: Vec<usize> = (0..units.len()).filter(|&item| calls(item)).collect();
            for (position, &first) in calling.iter().enumerate() {
                for &second in &calling[position + 1..] {
                    let (first, second) = (task_of(first), task_of(second));
                    assert!(
                        first == second
                            || before(second).contains(&first)
                            || before(first).contains(&second),
                        "{codegen:?}: calling units run unordered"
                    );
                }
            }
            let lanes: std::collections::BTreeSet<u32> = (0..units.len())
                .filter(|&item| !calls(item))
                .map(|item| kernel.tasks[task_of(item)].lane)
                .collect();
            assert!(lanes.len() > 1, "{codegen:?}: call-free units on {lanes:?}");
        }
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn hotspot_scheduler_releases_its_program_after_a_failed_stage() {
    use crate::backend::hotspot::{EntryKey, HotspotRuntime, SCRATCH_BYTES};
    let source = r#"`timescale 1ns/1ns
module Top;
  logic clk = 0;
  logic [7:0] q = 0;
  always #5 clk = ~clk;
  always_ff @(posedge clk) q <= q + 1;
endmodule"#;
    let (program, _, options, _, _) = crate::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("failed-scheduler.sv"))],
        "Top",
    )
    .emit_triggers()
    .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
    .unwrap();
    for rejected in [false, true] {
        let mut runtime = HotspotRuntime::new(program.layout(), &options).unwrap();
        runtime.prepare_scheduler(&program, None);
        assert!(runtime.holds_scheduler_program_for_test());
        runtime.finish_scheduler_for_test(program.layout(), rejected);
        let mut memory =
            vec![0; program.layout().merged_total_size.div_ceil(8) + SCRATCH_BYTES / 8];
        let poll = EntryKey::Block(0, usize::MAX, 0, crate::ir::BlockId(0));
        assert!(runtime.try_run(poll, &mut memory).is_none());
        let stats = runtime.stats();
        assert_eq!(
            (stats.failed_entries, stats.rejected_entries),
            if rejected { (0, 1) } else { (1, 0) }
        );
        assert!(!runtime.has_compiled_scheduler());
        assert!(
            !runtime.holds_scheduler_program_for_test(),
            "rejected={rejected}"
        );
    }
}

#[cfg(all(test, hotspot_jit))]
#[test]
fn hotspot_scheduler_overlaps_large_comb_and_links_after_publication() {
    use crate::backend::hotspot::{EntryKey, HotspotRuntime, SCRATCH_BYTES};
    use std::time::{Duration, Instant};
    let source = r#"`timescale 1ns/1ns
module Top;
  logic clk = 0;
  logic [129:0] q = 0;
  wire [129:0] observed = {q[129:128], q[127:0] ^ {128{clk}}};
  always #5 clk = ~clk;
  always_ff @(posedge clk) q <= {2'bxz, q[127:0] + 128'd1};
endmodule"#;
    let (program, _, options, _, _) = crate::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("early-scheduler.sv"))],
        "Top",
    )
    .emit_triggers()
    .four_state(true)
    .settle_partition(crate::SettlePartition::Always)
    .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
    .unwrap();
    assert!(!program.sir.eval_comb.is_empty());
    for large in [false, true] {
        for cancel_before_publication in [false, true] {
            let mut runtime = HotspotRuntime::new(program.layout(), &options).unwrap();
            runtime.prepare_scheduler(&program, None);
            let mut memory =
                vec![0; program.layout().merged_total_size.div_ceil(8) + SCRATCH_BYTES / 8];
            // Never execute a group against uninitialized simulation state.
            let poll = EntryKey::Block(0, usize::MAX, 0, crate::ir::BlockId(0));
            for _ in 0..128 {
                assert!(runtime.try_run(poll, &mut memory).is_none());
            }
            assert_eq!(runtime.stats().compile_time, Duration::ZERO);
            assert_eq!(runtime.stats().compiled_entries, 0);
            runtime.begin(EntryKey::Comb);
            runtime.finish(
                EntryKey::Comb,
                Some(Instant::now() - Duration::from_secs(1)),
                None,
            );
            let (_, mut units) = runtime.begin(EntryKey::Comb);
            assert!(units.is_some());
            if large {
                // Exercise size-based escalation without a huge test circuit.
                let mut instructions = usize::MAX;
                runtime.collect(
                    EntryKey::Comb,
                    &mut units,
                    &program.sir.eval_comb[0],
                    &mut instructions,
                );
                assert!(units.is_none());
                runtime.finish(EntryKey::Comb, None, units);
                let (_, units) = runtime.begin(EntryKey::Comb);
                assert!(units.is_some());
            }
            runtime.finish(EntryKey::Comb, None, Some(program.sir.eval_comb.clone()));
            // Large groups prepare while the worker compiles Comb: on the
            // owner with a single CPU, otherwise on their own thread.
            // Small groups retain lazy preparation on that worker.
            let synchronous = large && crate::backend::hotspot::single_cpu();
            assert_eq!(runtime.stats().compile_time.is_zero(), !synchronous);
            assert_eq!(runtime.stats().compiled_entries, 0);
            assert_eq!(runtime.stats().code_bytes, 0);
            assert_eq!(runtime.stats().failed_entries, 0);
            assert!(!runtime.has_compiled_scheduler());
            if cancel_before_publication {
                assert!(runtime.cancel());
                for _ in 0..128 {
                    assert!(runtime.try_run(poll, &mut memory).is_none());
                }
                assert_eq!(runtime.stats().compiled_entries, 0);
                assert!(!runtime.has_compiled_scheduler());
            } else {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !runtime.has_compiled_scheduler() {
                    assert!(runtime.try_run(poll, &mut memory).is_none());
                    assert!(
                        Instant::now() < deadline,
                        "scheduler never linked: {:?}",
                        runtime.stats()
                    );
                    std::thread::yield_now();
                }
                for _ in 0..128 {
                    assert!(runtime.try_run(poll, &mut memory).is_none());
                }
                assert_eq!(runtime.stats().compiled_entries, 2);
                assert_eq!(runtime.stats().compiled_calls, 0);
                assert_eq!(runtime.stats().failed_entries, 0);
            }
        }
    }
}

#[cfg(all(
    test,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        feature = "arm64-codegen"
    )
))]
#[test]
fn deferred_edges_preserve_expanded_ir_and_reserve_only_small_entries() {
    let source = r#"module Top(input bit clk, input bit en, output bit [7:0] q);
            bit [7:0] r = 0, a = 0, b = 0, c = 0;
            wire gated = clk & en;
            wire inverted = ~clk;
            wire second = gated & r[0];
            always_ff @(posedge clk) r <= r + 1;
            always_ff @(posedge gated) a <= a + r;
            always_ff @(posedge inverted) b <= b + a;
            always_ff @(posedge second) c <= c + b;
            assign q = a + b + c;
        endmodule"#;
    for partitioned in [false, true] {
        let builder = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("deferred_edges.sv"))],
            "Top",
        )
        .emit_triggers()
        .opt_level(crate::OptLevel::O1)
        .disable_pass(crate::SirPass::DerivedEdgeUnits);
        let builder = if partitioned {
            builder.settle_partition(crate::SettlePartition::Always)
        } else {
            builder
        };
        let (base, _, options, _, _) = builder
            .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
            .unwrap();
        let first = base.sir.processes.len();
        let mut expanded = base.clone();
        crate::derived_edge::attach(&mut expanded, true);
        crate::settle::instrument_processes_from(&mut expanded, first, false);
        let mut deferred = base;
        crate::derived_edge::attach_deferred(&mut deferred, true);
        crate::settle::instrument_processes_from(&mut deferred, first, false);
        assert!(deferred.deferred_derived_edges.len() >= 4);
        assert_eq!(expanded.sir.processes.len(), deferred.sir.processes.len());
        assert_eq!(
            expanded.runtime().runtime_schema.derived_edges,
            deferred.runtime().runtime_schema.derived_edges,
        );
        if partitioned {
            assert!(deferred.layout().settle.is_some());
        }
        let model = celox_design::schedule::ScheduleModel::derive(
            &deferred.design,
            &deferred.runtime().runtime_schema,
        );
        for (&index, edge) in &deferred.deferred_derived_edges {
            let reserved = &deferred.sir.processes[index];
            assert_eq!(reserved.blocks.len(), 1);
            assert!(reserved.register_map.is_empty());
            assert!(reserved.blocks.values().all(|block| {
                block.instructions.is_empty()
                    && matches!(block.terminator, crate::ir::SIRTerminator::Return)
            }));
            let generated = edge.generate(&deferred, &model).unwrap();
            generated.verify_result().unwrap();
            assert!(edge.blocks >= generated.blocks.len());
            assert!(
                edge.instructions
                    >= generated
                        .blocks
                        .values()
                        .map(|block| block.instructions.len())
                        .sum::<usize>()
            );
            assert_eq!(generated.blocks, expanded.sir.processes[index].blocks);
            assert_eq!(
                generated.register_map,
                expanded.sir.processes[index].register_map
            );
        }
        let expanded_image =
            crate::backend::native::NativeBackend::compile_image(&expanded, &options).unwrap();
        let deferred_image =
            crate::backend::native::NativeBackend::compile_image(&deferred, &options).unwrap();
        assert_eq!(expanded_image.code_image(), deferred_image.code_image());
        #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
        {
            let outlined_image =
                crate::backend::native::NativeBackend::compile_outlined_image_for_test(
                    &deferred, &options,
                )
                .unwrap();
            // Exercise pointer-free serialization before relocating both
            // images into independent executable mappings.
            let bytes = outlined_image.to_container_bytes().unwrap();
            let outlined_image: crate::backend::native::NativeProgramImage =
                crate::backend::native::NativeProgramImage::from_container_bytes(&bytes).unwrap();
            let mut simulations = [deferred_image, outlined_image].map(|image| {
                let runtime = image.runtime_program();
                // SAFETY: these images were compiled from this fixture.
                let backend =
                    unsafe { crate::backend::native::NativeBackend::from_image(image) }.unwrap();
                let mut sim = crate::Simulator::with_backend_and_program_for_simulation(
                    backend,
                    runtime,
                    Vec::new(),
                );
                sim.apply_initial_values();
                sim.modify(|_| {}).unwrap();
                sim
            });
            for step in 0..80 {
                for sim in &mut simulations {
                    let enable = sim.signal("en");
                    sim.modify(|io| io.set(enable, u8::from(step % 3 != 0)))
                        .unwrap();
                    let clk = sim.event("clk");
                    if step % 5 == 0 {
                        sim.edge(clk, crate::Edge::Falling).unwrap();
                    } else {
                        sim.tick(clk).unwrap();
                    }
                }
                for name in ["q", "r", "a", "b", "c"] {
                    assert_eq!(
                        simulations[0].get(simulations[0].signal(name)),
                        simulations[1].get(simulations[1].signal(name)),
                        "partitioned={partitioned}, step={step}, signal={name}"
                    );
                }
            }
        }
        let baseline = crate::derived_edge::without_units(&deferred);
        assert!(baseline.deferred_derived_edges.is_empty());
        assert!(baseline.runtime().runtime_schema.derived_edges.is_empty());
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_comb_chunks_preserve_cross_unit_state_through_image_roundtrip() {
    let source = r#"module Top(input bit [7:0] a,
            output bit [7:0] mid, out, mirror);
            assign mid = a + 8'h11;
            assign out = mid ^ 8'ha5;
            assign mirror = out + mid;
        endmodule"#;
    let (mut program, _, options, _, _) =
        crate::Simulator::from_sv_sources(vec![(source, std::path::Path::new("chunks.sv"))], "Top")
            .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
            .unwrap();
    crate::backend::native::NativeBackend::split_comb_units_for_test(&mut program);
    assert!(program.sir.eval_comb.len() > 1);
    let merged = crate::backend::native::NativeBackend::compile_image(&program, &options).unwrap();
    let chunked =
        crate::backend::native::NativeBackend::compile_outlined_image_for_test(&program, &options)
            .unwrap();
    assert!(
        chunked
            .code_entries()
            .iter()
            .filter(|entry| entry.name.starts_with("eval_comb_chunk["))
            .count()
            > 1
    );
    for image in [merged, chunked] {
        let image =
            crate::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        let runtime = image.runtime_program();
        // SAFETY: this image was compiled from this fixture.
        let backend = unsafe { crate::NativeBackend::from_image(image) }.unwrap();
        let mut sim =
            crate::Simulator::with_backend_and_program_for_simulation(backend, runtime, Vec::new());
        sim.apply_initial_values();
        for step in 0..80u8 {
            let value = step.wrapping_mul(17);
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, value)).unwrap();
            let mid = value.wrapping_add(0x11);
            let out = mid ^ 0xa5;
            for (name, expected) in [
                ("mid", mid),
                ("out", out),
                ("mirror", out.wrapping_add(mid)),
            ] {
                let signal = sim.signal(name);
                assert_eq!(sim.get(signal), num_bigint::BigUint::from(expected));
            }
        }
    }
}

#[cfg(all(test, target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn portable_fork_state_remaps_reordered_compiled_kernels() {
    let source = r#"`timescale 1ns/1ns
module Top(output bit [4:0] q);
 initial begin
   fork begin #2; q[0]=1; end begin #4; q[1]=1; end join
   q[2]=1;
 end
 initial begin fork begin #3; q[3]=1; end join q[4]=1; end
endmodule"#;
    let sources = vec![(source, std::path::Path::new("reordered.sv"))];
    let mut origin = crate::Simulation::from_sv_sources(sources.clone(), "Top")
        .build_native()
        .unwrap();
    origin.run_until(1).unwrap();
    let file = origin.save_state().unwrap();
    let (mut program, warnings, options, _, _) = crate::Simulation::from_sv_sources(sources, "Top")
        .emit_triggers()
        .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
        .unwrap();
    let schema = &mut program.runtime.runtime_schema;
    let count = schema.processes.len();
    assert!(count > 1);
    schema.processes.reverse();
    schema.process_origins.reverse();
    schema.process_frames.reverse();
    for site in &mut schema.wait_sites {
        site.process = count - 1 - site.process;
    }
    for site in &mut schema.nba_sites {
        site.process = count - 1 - site.process;
    }
    for site in &mut schema.host_request_sites {
        site.process = site.process.with_index(count - 1 - site.process.index());
    }
    program.sir.processes.reverse();
    let image = crate::backend::native::NativeBackend::compile_image(&program, &options).unwrap();
    let runtime = image.runtime_program();
    // SAFETY: this image was compiled from the trusted fixture above.
    let backend = unsafe { crate::backend::native::NativeBackend::from_image(image) }.unwrap();
    let mut simulator =
        crate::Simulator::with_backend_and_program_for_simulation(backend, runtime, warnings);
    simulator.apply_initial_values();
    simulator.modify(|_| {}).unwrap();
    let mut target = crate::Simulation::new(simulator);
    target.load_state(&file).unwrap();
    target.run_until(5).unwrap();
    assert_eq!(target.get(target.signal("q")), 31u8.into());
}

#[cfg(all(
    test,
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]
#[test]
fn extended_host_requests_survive_native_image_roundtrip() {
    let source = r#"module Top(output bit ok);
            int seed, value, parsed, count, real_count;
            real fractional;
            initial begin
                seed = 17;
                value = $dist_normal(seed, 0, 10);
                count = $sscanf("42", "%d", parsed);
                real_count = $sscanf("6.25", "%f", fractional);
                ok = value == -4 && parsed == 42 && count == 1
                    && real_count == 1 && $sqrt(fractional) == 2.5 && $realtime == 0.0;
            end
        endmodule"#;
    let (program, warnings, options, _, _) = crate::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("host_image.sv"))],
        "Top",
    )
    .emit_triggers()
    .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
    .unwrap();
    let image = crate::backend::native::NativeBackend::compile_image(&program, &options).unwrap();
    let image =
        crate::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let runtime = image.runtime_program();
    // SAFETY: this image was compiled from the trusted source above.
    let backend = unsafe { crate::backend::native::NativeBackend::from_image(image) }.unwrap();
    let mut simulator =
        crate::Simulator::with_backend_and_program_for_simulation(backend, runtime, warnings);
    simulator.apply_initial_values();
    simulator.modify(|_| {}).unwrap();
    let mut sim = crate::Simulation::new(simulator);
    sim.run_until(0).unwrap();
    assert_eq!(sim.get(sim.signal("ok")), num_bigint::BigUint::from(1u8));
}

#[cfg(all(
    test,
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]
#[test]
fn typed_vcd_selections_survive_native_image_roundtrip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("native.vcd");
    let source = format!(
        r#"
            package p; logic state=1; endpackage
            module Child(output logic \state.value ); initial \state.value = 1; endmodule
            module Top #(parameter int INDEX=1) (output logic [1:0] bus, output logic \literal[0] );
                Child u[0:1](bus);
                logic package_use;
                assign package_use=p::state;
                assign \literal[0] = 0;
                initial begin $dumpfile("{}");
                    $dumpvars(0, Top.u[INDEX].\state.value , p::state, \literal[0] );
                end
            endmodule"#,
        path.display()
    );
    let (program, warnings, options, _, _) = crate::Simulation::from_sv_sources(
        vec![(&source, std::path::Path::new("native-vcd.sv"))],
        "Top",
    )
    .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
    .unwrap();
    let image = crate::backend::native::NativeBackend::compile_image(&program, &options).unwrap();
    let image =
        crate::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let runtime = image.runtime_program();
    assert!(
        runtime
            .runtime_schema
            .host_request_sites
            .iter()
            .flat_map(|site| &site.strings)
            .any(|path| path.contains("\0local\0"))
    );
    // SAFETY: the image was compiled from this test's trusted source.
    let backend = unsafe { crate::backend::native::NativeBackend::from_image(image) }.unwrap();
    let mut simulator =
        crate::Simulator::with_backend_and_program_for_simulation(backend, runtime, warnings);
    simulator.apply_initial_values();
    simulator.modify(|_| {}).unwrap();
    let mut simulation = crate::Simulation::new(simulator);
    simulation.run_until(1).unwrap();
    simulation.flush_vcd().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert_eq!(text.matches("$var wire ").count(), 3, "{text}");
    assert!(text.contains("Top.u[1]"), "{text}");
    assert!(text.contains("\\state.value"), "{text}");
    assert!(text.contains("\\literal[0]"), "{text}");
}

#[cfg(all(
    test,
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]
#[test]
fn runtime_declaration_startup_survives_native_image_roundtrip() {
    let source = r#"module Top;
            int a = $urandom | 1;
            logic [31:0] q = 32'h55;
            always @* q = a;
            initial a = 0;
            initial begin
                #1;
                if (a !== 0 || q !== 0) $fatal(1, "ordinary initial overwrite was lost");
                a = 5;
                #1;
                if (q !== 5) $fatal(1, "later wildcard event was lost");
                $finish;
            end
        endmodule"#;
    for four_state in [false, true] {
        let (program, warnings, options, _, _) = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("native-declaration.sv"))],
            "Top",
        )
        .four_state(four_state)
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
        let image =
            crate::backend::native::NativeBackend::compile_image(&program, &options).unwrap();
        let image =
            crate::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
                .unwrap();
        let runtime = image.runtime_program();
        // SAFETY: this image was compiled from the trusted source above.
        let backend = unsafe { crate::backend::native::NativeBackend::from_image(image) }.unwrap();
        let mut simulator =
            crate::Simulator::with_backend_and_program_for_simulation(backend, runtime, warnings);
        simulator.apply_initial_values();
        // The constructor completes declarations, primes sensitivity, and
        // replays folded ordinary writes before exposing the simulation.
        let mut simulation = crate::Simulation::try_new(simulator).unwrap();
        let a = simulation.signal("a");
        let q = simulation.signal("q");
        // Read storage directly: public get() would settle the pending
        // ordinary write and wake the wildcard before these assertions.
        assert_eq!(
            crate::SimBackend::get(&simulation.simulator.backend, a),
            0u8.into()
        );
        assert_eq!(
            crate::SimBackend::get(&simulation.simulator.backend, q),
            0x55u8.into()
        );
        for _ in 0..16 {
            if simulation.is_finished() || simulation.step().unwrap().is_none() {
                break;
            }
        }
        assert!(simulation.is_finished());
        assert_eq!(simulation.get(q), 5u8.into());
    }
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn unchanged_clock_does_not_invalidate_compiled_settle() {
    assert_eq!(
        count_clean_clock_batch(true, false, false).0,
        0,
        "only a moved; the combinational input b did not change"
    );
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn clean_clock_batch_does_not_repeat_fused_comb_evaluation() {
    let (evaluations, fused_domains) = count_clean_clock_batch(false, false, false);
    assert!(
        fused_domains > 0,
        "the regression must exercise a fused domain"
    );
    assert_eq!(
        evaluations, 0,
        "the FF's combinational inputs were already settled"
    );
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn clock_alias_notifications_do_not_force_datapath_evaluation() {
    assert_eq!(count_clean_clock_batch(false, true, false).0, 0);
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
#[test]
fn dirty_clock_batch_evaluates_new_inputs_before_ff_update() {
    assert!(count_clean_clock_batch(false, false, true).0 > 0);
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_scheduler_delayed_nba_code_grows_linearly() {
    // Every process both stages a delayed NBA and waits on the clock:
    // publishing a site must not copy every wait check, nor replaying a
    // host input every whole-object store, once per site or call.
    for four_state in [false, true] {
        let size = |processes| {
            let mut source = String::from(
                "`timescale 1ns/1ns\nmodule Top; bit clk = 0; always #1 clk = ~clk;\n",
            );
            for index in 0..processes {
                source += &format!("logic [71:0] r{index} = 0;\n");
                let previous = index.max(1) - 1;
                source += &format!("always @(posedge clk) r{index} <= #1 r{previous} + 1;\n");
            }
            source += "endmodule\n";
            let mut builder = crate::Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("delayed-nbas.sv"))],
                "Top",
            )
            .four_state(four_state)
            .emit_triggers();
            builder.native_file_calls = true;
            let (program, _, _, _, _) = builder
                .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, true, false)
                .unwrap();
            let scheduler = program.runtime.runtime_schema.scheduler.as_ref().unwrap();
            assert!(scheduler.fallback.iter().all(Option::is_none));
            program.sir.processes[scheduler.unit.unwrap()]
                .blocks
                .values()
                .map(|block| block.instructions.len())
                .sum::<usize>()
        };
        let small = size(32);
        let large = size(64);
        assert!(
            large < small * 3,
            "quadratic delayed NBA code: {small} -> {large}, four_state={four_state}"
        );
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn hotspot_scheduler_outlines_kernels_past_its_inline_budget() {
    // Hundreds of medium processes: inlining all of them would make the
    // compiled scheduler one function of their combined size.
    let mut source =
        String::from("`timescale 1ns/1ns\nmodule Top; bit clk = 0; always #1 clk = ~clk;\n");
    for index in 0..1000 {
        source +=
            &format!("logic [63:0] r{index} = {index};\ninitial forever begin @(posedge clk);\n");
        for step in 0..40 {
            source += &format!(
                "  if (r{index}[{bit}]) r{index} = r{index} * 3 + {step}; else r{index} = r{index} ^ (r{index} >> 3);\n",
                bit = step % 64
            );
        }
        source += "end\n";
    }
    source += "endmodule\n";
    let mut builder = crate::Simulation::from_sv_sources(
        vec![(source.as_str(), std::path::Path::new("outlined-kernels.sv"))],
        "Top",
    )
    .emit_triggers();
    builder.native_file_calls = true;
    let (program, _, _, _, _) = builder
        .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, true, false)
        .unwrap();
    let scheduler = program.runtime.runtime_schema.scheduler.as_ref().unwrap();
    assert!(scheduler.fallback.iter().all(Option::is_none));
    let size = |unit: &crate::ir::ExecutionUnit<crate::ir::RegionedAbsoluteAddr>| {
        unit.blocks
            .values()
            .map(|block| block.instructions.len())
            .sum::<usize>()
    };
    let kernels: usize = program
        .sir
        .processes
        .iter()
        .enumerate()
        .filter(|&(index, _)| Some(index) != scheduler.unit)
        .map(|(_, unit)| size(unit))
        .sum();
    let budget = crate::scheduler::INLINE_BUDGET_INSTRUCTIONS;
    assert!(
        kernels > 2 * budget,
        "the design must exceed the budget: {kernels}"
    );
    let staged = crate::scheduler::hotspot_scheduler(&program, Some(budget), None).unwrap();
    assert!(staged.budgeted && !staged.callees.is_empty());
    assert!(
        size(&staged.unit) < kernels,
        "scheduler {} inlines most of {kernels} kernel instructions",
        size(&staged.unit)
    );
    // The final stage inlines them all.
    let inlined = crate::scheduler::hotspot_scheduler(&program, None, None).unwrap();
    assert!(!inlined.budgeted && inlined.callees.is_empty());
    assert!(size(&inlined.unit) > kernels);
    // A cancelled generation stops inlining and returns no scheduler.
    let cancel = crate::backend::compile_cancel::CompileCancel::new();
    cancel.cancel();
    assert!(crate::scheduler::hotspot_scheduler(&program, None, Some(&cancel)).is_err());
}

#[test]
fn always_comb_startup_gate_reads_process_statuses_once() {
    // Every always_comb waits for the time-zero barrier. Testing every
    // process status in each of them would make every status write
    // dirty every gated unit in a partitioned settle.
    let mut source = String::from("`timescale 1ns/1ns\nmodule Top; logic clk = 0;\n");
    for index in 0..8 {
        source += &format!("logic [7:0] a{index} = 0, b{index};\n");
        source += &format!("initial begin #1; a{index} = {index}; end\n");
        source += &format!("always_comb b{index} = a{index} + 8'd1;\n");
    }
    source += "always #5 clk = ~clk;\nendmodule\n";
    let (program, _, _, _, _) = crate::Simulation::from_sv_sources(
        vec![(source.as_str(), std::path::Path::new("startup-gate.sv"))],
        "Top",
    )
    .emit_triggers()
    .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, false, false)
    .unwrap();
    let statuses: crate::HashSet<_> = program
        .runtime
        .runtime_schema
        .processes
        .iter()
        .map(|process| process.status)
        .collect();
    assert!(statuses.len() >= 9);
    let loads = program
        .sir
        .eval_comb
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .flat_map(|block| &block.instructions)
        .filter(|instruction| {
            matches!(instruction, crate::ir::SIRInstruction::Load(_, address, ..)
                    if statuses.contains(&address.absolute_addr()))
        })
        .count();
    assert!(
        loads <= statuses.len(),
        "{loads} status loads for {} statuses",
        statuses.len()
    );
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_scheduler_host_wait_scans_grow_linearly() {
    for four_state in [false, true] {
        let size = |processes| {
            let mut source = String::from(
                "`timescale 1ns/1ns\nmodule Top; bit clk = 0; always #1 clk = ~clk;\n",
            );
            for index in 0..processes {
                source += &format!("always @(posedge clk) $fwrite(1, \"process {index}\\n\");\n");
            }
            source += "endmodule\n";
            let mut builder = crate::Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("host-waits.sv"))],
                "Top",
            )
            .four_state(four_state)
            .emit_triggers();
            builder.native_file_calls = true;
            let (program, _, _, _, _) = builder
                .into_laid_out_program(crate::MemoryLayoutMode::ElementStrided, true, false)
                .unwrap();
            let scheduler = program.runtime.runtime_schema.scheduler.as_ref().unwrap();
            assert!(scheduler.fallback.iter().all(Option::is_none));
            let unit = crate::scheduler::hotspot_scheduler(&program, None, None)
                .unwrap()
                .unit;
            unit.blocks
                .values()
                .map(|block| block.instructions.len())
                .sum::<usize>()
        };
        let small = size(32);
        let large = size(64);
        assert!(
            large < small * 3,
            "quadratic wait scans: {small} -> {large}, four_state={four_state}"
        );
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_scheduler_host_wait_continuations_exceed_one_byte() {
    let mut source =
        String::from("`timescale 1ns/1ns\nmodule Top; bit clk = 0; always #1 clk = ~clk;\n");
    for index in 0..260 {
        source += &format!("always @(posedge clk) $fwrite(1, \"process {index}\\n\");\n");
    }
    source += "endmodule\n";
    let mut results = Vec::new();
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::DifferentialRuns,
    ] {
        let mut sim = crate::Simulation::from_sv_sources(
            vec![(
                source.as_str(),
                std::path::Path::new("host-continuations.sv"),
            )],
            "Top",
        )
        .four_state(true)
        .scheduler_mode(mode)
        .build_native()
        .unwrap();
        sim.run_until(3).unwrap();
        results.push(sim.drain_runtime_events());
        if mode == crate::SchedulerMode::DifferentialRuns {
            assert!(sim.scheduler_stats().compiled_slots > 0);
        }
    }
    assert_eq!(results[0].len(), 520);
    assert_eq!(results[0], results[1]);
}

/// A bit-level schedule may load a range before the bits it needs are
/// stored, and discard the stale ones: the settle is still a function of
/// its inputs, so it may be deferred.
#[test]
fn discarded_stale_bits_keep_settles_deferred() {
    let source = r#"
            module Child (
                translate: input logic, frame: input logic<20>, pc: input logic<12>,
                physical: output logic<12>,
            ) {
                var translated: logic<32>;
                assign translated = {frame, pc};
                assign physical = if translate ? translated[11:0] : pc;
            }
            module Top (
                clk: input clock, rst: input reset, translate: input logic,
                next_frame: input logic<20>, next_pc: input logic<12>,
                physical: output logic<12>,
            ) {
                var frame_q: logic<20>;
                var pc_q: logic<12>;
                var frame: logic<20>;
                always_ff {
                    if_reset { frame_q = 0; pc_q = 0; } else { frame_q = next_frame; pc_q = next_pc; }
                }
                inst child: Child (translate, frame, pc: pc_q, physical);
                assign frame = frame_q;
            }
        "#;
    let (program, _, _, _, _) = crate::Simulation::builder(source, "Top")
        .four_state(false)
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    assert_eq!(program.sir.eval_comb_stateful, Some(Vec::new()));
    let (defers, by_loads) = crate::scheduler::defers_settles_for_test(&program);
    assert!(defers);
    assert!(!by_loads, "the fixture must load stale bits it discards");
}

#[cfg(all(
    test,
    any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    )
))]
#[test]
fn blocking_latch_input_settles_before_nba_closes_the_latch() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, d = 0, en = 0, q = 0;
    always #1 clk = !clk;
    always_latch if (!en) q = d;
    always @(posedge clk) begin d = 1; en <= 1; end
    initial begin #2; if (d != 1 || q != 1) $fatal(1, "latch missed Active update"); $finish; end
endmodule"#;
    for (fused, statefulness_known) in [(false, false), (false, true), (true, false), (true, true)]
    {
        for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
            for four_state in [false, true] {
                let mut simulation = crate::Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("active-latch.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .opt_level(level)
                .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
                .build_with_backend(
                    crate::MemoryLayoutMode::ElementStrided,
                    true,
                    false,
                    |program, options| {
                        // Existing SV probes otherwise mask the missing stateful
                        // consumer dependency in the scheduler's classification.
                        let program = crate::scheduler::without_probe_hints_for_test(
                            program.clone(),
                            statefulness_known,
                        );
                        if fused {
                            crate::NativeBackend::new(&program, options)
                        } else {
                            let first_tier =
                                crate::scheduler::without_fused_domains(program.clone(), true);
                            crate::NativeBackend::new(&first_tier, options)
                        }
                    },
                )
                .unwrap();
                simulation.run_until(3).unwrap_or_else(|error| {
                    panic!("four_state={four_state}, level={level:?}, fused={fused}: {error}")
                });
                assert!(simulation.is_finished());
                let q = simulation.signal("q");
                assert_eq!(simulation.get(q), num_bigint::BigUint::from(1u8));
                let stats = simulation.scheduler_stats();
                assert!(stats.compiled_slots > 0, "{stats:?}");
                assert!(stats.checked_steps > 0, "{stats:?}");
            }
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn unrelated_active_updates_allow_a_procedural_clock_cone() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, d = 0, en = 0, q = 0;
    bit [1:0] select_clock = 0;
    bit gated;
    int count = 0;
    always #1 clk = !clk;
    always @* case (select_clock)
        0: gated = clk;
        1: gated = !clk;
        default: gated = 0;
    endcase
    always_latch if (!en) q = d;
    always @(posedge clk) begin d = 1; en <= 1; end
    always @(posedge gated) count <= count + 1;
    initial begin
        #6;
        if (q != 1 || count != 3) $fatal(1, "procedural clock or latch lost an update");
        $finish;
    end
endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("procedural-clock.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_with_backend(
                crate::MemoryLayoutMode::ElementStrided,
                true,
                false,
                |program, options| {
                    assert!(
                        program
                            .design
                            .events
                            .combinational_derived
                            .values()
                            .flatten()
                            .any(|clock| clock.gate.is_none() && clock.inputs.is_some()),
                        "the fixture must need the full dependency cone"
                    );
                    crate::NativeBackend::new(program, options)
                },
            )
            .unwrap();
            simulation.run_until(7).unwrap();
            assert!(simulation.is_finished());
            let stats = simulation.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn derived_clock_domains_stop_at_the_same_state_as_the_reference() {
    for active in [false, true] {
        for derived in [false, true] {
            for fatal in [false, true] {
                let counter = if derived { "secondary" } else { "primary" };
                let stop = if fatal {
                    "$fatal(1, \"domain stopped\");"
                } else {
                    "$finish;"
                };
                let condition = format!("if ({counter} == 2) {stop}");
                let source = format!(
                    r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, d = 0, en = 0, q = 0;
    wire derived = ~clk;
    int primary = 0, secondary = 0;
    always #1 clk = !clk;
    always_latch if (!en) q = d;
    always @(posedge clk) begin
        primary <= primary + 1;
        {blocking}
        {primary_stop}
    end
    always @(posedge derived) begin
        secondary <= secondary + 1;
        {derived_stop}
    end
    initial #100 $fatal(1, "deadline");
endmodule"#,
                    blocking = if active { "d = 1; en <= 1;" } else { "" },
                    primary_stop = if derived { "" } else { &condition },
                    derived_stop = if derived { &condition } else { "" },
                );
                for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
                    for four_state in [false, true] {
                        let build = |mode| {
                            crate::Simulation::from_sv_sources(
                                vec![(source.as_str(), std::path::Path::new("domain-stop.sv"))],
                                "Top",
                            )
                            .four_state(four_state)
                            .opt_level(level)
                            .scheduler_mode(mode)
                            .live_signal(Vec::new(), vec!["primary".into()])
                            .live_signal(Vec::new(), vec!["secondary".into()])
                            .live_signal(Vec::new(), vec!["clk".into()])
                            .live_signal(Vec::new(), vec!["derived".into()])
                            .live_signal(Vec::new(), vec!["d".into()])
                            .live_signal(Vec::new(), vec!["en".into()])
                            .live_signal(Vec::new(), vec!["q".into()])
                            .build_native()
                            .unwrap()
                        };
                        let mut reference = build(crate::SchedulerMode::Reference);
                        let mut compiled = build(crate::SchedulerMode::DifferentialRuns);
                        let expected = reference.run_until(100);
                        let actual = compiled.run_until(100);
                        assert_eq!(
                            format!("{actual:?}"),
                            format!("{expected:?}"),
                            "active={active}, derived={derived}, fatal={fatal}, level={level:?}, four_state={four_state}"
                        );
                        assert_eq!(expected.is_err(), fatal);
                        assert_eq!(compiled.time(), reference.time());
                        for name in ["primary", "secondary", "clk", "derived", "d", "en", "q"] {
                            let expected = reference.signal(name);
                            let actual = compiled.signal(name);
                            assert_eq!(compiled.get(actual), reference.get(expected), "{name}");
                        }
                        let stats = compiled.scheduler_stats();
                        assert!(stats.compiled_slots > 0, "{stats:?}");
                        assert!(stats.checked_steps > 0, "{stats:?}");
                    }
                }
            }
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn raw_wait_multiple_terms_can_wake_one_kernel_twice_in_a_slot() {
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        let source = r#"`timescale 1ns/1ns
module Top;
    bit [69:0] a = 0;
    bit [3:0] b = 0;
    bit [7:0] count = 0;
    always #1 a = a + 70'h200000000000000000;
    initial forever begin @(a or b); count = count + 1; end
    initial forever begin #2; b = b + 1; end
    initial begin #11; if (count != 16) $fatal(1, "lost wakeup"); $finish; end
endmodule"#;
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("multiple-wait.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            let result = simulation.run_until(11);
            let observed = simulation.signal("count");
            assert!(
                result.is_ok(),
                "{result:?}; count={}; stats={:?}",
                simulation.get(observed),
                simulation.scheduler_stats()
            );
            assert!(simulation.is_finished());
            let count = simulation.signal("count");
            assert_eq!(simulation.get(count), num_bigint::BigUint::from(16u8));
            let stats = simulation.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn raw_wait_snapshots_distinguish_x_from_z() {
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let expected = if four_state { 3 } else { 1 };
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
    bit [1:0] phase = 0;
    logic [69:0] data = 0;
    bit [7:0] count = 0;
    always #1 begin
        phase = phase + 1;
        data = phase == 1 ? 'x : (phase == 2 ? 'z : 70'd7);
    end
    initial forever begin @(data); count = count + 1; end
    initial begin #4; if (count != {expected}) $fatal(1, "bad raw planes"); $finish; end
endmodule"#
            );
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source.as_str(), std::path::Path::new("wide-wait.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(4).unwrap();
            assert!(simulation.is_finished());
            let count = simulation.signal("count");
            assert_eq!(
                simulation.get(count),
                num_bigint::BigUint::from(expected as u8)
            );
            let stats = simulation.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn raw_wait_rearms_at_distinct_sites_and_across_run_boundaries() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit [69:0] a = 0;
    bit [3:0] b = 0;
    int count = 0;
    always #1 a = a + 70'h200000000000000000;
    initial forever begin
        @(a or b); count = count + 1;
        #1;
        @(a or b); count = count + 1;
    end
    initial forever begin #2; b = b + 1; end
    initial #12 $finish;
endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let build = |mode| {
                crate::Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("rearm-raw-wait.sv"))],
                    "Top",
                )
                .opt_level(level)
                .four_state(four_state)
                .scheduler_mode(mode)
                .live_signal(Vec::new(), vec!["count".into()])
                .build_native()
                .unwrap()
            };
            let mut reference = build(crate::SchedulerMode::Reference);
            let mut compiled = build(crate::SchedulerMode::DifferentialRuns);
            for end in [2, 3, 4, 5, 11, 12] {
                reference.run_until(end).unwrap();
                compiled.run_until(end).unwrap();
                assert_eq!(compiled.time(), reference.time());
                let expected = reference.signal("count");
                let actual = compiled.signal("count");
                assert_eq!(compiled.get(actual), reference.get(expected));
            }
            assert!(compiled.is_finished());
            let stats = compiled.scheduler_stats();
            assert!(stats.compiled_slots > 0, "{stats:?}");
            assert!(stats.checked_steps > 0, "{stats:?}");
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn reactive_waits_run_after_nbas_without_repeating_domains() {
    for derived in [false, true] {
        let clock = if derived { "divided" } else { "clk" };
        let source = format!(
            r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    bit divided = 0;
    bit [69:0] data = 0;
    wire [69:0] expression = data + data;
    bit [69:0] middle = 0;
    bit [69:0] seen = 0;
    int count = 0;
    int sampled = 0;
    always #1 clk = ~clk;
    always @(posedge clk) divided <= ~divided;
    always @(posedge {clock}) begin data <= data + 1; sampled <= count; end
    initial forever begin @(expression); middle = expression; end
    initial forever begin @(middle); seen = middle; count = count + 1; end
    initial #13 $finish;
endmodule"#
        );
        for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
            for four_state in [false, true] {
                let build = |mode| {
                    crate::Simulation::from_sv_sources(
                        vec![(source.as_str(), std::path::Path::new("reactive-waits.sv"))],
                        "Top",
                    )
                    .opt_level(level)
                    .four_state(four_state)
                    .scheduler_mode(mode)
                    .build_native()
                    .unwrap()
                };
                let mut reference = build(crate::SchedulerMode::Reference);
                let mut compiled = build(crate::SchedulerMode::DifferentialRuns);
                for end in [2, 3, 4, 7, 12] {
                    reference.run_until(end).unwrap();
                    compiled.run_until(end).unwrap();
                    assert_eq!(compiled.time(), reference.time());
                    for name in ["data", "middle", "seen", "count", "sampled"] {
                        let expected = reference.signal(name);
                        let actual = compiled.signal(name);
                        assert_eq!(compiled.get(actual), reference.get(expected), "{name}");
                    }
                }
                let count = compiled.signal("count");
                let sampled = compiled.signal("sampled");
                let expected = if derived { 3u8 } else { 6u8 };
                assert_eq!(compiled.get(count), num_bigint::BigUint::from(expected));
                assert_eq!(
                    compiled.get(sampled),
                    num_bigint::BigUint::from(expected - 1)
                );
                let stats = compiled.scheduler_stats();
                assert!(
                    stats.compiled_slots > 0 && stats.checked_steps > 0,
                    "{stats:?}"
                );
                assert!(
                    stats
                        .kernels
                        .iter()
                        .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
                    "{stats:?}"
                );
            }
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn reactive_wait_cycles_use_a_bounded_native_work_list() {
    let source = r#"module Top;
            bit a = 0;
            bit b = 0;
            initial forever begin @(a); b = ~b; end
            initial forever begin @(b); a = ~a; end
        endmodule"#;
    for four_state in [false, true] {
        let simulation = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("wait-cycle.sv"))],
            "Top",
        )
        .four_state(four_state)
        .build_native()
        .unwrap();
        let stats = simulation.scheduler_stats();
        assert_eq!(stats.kernels.len(), 2, "{stats:?}");
        assert!(
            stats
                .kernels
                .iter()
                .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
            "{stats:?}"
        );
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn reactive_waits_on_active_results_run_before_native_nba_publication() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0;
            bit [69:0] data = 0;
            wire [69:0] expression = data + data;
            bit nba_value = 0;
            bit seen = 0;
            always #1 clk = ~clk;
            always @(posedge clk) begin data = data + 1; nba_value <= 1; end
            initial forever begin @(expression); seen = nba_value; end
        endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("active-wait.sv"))],
                "Top",
            )
            .opt_level(level)
            .four_state(four_state)
            .build_native()
            .unwrap();
            simulation.run_until(2).unwrap();
            let seen = simulation.signal("seen");
            assert_eq!(simulation.get(seen), num_bigint::BigUint::from(0u8));
            let stats = simulation.scheduler_stats();
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled))
                    && stats.compiled_slots > 0,
                "{stats:?}"
            );
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn reactive_wait_can_finish_after_a_host_clock_nba() {
    let source = r#"`timescale 1ns/1ns
        module Top(input bit clk);
            bit [69:0] data = 0;
            always @(posedge clk) data <= data + 1;
            initial forever begin @(data); if (data == 3) $finish; end
        endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("finish-after-nba.sv"))],
                "Top",
            )
            .opt_level(level)
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.add_clock("clk", 2, 1);
            simulation.run_until(12).unwrap();
            assert!(simulation.is_finished());
            assert_eq!(simulation.time(), 5);
            assert_eq!(
                simulation.get(simulation.signal("data")),
                num_bigint::BigUint::from(3u8)
            );
            let stats = simulation.scheduler_stats();
            assert!(
                stats.compiled_slots > 0 && stats.checked_steps > 0,
                "{stats:?}"
            );
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
                "{stats:?}"
            );
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn reactive_wait_remembers_an_intermediate_domain_wave() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0;
            bit divided = 0;
            bit first = 0, second = 0;
            wire changed = first ^ second;
            int count = 0;
            always #1 clk = ~clk;
            always @(posedge clk) begin divided <= ~divided; first <= ~first; end
            always @(posedge divided) second <= ~second;
            initial forever begin @(changed); count = count + 1; end
            initial #13 $finish;
        endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("intermediate-wave.sv"))],
                "Top",
            )
            .opt_level(level)
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(0).unwrap();
            let initial_count = simulation.get(simulation.signal("count"));
            simulation.run_until(12).unwrap();
            assert_eq!(
                simulation.get(simulation.signal("count")),
                initial_count + num_bigint::BigUint::from(6u8),
                "level={level:?}, four_state={four_state}"
            );
            let stats = simulation.scheduler_stats();
            assert!(
                stats.compiled_slots > 0 && stats.checked_steps > 0,
                "{stats:?}"
            );
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
                "{stats:?}"
            );
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn reactive_wait_direct_self_write_is_captured_before_registration() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit trigger = 0, own = 0;
            int count = 0;
            always #1 trigger = ~trigger;
            initial forever begin @(trigger or own); own = ~own; count = count + 1; end
            initial #13 $finish;
        endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("direct-self-write.sv"))],
                "Top",
            )
            .opt_level(level)
            .four_state(four_state)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            for end in [2, 3, 7, 12] {
                simulation.run_until(end).unwrap();
            }
            assert_eq!(
                simulation.get(simulation.signal("count")),
                num_bigint::BigUint::from(12u8)
            );
            let stats = simulation.scheduler_stats();
            assert!(
                stats.compiled_slots > 0 && stats.checked_steps > 0,
                "{stats:?}"
            );
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
                "{stats:?}"
            );
        }
    }
    let feedback = r#"module Top;
            bit own = 0;
            wire expression = ~own;
            initial forever begin @(expression); own = ~own; end
        endmodule"#;
    let simulation = crate::Simulation::from_sv_sources(
        vec![(feedback, std::path::Path::new("computed-self-write.sv"))],
        "Top",
    )
    .build_native()
    .unwrap();
    assert!(
        simulation
            .scheduler_stats()
            .kernels
            .iter()
            .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
        "{:?}",
        simulation.scheduler_stats()
    );
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn native_file_calls_keep_the_host_return_bound() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0;
            int count = 0;
            always #1 clk = ~clk;
            always @(posedge clk) count <= count + 1;
            always @(posedge clk) for (int i = 0; i < 8; i++) $fwrite(1, "bounded");
        endmodule"#;
    let shared = source.replace(
        r#"always @(posedge clk) for (int i = 0; i < 8; i++) $fwrite(1, "bounded");"#,
        concat!(
            "always @(posedge clk) repeat (3) $fwrite(1, \"first\"); ",
            "always @(posedge clk) repeat (3) $fwrite(1, \"second\");",
        ),
    );
    for source in [source, shared.as_str()] {
        for four_state in [false, true] {
            let mut results = Vec::new();
            for mode in [
                crate::SchedulerMode::Reference,
                crate::SchedulerMode::Compiled,
            ] {
                let mut simulation = crate::Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("host-return-bound.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .scheduler_mode(mode)
                .build_native()
                .unwrap();
                simulation.state.set_slot_budgets(32, 4);
                let error = simulation.run_until(2).unwrap_err();
                assert!(error.to_string().contains("does not suspend"), "{error}");
                results.push((
                    simulation.time(),
                    simulation.get(simulation.signal("count")),
                    simulation.drain_runtime_events(),
                ));
                if mode == crate::SchedulerMode::Compiled {
                    assert!(
                        simulation.scheduler_stats().compiled_slots > 0,
                        "{:?}",
                        simulation.scheduler_stats()
                    );
                }
            }
            assert_eq!(results[0], results[1]);
            assert_eq!(results[1].2.len(), 5);
        }
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn native_file_calls_survive_tiered_adoption() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0;
            int count = 0;
            always #1 clk = ~clk;
            always @(posedge clk) count <= count + 1;
            always @(posedge clk) $fdisplay(1, "count:%0d", count);
        endmodule"#;
    for four_state in [false, true] {
        let build = || {
            crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("tiered-file-call.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        let mut reference = build()
            .scheduler_mode(crate::SchedulerMode::Reference)
            .build_native()
            .unwrap();
        let mut tiered = build()
            .tier_promotion(crate::TierPromotion::AfterSteps(1))
            .build_tiered()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut end = 2;
        loop {
            reference.run_until(end).unwrap();
            tiered.run_until(end).unwrap();
            assert_eq!(
                tiered.drain_runtime_events(),
                reference.drain_runtime_events()
            );
            assert_eq!(
                tiered.get(tiered.signal("count")),
                reference.get(reference.signal("count"))
            );
            if tiered.simulator.is_compiled() && tiered.scheduler_stats().compiled_slots > 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                tiered.simulator.backend.promotion_error()
            );
            end += 2;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            tiered
                .scheduler_stats()
                .kernels
                .iter()
                .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
            "{:?}",
            tiered.scheduler_stats()
        );
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn native_file_calls_reset_the_bound_after_a_wait() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0;
            bit [69:0] data = 0;
            always #1 clk = ~clk;
            always @(posedge clk) data <= data + 1;
            initial forever begin @(clk or data); repeat (4) $fwrite(1, "bounded"); end
        endmodule"#;
    for four_state in [false, true] {
        let mut results = Vec::new();
        for mode in [
            crate::SchedulerMode::Reference,
            crate::SchedulerMode::Compiled,
        ] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("host-return-rearm.sv"))],
                "Top",
            )
            .four_state(four_state)
            .scheduler_mode(mode)
            .build_native()
            .unwrap();
            simulation.state.set_slot_budgets(32, 4);
            simulation.run_until(2).unwrap();
            results.push((simulation.time(), simulation.drain_runtime_events()));
            if mode == crate::SchedulerMode::Compiled {
                assert!(
                    simulation.scheduler_stats().compiled_slots > 0,
                    "{:?}",
                    simulation.scheduler_stats()
                );
            }
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[1].1.len(), 12);
    }
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn native_loop_checkpoints_continue_without_reference_slots() {
    let source = r#"`timescale 1ns/1ns
        module Top;
            bit clk = 0;
            int count = 0;
            always #1 clk = ~clk;
            initial forever begin @(posedge clk); repeat (65537) count = count + 1; end
        endmodule"#;
    for level in [crate::OptLevel::O0, crate::OptLevel::O2] {
        for four_state in [false, true] {
            let mut simulation = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("loop-checkpoint.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
            simulation.run_until(6).unwrap();
            assert_eq!(
                simulation.get(simulation.signal("count")),
                num_bigint::BigUint::from(196611u32)
            );
            let stats = simulation.scheduler_stats();
            if cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen"))) {
                assert_eq!(stats.native_service_calls, 0, "{stats:?}");
            } else {
                assert!(stats.native_service_calls > 0, "{stats:?}");
            }
            assert!(
                stats.compiled_slots > 0 && stats.checked_steps > 0,
                "{stats:?}"
            );
            assert!(
                stats
                    .kernels
                    .iter()
                    .all(|kernel| matches!(kernel, crate::KernelSchedule::Compiled)),
                "{stats:?}"
            );
            let mut bounded = crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("bounded-loop-checkpoint.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(level)
            .build_native()
            .unwrap();
            bounded.state.set_slot_budgets(32, 0);
            let error = bounded.run_until(2).unwrap_err();
            assert!(error.to_string().contains("does not suspend"), "{error}");
            assert!(
                bounded.scheduler_stats().compiled_slots > 0,
                "{:?}",
                bounded.scheduler_stats()
            );
        }
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_budget_guard_continuations_exceed_one_byte() {
    let mut source =
        String::from("`timescale 1ns/1ns\nmodule Top; bit clk = 0; always #1 clk = ~clk;\n");
    for index in 0..260 {
        source += &format!(
            "int count_{index} = 0; initial forever begin @(posedge clk); repeat (65537) count_{index}++; end\n"
        );
    }
    source += "endmodule\n";
    for hotspot in [false, true] {
        let mut sim = crate::Simulation::from_sv_sources(
            vec![(
                source.as_str(),
                std::path::Path::new("guard-continuations.sv"),
            )],
            "Top",
        )
        .four_state(true)
        .hotspot_jit(hotspot)
        .scheduler_mode(crate::SchedulerMode::Compiled)
        .build_native()
        .unwrap();
        sim.state.set_slot_budgets(u64::MAX, u64::MAX);
        sim.run_until(3).unwrap();
        for index in 0..260 {
            assert_eq!(
                sim.get(sim.signal(&format!("count_{index}"))),
                131074u32.into(),
                "process {index}, hotspot={hotspot}"
            );
        }
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert_eq!(stats.native_service_calls, 0, "{stats:?}");
    }
}

#[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
#[test]
fn native_budget_is_shared_by_checkpoints_and_file_calls() {
    for file_first in [false, true] {
        let checkpoint = "repeat (65537) count++;";
        let file = "$fwrite(1, \"bounded\");";
        let body = if file_first {
            format!("{file} {checkpoint}")
        } else {
            format!("{checkpoint} {file}")
        };
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top; bit clk = 0; int count = 0; always #1 clk = ~clk; initial forever begin @(posedge clk); {body} end endmodule"
        );
        for four_state in [false, true] {
            let mut results = Vec::new();
            for mode in [
                crate::SchedulerMode::Reference,
                crate::SchedulerMode::Compiled,
            ] {
                let mut sim = crate::Simulation::from_sv_sources(
                    vec![(
                        source.as_str(),
                        std::path::Path::new("mixed-return-budget.sv"),
                    )],
                    "Top",
                )
                .four_state(four_state)
                .scheduler_mode(mode)
                .build_native()
                .unwrap();
                sim.state.set_slot_budgets(32, 1);
                let error = sim.run_until(2).unwrap_err();
                assert!(error.to_string().contains("does not suspend"), "{error}");
                results.push((
                    sim.time(),
                    sim.get(sim.signal("count")),
                    sim.drain_runtime_events(),
                ));
                if mode == crate::SchedulerMode::Compiled {
                    assert!(sim.scheduler_stats().compiled_slots > 0);
                }
            }
            assert_eq!(
                results[0], results[1],
                "file_first={file_first}, four_state={four_state}"
            );
        }
    }
}

#[test]
fn first_tier_scheduler_continuations_exceed_one_byte() {
    let mut source = String::from("module Top(input bit [7:0] d");
    for index in 0..260 {
        source.push_str(&format!(", input bit c{index}, output bit [7:0] q{index}"));
    }
    source.push_str("); wire [7:0] next_value = d + 1;\n");
    for index in 0..260 {
        source.push_str(&format!(
            "always_ff @(posedge c{index}) q{index} <= next_value;\n"
        ));
    }
    source.push_str("endmodule");
    let mut sim =
            crate::Simulation::from_sv_sources(
                vec![(
                    source.as_str(),
                    std::path::Path::new("many-continuations.sv"),
                )],
                "Top",
            )
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_with_backend(
                crate::MemoryLayoutMode::Packed,
                false,
                false,
                |program, options| {
                    let first = crate::scheduler::without_fused_domains(program.clone(), true);
                    let unit = first
                        .runtime
                        .runtime_schema
                        .scheduler
                        .as_ref()
                        .unwrap()
                        .unit
                        .unwrap();
                    let cont = first.runtime.runtime_schema.scheduler.as_ref().unwrap().slots.cont;
                    assert!(first.sir.processes[unit].blocks.values().flat_map(|block| &block.instructions).any(|inst| {
                        matches!(inst, crate::ir::SIRInstruction::Load(_, address, crate::ir::SIROffset::Static(8), 8)
                            if address.absolute_addr() == cont)
                    }), "the fixture must dispatch the high byte of a continuation");
                    crate::NativeBackend::new(&first, options)
                },
            )
            .unwrap();
    let d = sim.signal("d");
    sim.modify(|io| io.set(d, 41u8)).unwrap();
    sim.add_clock("c259", 10, 5);
    sim.run_until(25).unwrap();
    let q = sim.signal("q259");
    assert_eq!(sim.get(q), 42u8.into());
    assert!(sim.scheduler_stats().compiled_slots > 0);
}

/// A first tier settles before a domain instead of compiling a fused
/// copy of the settle into the scheduler for it.
#[test]
fn first_tier_scheduler_settles_instead_of_fusing() {
    let source = r#"
            module Top (clk: input clock, d: input bit<8>, q: output bit<8>) {
                var next: bit<8>;
                assign next = d + 1;
                always_ff (clk) { q = next; }
            }
        "#;
    let (program, _, _, _, _) = crate::Simulation::builder(source, "Top")
        .emit_triggers()
        .four_state(false)
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    let unit = program
        .runtime
        .runtime_schema
        .scheduler
        .as_ref()
        .unwrap()
        .unit
        .unwrap();
    let size = |program: &crate::ir::LaidOutProgram| {
        program.sir.processes[unit]
            .blocks
            .values()
            .map(|block| block.instructions.len())
            .sum::<usize>()
    };
    assert!(
        !program.sir.eval_comb_apply_ffs.is_empty(),
        "the fixture fuses its domain"
    );
    let first_tier = crate::scheduler::without_fused_domains(program.clone(), true);
    assert!(size(&first_tier) < size(&program));
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
fn count_clean_clock_batch(clock_data: bool, alias: bool, dirty: bool) -> (u64, usize) {
    use crate::SimBackend;
    let source = r#"
            module Top (
                a: input 'domain_a clock, b: input 'domain_b clock,
                d: input bit<8>, q: output 'domain_a bit<8>, r: output 'domain_b bit<8>,
                monitor: output bit<8>, evaluations: input bit<64>,
            ) {
                assign monitor = __COMB__;
                unsafe (cdc) {
                    always_ff (a) { q = __INPUT__; }
                    always_ff (b) { r = d; }
                }
            }
        "#;
    let mut source = source
        .replace("__COMB__", if clock_data { "b as u8" } else { "d + 1" })
        .replace("__INPUT__", if clock_data { "d" } else { "monitor" });
    if alias {
        source = source.replace(
            "always_ff (a) { q = monitor; }",
            "inst child: Child (clk: a, d: monitor, q);",
        );
        source.push_str("module Child (clk: input clock, d: input bit<8>, q: output bit<8>) { always_ff { q = d; } }");
    }
    let (mut program, _, options, _, _) = crate::Simulation::builder(&source, "Top")
        .emit_triggers()
        .four_state(false)
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    if alias {
        assert!(program.sir.eval_comb.iter().flat_map(|u| u.blocks.values())
                .flat_map(|b| &b.instructions).any(|inst| matches!(inst,
                    crate::ir::SIRInstruction::Store(_, _, _, _, triggers, _) if !triggers.is_empty()
                )), "the fixture must retain clock notifications");
    }
    let counter = program.get_addr(&[], &["evaluations"]).unwrap();
    let fused = crate::scheduler::count_test_settles(&mut program, counter, counter);
    let scheduler = program.runtime.runtime_schema.scheduler.clone().unwrap();
    let mut backend = crate::NativeBackend::new(&program, &options).unwrap();
    let slots = scheduler
        .slots
        .map(|address| backend.resolve_signal(&address));
    let count = backend.resolve_signal(&counter);
    let a = program.get_addr(&[], &["a"]).unwrap();
    let bit = scheduler
        .clocks
        .iter()
        .position(|&clock| clock == a)
        .unwrap();
    let element = |mut signal: crate::SignalRef| {
        signal.offset += bit * 8;
        signal.width = 64;
        signal
    };
    if alias {
        // A falling edge only publishes the clock alias: no FF output
        // needs a settle to propagate back through the child boundary.
        let clock = backend.resolve_signal(&a);
        backend.set(clock, 1u8);
    }
    backend.eval_comb().unwrap();
    backend.set(count, 0u64);
    backend.set(slots.limit, 0u64);
    backend.set(slots.dirty, u8::from(dirty));
    if dirty {
        let d = backend.resolve_signal(&program.get_addr(&[], &["d"]).unwrap());
        backend.set(d, 7u8);
    }
    backend.set(slots.clock_armed, 1u64 << bit);
    backend.set(slots.clock_value, if alias { 0 } else { 1u64 << bit });
    backend.set(element(slots.clock_next), 0u64);
    backend.set(element(slots.clock_period), 2u64);
    backend.run_process(scheduler.unit.unwrap()).unwrap();
    let q = backend.resolve_signal(&program.get_addr(&[], &["q"]).unwrap());
    assert_eq!(
        backend.get_as::<u8>(q),
        if clock_data || alias {
            0
        } else if dirty {
            8
        } else {
            1
        }
    );
    (backend.get_as::<u64>(count), fused)
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn partitioned_host_clock_glue_invalidates_its_comb_readers() {
    let source = r#"module Top(clk: input clock, d: input bit<8>, low: output bit<8>, probe: output bit<8>, side: output bit<8>, q: output bit<8>) {
            assign low = (clk as u8) + d;
            assign probe = low + 1;
            assign side = d ^ 3;
            always_ff { q = probe; }
        }"#;
    for four_state in [false, true] {
        let mut sim = crate::Simulation::builder(source, "Top")
            .four_state(four_state)
            .settle_partition(crate::SettlePartition::Always)
            .settle_check(true)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
        assert!(
            sim.simulator
                .program
                .runtime_schema
                .settle_partitions
                .is_some()
        );
        let d = sim.signal("d");
        sim.modify(|io| io.set(d, 7u8)).unwrap();
        sim.add_clock("clk", 2, 1);
        sim.run_until(8).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 9u8.into());
        assert_eq!(sim.get(sim.signal("probe")), 8u8.into());
        let stats = sim.scheduler_stats();
        assert!(
            stats.compiled_slots > 0 && stats.checked_steps > 0,
            "{stats:?}"
        );
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn partitioned_compiled_run_marks_native_nbas() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q, output wire [7:0] y, output wire clock_echo);
    bit clk = 0;
    wire [7:0] a = q + 3;
    wire [7:0] b = a ^ 8'h35;
    assign y = b + 7;
    assign clock_echo = clk;
    always #1 clk = !clk;
    initial begin q = 0; #2; q <= 5; #3; q <= 12; #3; q <= 7; #2; $write("q=%0d", q); end
endmodule"#;
    for four_state in [false, true] {
        let mut sim = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("partitioned-native-nba.sv"))],
            "Top",
        )
        .four_state(four_state)
        .settle_partition(crate::SettlePartition::Always)
        .settle_check(true)
        .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        sim.run_until(12).unwrap();
        assert_eq!(sim.get(sim.signal("q")), 7u8.into());
        assert_eq!(sim.get(sim.signal("y")), (((7u8 + 3) ^ 0x35) + 7).into());
        let stats = sim.scheduler_stats();
        assert!(
            stats.compiled_slots > 0 && stats.checked_steps > 0,
            "{stats:?}"
        );
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn incremental_clock_settle_matches_full_reference_with_mixed_writes() {
    let source = r#"`timescale 1ns/1ns
module Top(output wire [1023:0] padding, output wire observed_clock, output logic [7:0] held, output wire [63:0] now_value);
    bit clk = 0;
    logic [7:0] epoch = 0;
    logic enable = 1;
    logic [31:0] payload = 1;
    integer seen = 0;
    wire watched = ^padding;
    assign observed_clock = clk ^ enable ^ epoch[1];
    assign now_value = $time;
    always_latch if (enable) held = payload[7:0];
    always #1 begin clk = !clk; epoch = epoch + 1; end
    always @(posedge clk) payload <= payload + 1;
    initial forever begin @(watched); seen = seen + 1; end
    for (genvar i = 0; i < 32; i = i + 1) begin
        assign padding[i*32 +: 32] = (payload + i) * (payload ^ (i + 1));
    end
    initial begin #4; enable = 'x; #4; enable = 'z; #4; enable = 0; #4; enable = 1; end
    initial begin #24; if (seen == 0) $fatal(1, "wait never woke"); $finish; end
endmodule"#;
    for four_state in [false, true] {
        let builder = || {
            crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("incremental-clock.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        let (program, _, _, _, _) = builder()
            .emit_triggers()
            .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
            .unwrap();
        assert!(
            crate::scheduler::incremental_inputs_for_test(&program)
                .is_some_and(|inputs| inputs >= 2),
            "clock-only writes must use the small cone"
        );
        let mut simulation = builder()
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
        simulation.run_until(28).unwrap();
        assert!(simulation.is_finished());
        let stats = simulation.scheduler_stats();
        assert!(
            stats.compiled_slots > 0 && stats.checked_steps > 0,
            "{stats:?}"
        );
    }
}

/// An intra-assignment delay on a nonblocking assignment evaluates the
/// right-hand side at the edge and publishes the update at the due time
/// (IEEE 1800-2023 9.4.5, 10.4.2). The flip-flop still fuses into its
/// clock domain instead of running as a process kernel per edge.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn delayed_nonblocking_ff_is_fused_and_matches_reference() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] q, output logic [7:0] seen);
    bit clk = 0;
    bit rst_n = 0;
    logic [7:0] d = 8'h11;
    always #5 clk = ~clk;
    always @(posedge clk or negedge rst_n) begin
        if (!rst_n) q <= 8'h00;
        else q <= #1 d;
    end
    always @(posedge clk) seen <= q;
    initial begin
        #12 rst_n = 1;
        #10 d = 8'h22;
        #10 d = 8'h33;
        #40 $finish;
    end
endmodule"#;
    for four_state in [false, true] {
        let builder = || {
            crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("delayed-ff.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        let (program, _, _, _, _) = builder()
            .emit_triggers()
            .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
            .unwrap();
        assert!(
            !program.sir.eval_apply_ffs.is_empty(),
            "a delayed nonblocking flip-flop must fuse into its clock domain"
        );
        let mut simulation = builder()
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
        // The edge at t=15 captures d=8'h11; the update lands at t=16,
        // so at t=15 the old value is still visible.
        simulation.run_until(15).unwrap();
        assert_eq!(simulation.get(simulation.signal("q")), 0u8.into());
        simulation.run_until(16).unwrap();
        assert_eq!(simulation.get(simulation.signal("q")), 0x11u8.into());
        simulation.run_until(26).unwrap();
        assert_eq!(simulation.get(simulation.signal("q")), 0x22u8.into());
        simulation.run_until(72).unwrap();
        assert!(simulation.is_finished());
        let stats = simulation.scheduler_stats();
        assert!(
            stats.compiled_slots > 0 && stats.checked_steps > 0,
            "{stats:?}"
        );
    }
}

/// A flip-flop's stage holds one delayed value per run. A run that
/// assigns a variable twice schedules two updates, each an event of its
/// own (IEEE 1800-2023 10.4.2), and a part-select updates its bits
/// alone: such an edge process, `always_ff` included, keeps the queue.
/// Assignments in alternative branches stay in the flip-flop.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn delayed_nonblocking_a_stage_cannot_hold_keeps_the_queue() {
    for keyword in ["always", "always_ff"] {
        let source = |body: &str| {
            format!(
                r#"`timescale 1ns/1ns
module Top(input bit sel, output int pulses);
    bit clk;
    bit q;
    bit [7:0] w = 8'ha0;
    always #5 clk = ~clk;
    {keyword} @(posedge clk) begin {body} end
    always @(posedge q) pulses++;
endmodule"#
            )
        };
        fn builder(source: &str) -> crate::SimulatorBuilder<'_, crate::Simulation> {
            crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("delayed-queue.sv"))],
                "Top",
            )
        }
        let clock_domains = |source: &str| {
            let (program, _, _, _, _) = builder(source)
                .emit_triggers()
                .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
                .unwrap_or_else(|error| panic!("{keyword}: {error}"));
            program.sir.eval_apply_ffs.len()
        };
        let twice = source("q <= #1 1'b1; q <= #1 1'b0;");
        let looped = source("for (int i = 0; i < 2; i++) q <= #1 ~i[0];");
        let part = source("w[3:0] <= #1 4'hf;");
        let alternatives = source("if (sel) q <= #1 1'b1; else q <= #1 1'b0;");
        let fused = clock_domains(&alternatives);
        for queued in [&twice, &looped, &part] {
            assert_eq!(clock_domains(queued) + 1, fused, "{keyword}: {queued}");
        }
        for mode in [
            crate::SchedulerMode::Reference,
            crate::SchedulerMode::Compiled,
        ] {
            let run = |source: &str| {
                let mut simulation = builder(source)
                    .scheduler_mode(mode)
                    .build_native()
                    .unwrap_or_else(|error| panic!("{keyword}: {error}"));
                simulation.run_until(30).unwrap();
                simulation
            };
            // The edges at 5, 15 and 25 each raise and lower `q` one
            // time unit later.
            for pulsed in [&twice, &looped] {
                let mut simulation = run(pulsed);
                let pulses = simulation.signal("pulses");
                assert_eq!(simulation.get(pulses), 3u32.into(), "{keyword} {mode:?}");
                let q = simulation.signal("q");
                assert_eq!(simulation.get(q), 0u8.into(), "{keyword} {mode:?}");
            }
            let mut simulation = run(&part);
            let w = simulation.signal("w");
            assert_eq!(simulation.get(w), 0xafu8.into(), "{keyword} {mode:?}");
            let mut simulation = run(&alternatives);
            let pulses = simulation.signal("pulses");
            assert_eq!(simulation.get(pulses), 0u32.into(), "{keyword} {mode:?}");
        }
    }
}

/// Sampled values are captured at the beginning of each timestamp
/// (IEEE 1800-2023 4.4.2.5, 16.5.1). A compiled run spanning many
/// timestamps refreshes the sampled copies at its own slot starts,
/// so a reader deep inside a batched run still sees each slot's
/// beginning-of-slot value, matching the reference loop.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn sampled_shadows_refresh_inside_batched_compiled_runs() {
    let source = r#"`timescale 1ns/1ns
module Top(input bit clk, output logic [3:0] count, output logic [3:0] seen);
    always @(posedge clk) count <= count + 1;
    always @(posedge clk) seen <= $sampled(count);
endmodule"#;
    let mut simulation = crate::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("sampled-batched.sv"))],
        "Top",
    )
    .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
    .build_native()
    .unwrap();
    simulation.add_clock("clk", 1, 1);
    // One long run: many timestamps inside single compiled runs.
    // Posedges at t=1,3,...,19: count reaches 10, and `seen` holds
    // the count sampled at the beginning of the last posedge's slot.
    simulation.run_until(20).unwrap();
    assert_eq!(simulation.get(simulation.signal("count")), 10u8.into());
    assert_eq!(simulation.get(simulation.signal("seen")), 9u8.into());
    let stats = simulation.scheduler_stats();
    assert!(stats.checked_steps > 0, "{stats:?}");
}

/// A sampled value is that of the Preponed region of its time slot
/// (IEEE 1800-2023 4.4.2.1, 16.5.1). A compiled run that takes up work
/// of the timestamp the last run ended at, here a host write, keeps
/// the copies of that slot; it refreshes them only as time advances.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn sampled_shadows_keep_the_slot_start_within_a_timestamp() {
    let source = r#"`timescale 1ns/1ns
module Top(input bit clk, input bit go, output logic [3:0] count, output logic [3:0] seen);
    always @(posedge clk) count <= count + 1;
    always @(posedge go) seen = $sampled(count);
endmodule"#;
    for mode in [
        crate::SchedulerMode::Reference,
        crate::SchedulerMode::Compiled,
        crate::SchedulerMode::DifferentialRuns,
    ] {
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("sampled-same-slot.sv"))],
            "Top",
        )
        .scheduler_mode(mode)
        .build_native()
        .unwrap();
        simulation.add_clock("clk", 1, 1);
        // Posedges at 1, 3 and 5: the slot at 5 begins with 2.
        simulation.run_until(5).unwrap();
        let count = simulation.signal("count");
        assert_eq!(simulation.get(count), 3u8.into(), "{mode:?}");
        let go = simulation.signal("go");
        simulation.modify(|io| io.set(go, 1u8)).unwrap();
        simulation.run_until(5).unwrap();
        let seen = simulation.signal("seen");
        assert_eq!(simulation.get(seen), 2u8.into(), "{mode:?}");
        // The next timestamp samples again.
        simulation.modify(|io| io.set(go, 0u8)).unwrap();
        simulation.run_until(7).unwrap();
        simulation.modify(|io| io.set(go, 1u8)).unwrap();
        simulation.run_until(7).unwrap();
        assert_eq!(simulation.get(seen), 3u8.into(), "{mode:?}");
    }
}

/// The tiered layout instruments flip-flop units with the wide
/// commits only a native image compiles (raw state words shadowed
/// by typed commits). The interpreted tier executes the same units
/// before adoption, so it must run those raw accesses as plain
/// state loads and stores instead of extern calls.
#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn tiered_wide_ff_commits_match_reference() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [63:0] wide_q, output logic [7:0] a_q, output logic [7:0] b_q);
    bit clk = 0;
    always #5 clk = ~clk;
    initial begin wide_q = 0; a_q = 0; b_q = 0; end
    always @(posedge clk) begin
        wide_q <= wide_q + 64'd1;
        a_q <= a_q + 8'd3;
        b_q <= b_q ^ a_q;
    end
endmodule"#;
    let build = || {
        crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("tiered-wide-ff.sv"))],
            "Top",
        )
    };
    let mut reference = build()
        .scheduler_mode(crate::SchedulerMode::Reference)
        .build_native()
        .unwrap();
    // Adoption is delayed so the interpreted tier runs the wide
    // units first; the compiled tier adopts them later.
    let mut tiered = build()
        .tier_promotion(crate::TierPromotion::AfterSteps(64))
        .build_tiered()
        .unwrap();
    for end in [10, 20, 40, 80] {
        reference.run_until(end).unwrap();
        tiered.run_until(end).unwrap();
        for name in ["wide_q", "a_q", "b_q"] {
            assert_eq!(
                tiered.get(tiered.signal(name)),
                reference.get(reference.signal(name)),
                "{name} at t={end}"
            );
        }
    }
    assert_eq!(tiered.get(tiered.signal("wide_q")), 8u8.into());
}

/// A partitioned settle has a clock cone of its own: the partitions
/// the kernels' clock writes dirty and those they publish to. The slot
/// after a clock toggle runs only the cone; a kernel writing data as
/// well runs the whole settle.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn partitioned_clock_cone_matches_the_single_pass_settle() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] fast, output logic [7:0] slow, output logic [7:0] jcount, output wire [7:0] view, output wire tick, output wire [7:0] mix);
    bit clk = 0;
    bit jclk = 0;
    logic en = 0;
    logic [7:0] din = 0;
    always #5 clk = ~clk;
    always #7 jclk = ~jclk;
    always #11 en = ~en;
    always #3 din = din + 8'd29;
    logic en_l;
    always_latch if (!clk) en_l = en;
    wire gclk = clk & en_l;
    assign tick = clk ^ jclk;
    initial begin fast = 0; slow = 0; jcount = 0; end
    always @(posedge clk) fast <= fast + din;
    always @(posedge gclk) slow <= slow ^ fast;
    always @(posedge jclk) jcount <= jcount + 8'd1;
    assign view = slow + fast + jcount + {7'd0, tick};
    // Data logic outside the clock cone.
    logic [31:0] d0, d1, d2, d3, d4, d5, d6, d7;
    assign d0 = {din, fast, slow, jcount} * 32'd2654435761;
    assign d1 = d0 ^ (d0 >> 7);
    assign d2 = d1 + {jcount, din, fast, slow};
    assign d3 = d2 ^ (d2 << 3);
    assign d4 = d3 * 32'd40503;
    assign d5 = d4 ^ (d4 >> 11);
    assign d6 = d5 + d1;
    assign d7 = d6 ^ d3;
    assign mix = d7[7:0] ^ d7[15:8] ^ d7[23:16] ^ d7[31:24];
    // A kernel waiting on the data outputs keeps their logic eager: the
    // settle is not deferred.
    integer hits = 0;
    always @(mix or view) hits = hits + 1;
endmodule"#;
    let builder = || {
        crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("clock-cone.sv"))],
            "Top",
        )
        .settle_partition(crate::SettlePartition::Always)
    };
    let (program, _, _, _, _) = builder()
        .emit_triggers()
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    assert!(
        crate::scheduler::incremental_inputs_for_test(&program).is_some_and(|inputs| inputs >= 2),
        "the clock writes must use the partition cone"
    );
    let trace = |mut simulation: crate::Simulation| {
        let outputs =
            ["fast", "slow", "jcount", "view", "tick"].map(|name| simulation.signal(name));
        let mut trace = Vec::new();
        for time in 0..120 {
            simulation.run_until(time).unwrap();
            trace.push(outputs.map(|signal| simulation.get(signal)));
        }
        (trace, simulation.scheduler_stats())
    };
    let (cone, stats) = trace(
        builder()
            .settle_check(true)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap(),
    );
    assert!(
        stats.compiled_slots > 0 && stats.checked_steps > 0,
        "{stats:?}"
    );
    let (single, _) = trace(
        crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("clock-cone.sv"))],
            "Top",
        )
        .disable_pass(crate::optimizer::SirPass::PartitionedSettle)
        .build_native()
        .unwrap(),
    );
    assert_eq!(cone, single);
    assert_ne!(single[119][1], 0u8.into(), "the gated domain must have run");
}

/// The kernels' data writes are cone sources too: the slot after a
/// write of `din`, declared before the clocks so that its mark comes
/// first in a pass that also toggles a clock, runs the cone of both,
/// and the data logic reading `din` is settled before anything
/// observes it.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn partitioned_clock_cone_covers_the_kernels_data_writes() {
    let source = r#"`timescale 1ns/1ns
module Top(output logic [7:0] fast, output logic [7:0] slow, output wire [7:0] view, output wire [7:0] mix, output wire [7:0] dmix);
    logic [7:0] din = 0;
    always #3 din = din + 8'd29;
    bit clk = 0;
    bit jclk = 0;
    logic en = 0;
    always #5 clk = ~clk;
    always #7 jclk = ~jclk;
    always #11 en = ~en;
    logic en_l;
    always_latch if (!clk) en_l = en;
    wire gclk = clk & en_l;
    initial begin fast = 0; slow = 0; end
    always @(posedge clk) fast <= fast + din;
    always @(posedge gclk) slow <= slow ^ fast;
    assign view = slow + fast + {7'd0, clk ^ jclk};
    // Data logic reading the kernel's write directly.
    logic [31:0] d0, d1, d2, d3;
    assign d0 = {din, din ^ 8'h5a, ~din, din + 8'd3} * 32'd2654435761;
    assign d1 = d0 ^ (d0 >> 7);
    assign d2 = d1 + {4{din}};
    assign d3 = d2 ^ (d2 << 3);
    assign dmix = d3[7:0] ^ d3[15:8] ^ d3[23:16] ^ d3[31:24];
    // Data logic of the registers.
    logic [31:0] e0, e1;
    assign e0 = {fast, slow, fast ^ slow, din} * 32'd40503;
    assign e1 = e0 ^ (e0 >> 11);
    assign mix = e1[7:0] ^ e1[15:8] ^ e1[23:16] ^ e1[31:24];
    integer hits = 0;
    always @(mix or view or dmix) hits = hits + 1;
endmodule"#;
    let builder = || {
        crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("data-cone.sv"))],
            "Top",
        )
        .settle_partition(crate::SettlePartition::Always)
    };
    let (program, _, _, _, _) = builder()
        .emit_triggers()
        .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
        .unwrap();
    assert!(
        crate::scheduler::incremental_inputs_for_test(&program).is_some(),
        "the kernel writes must use the partition cone"
    );
    let trace = |mut simulation: crate::Simulation| {
        let outputs = ["fast", "slow", "view", "mix", "dmix"].map(|name| simulation.signal(name));
        let mut trace = Vec::new();
        for time in 0..120 {
            simulation.run_until(time).unwrap();
            trace.push(outputs.map(|signal| simulation.get(signal)));
        }
        (trace, simulation.scheduler_stats())
    };
    let (cone, stats) = trace(
        builder()
            .settle_check(true)
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap(),
    );
    assert!(
        stats.compiled_slots > 0 && stats.checked_steps > 0,
        "{stats:?}"
    );
    let (single, _) = trace(
        crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("data-cone.sv"))],
            "Top",
        )
        .disable_pass(crate::optimizer::SirPass::PartitionedSettle)
        .build_native()
        .unwrap(),
    );
    assert_eq!(cone, single);
    assert_ne!(
        single[119][4], single[3][4],
        "the data logic must follow din"
    );
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
#[test]
fn combinational_rtl_wait_inputs_keep_unrelated_outputs_deferred() {
    let source = r#"`timescale 1ns/1ns
module Top(output wire [1023:0] padding);
    bit clk = 0;
    logic [31:0] payload = 1;
    integer seen = 0;
    wire watched = clk ^ payload[0];
    always #1 clk = !clk;
    always @(posedge clk) payload <= payload + 1;
    initial forever begin @(watched); seen = seen + 1; end
    for (genvar i = 0; i < 32; i = i + 1) begin
        assign padding[i*32 +: 32] = (payload + i) * (payload ^ (i + 1));
    end
    initial begin #20; if (seen == 0) $fatal(1, "wait never woke"); $finish; end
endmodule"#;
    for four_state in [false, true] {
        let builder = || {
            crate::Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("deferred-wait-inputs.sv"))],
                "Top",
            )
            .four_state(four_state)
        };
        let (mut program, _, _, _, _) = builder()
            .emit_triggers()
            .into_laid_out_program(crate::MemoryLayoutMode::Packed, false, false)
            .unwrap();
        let (deferred, _) = crate::scheduler::defers_settles_for_test(&program);
        assert!(
            deferred,
            "a small observed cone must not force whole-circuit settling"
        );
        // State-retention metadata is captured before optimization. Model
        // a removed comb writer whose home remains readable as an input.
        let clock = program.get_addr(&[], &["clk"]).unwrap();
        let stale =
            crate::ir::RegionedAbsoluteAddr::from_absolute_addr(celox_design::STABLE_REGION, clock);
        program.sir.eval_comb_stateful.as_mut().unwrap().push(stale);
        // Also model an old self-read eliminated from a surviving writer.
        let padding = program.get_addr(&[], &["padding"]).unwrap();
        program.sir.eval_comb_stateful.as_mut().unwrap().push(
            crate::ir::RegionedAbsoluteAddr::from_absolute_addr(
                celox_design::STABLE_REGION,
                padding,
            ),
        );

        assert!(
            crate::scheduler::defers_settles_for_test(&program).0,
            "a stateful target without a remaining comb writer must not disable deferral"
        );
        let mut simulation = builder()
            .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            .build_native()
            .unwrap();
        simulation.run_until(24).unwrap();
        assert!(simulation.is_finished());
        let stats = simulation.scheduler_stats();
        assert!(
            stats.compiled_slots > 0 && stats.checked_steps > 0,
            "{stats:?}"
        );
    }
}
