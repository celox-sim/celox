//! Traces of native code generation.

use super::*;

pub(super) fn append_native_function_trace(
    optimized_sir: &mut String,
    mir: &mut String,
    reactive_graph: &mut String,
    state_layout: &mut String,
    name: &str,
    bindings: &[String],
    trace: &emit::NativeFunctionTrace,
) {
    let mut bindings = bindings.to_vec();
    bindings.sort();
    bindings.dedup();

    optimized_sir.push_str(&format!("=== Native function {name} ===\n"));
    if !bindings.is_empty() {
        optimized_sir.push_str("Bindings:\n");
        for binding in &bindings {
            optimized_sir.push_str(&format!("  {binding}\n"));
        }
    }
    optimized_sir.push_str(&trace.optimized_sir);
    if !trace.optimized_sir.ends_with('\n') {
        optimized_sir.push('\n');
    }
    optimized_sir.push('\n');

    mir.push_str(&format!("=== Native function {name} ===\n"));
    if !bindings.is_empty() {
        mir.push_str("Bindings:\n");
        for binding in &bindings {
            mir.push_str(&format!("  {binding}\n"));
        }
    }
    mir.push_str("--- MIR after main optimization, before regalloc-owned late folds ---\n");
    mir.push_str(&trace.mir_before_regalloc);
    if !trace.mir_before_regalloc.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str("--- MIR after late memory folds, before allocation-owned scheduling ---\n");
    mir.push_str(&trace.mir_after_late_memory_folds);
    if !trace.mir_after_late_memory_folds.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str("--- MIR after allocation-owned scheduling, before spill reconstruction ---\n");
    mir.push_str(&trace.mir_after_scheduling);
    if !trace.mir_after_scheduling.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str("--- MIR after register allocation and post-RA peepholes ---\n");
    mir.push_str(&trace.mir_after_regalloc);
    if !trace.mir_after_regalloc.ends_with('\n') {
        mir.push('\n');
    }
    mir.push_str(&format!("Spill frame: {} bytes\n", trace.spill_frame_size));
    mir.push_str("Register assignment:\n");
    mir.push_str(&trace.register_assignment);
    #[cfg(any(
        feature = "x86_64-codegen",
        all(target_arch = "x86_64", not(feature = "arm64-codegen"))
    ))]
    mir.push_str("x86-64 disassembly of emitted function:\n");
    #[cfg(any(
        feature = "arm64-codegen",
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    mir.push_str("AArch64 disassembly of emitted function:\n");
    mir.push_str(&trace.disassembly);
    if !trace.disassembly.ends_with('\n') {
        mir.push('\n');
    }
    mir.push('\n');

    if !trace.reactive_graph.is_empty() {
        reactive_graph.push_str(&format!("=== Native function {name} ===\n"));
        if !bindings.is_empty() {
            reactive_graph.push_str("Bindings:\n");
            for binding in &bindings {
                reactive_graph.push_str(&format!("  {binding}\n"));
            }
        }
        reactive_graph.push_str(&trace.reactive_graph);
        if !trace.reactive_graph.ends_with('\n') {
            reactive_graph.push('\n');
        }
        reactive_graph.push('\n');
    }

    if !trace.state_layout.is_empty() {
        state_layout.push_str(&format!("=== Native function {name} ===\n"));
        if !bindings.is_empty() {
            state_layout.push_str("Bindings:\n");
            for binding in &bindings {
                state_layout.push_str(&format!("  {binding}\n"));
            }
        }
        state_layout.push_str(&trace.state_layout);
        if !trace.state_layout.ends_with('\n') {
            state_layout.push('\n');
        }
        state_layout.push('\n');
    }
}

pub(super) fn format_native_codegen_trace(
    comb: &CompiledNativeFunction,
    ff_codes: &HashMap<usize, CompiledNativeFunction>,
    tasks: &[NativeCompileTask<'_>],
    processes: &[CompiledNativeFunction],
) -> NativeCodegenTrace {
    let mut optimized_sir = String::from("=== Optimized SIR used by native emission ===\n");
    let mut mir = String::from("=== MIR used by native emission ===\n");
    let mut reactive_graph = String::from("=== Reactive clock-event projection oracle ===\n");
    let mut state_layout =
        String::from("=== Profile-selected native state-layout feasibility ===\n");
    append_native_function_trace(
        &mut optimized_sir,
        &mut mir,
        &mut reactive_graph,
        &mut state_layout,
        "eval_comb",
        &[],
        comb.trace
            .as_ref()
            .expect("explicit native trace must capture eval_comb"),
    );

    let mut ff_entries = ff_codes
        .keys()
        .map(|&task_id| {
            let task = &tasks[task_id];
            let mut sort_key = task.bindings.clone();
            sort_key.sort();
            (sort_key, task_id, task)
        })
        .collect::<Vec<_>>();
    ff_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut label_indices = HashMap::<&str, usize>::default();
    for (_, task_id, task) in ff_entries {
        let index = label_indices.entry(task.label).or_default();
        let name = format!("{}[{index}]", task.label);
        *index += 1;
        append_native_function_trace(
            &mut optimized_sir,
            &mut mir,
            &mut reactive_graph,
            &mut state_layout,
            &name,
            &task.bindings,
            ff_codes[&task_id]
                .trace
                .as_ref()
                .expect("explicit native trace must capture every FF function"),
        );
    }
    for (index, process) in processes.iter().enumerate() {
        append_native_function_trace(
            &mut optimized_sir,
            &mut mir,
            &mut reactive_graph,
            &mut state_layout,
            &format!("process[{index}]"),
            &[],
            process
                .trace
                .as_ref()
                .expect("explicit native trace must capture every process"),
        );
    }
    NativeCodegenTrace {
        optimized_sir,
        mir,
        reactive_graph,
        state_layout,
    }
}
