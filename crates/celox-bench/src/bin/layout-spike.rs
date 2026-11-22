//! Stage 0 spike: compare the physical layout + event space produced from
//! cheap (O0) SIR against full (O2) SIR on the same design, in both packed and
//! element-strided layout modes, including the assigned event IDs.
//!
//! The tiered fast-start plan needs the Tier-0 interpreter (built on cheap
//! SIR) to share the memory image layout and trigger-ID space with the
//! background full-pipeline compilation. If the layouts agree, promotion can
//! reuse the existing `tier_transfer` whole-image handoff.

use std::collections::BTreeMap;
use std::path::PathBuf;

use celox::UpdateEntry;
use celox::{
    AbsoluteAddr, MemoryLayout, MemoryLayoutMode, OptLevel, OptimizeOptions, OptimizedSir,
    SirProgram, compile_to_sir,
};
use fxhash::FxHashMap;
use veryl_metadata::Metadata;

#[expect(clippy::disallowed_macros, reason = "CLI usage is written to stderr")]
fn usage() -> ! {
    eprintln!("usage: layout-spike --project <dir> --test <name> [--four-state]");
    std::process::exit(2);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut project: Option<PathBuf> = None;
    let mut test: Option<String> = None;
    let mut four_state = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--project" => project = args.next().map(PathBuf::from),
            "--test" => test = args.next(),
            "--four-state" => four_state = true,
            _ => usage(),
        }
    }
    let (Some(project), Some(test)) = (project, test) else {
        usage()
    };

    let toml_path = Metadata::search_from(&project).expect("find Veryl.toml");
    let mut metadata = Metadata::load(&toml_path).expect("load Veryl metadata");
    let paths: Vec<PathBuf> = metadata
        .paths::<&str>(&[], false, false)
        .expect("metadata paths")
        .into_iter()
        .filter(|path| !path.example)
        .map(|path| path.src)
        .collect();
    let sources: Vec<String> = paths
        .iter()
        .map(|path| std::fs::read_to_string(path).expect("read source"))
        .collect();
    let source_refs: Vec<(&str, &std::path::Path)> = sources
        .iter()
        .zip(paths.iter())
        .map(|(source, path)| (source.as_str(), path.as_path()))
        .collect();

    let (cheap, full) = (
        build(&source_refs, &test, &metadata, OptLevel::O0, four_state),
        build(&source_refs, &test, &metadata, OptLevel::O2, four_state),
    );

    let mut failures = 0;
    for (cheap, full) in cheap.iter().zip(&full) {
        println!("layout-mode {:?}", cheap.0.mode);
        failures += compare(cheap, full);
    }
    println!(
        "layout-spike {}",
        if failures == 0 { "AGREE" } else { "DISAGREE" }
    );
    std::process::exit(if failures == 0 { 0 } else { 1 });
}

fn compare(cheap: &LayoutAndEvents, full: &LayoutAndEvents) -> usize {
    let mut failures = 0;
    let mut check = |name: &str, same: bool, detail: String| {
        println!(
            "{} {} {}",
            if same { "SAME " } else { "DIFFER" },
            name,
            detail
        );
        if !same {
            failures += 1;
        }
    };

    check(
        "stable-offsets",
        cheap.0.offsets == full.0.offsets,
        format!(
            "cheap={} full={}",
            cheap.0.offsets.len(),
            full.0.offsets.len()
        ),
    );
    check(
        "stable-widths",
        cheap.0.widths == full.0.widths,
        format!(
            "cheap={} full={}",
            cheap.0.widths.len(),
            full.0.widths.len()
        ),
    );
    check(
        "stable-sizes",
        cheap.0.total_size == full.0.total_size,
        format!("cheap={} full={}", cheap.0.total_size, full.0.total_size),
    );
    check(
        "working-offsets",
        cheap.0.working_offsets == full.0.working_offsets,
        format!(
            "cheap={} full={}",
            cheap.0.working_offsets.len(),
            full.0.working_offsets.len()
        ),
    );
    check(
        "sparse-offsets",
        cheap.0.sparse_offsets == full.0.sparse_offsets,
        format!(
            "cheap={} full={}",
            cheap.0.sparse_offsets.len(),
            full.0.sparse_offsets.len()
        ),
    );
    check(
        "merged-total-size",
        cheap.0.merged_total_size == full.0.merged_total_size,
        format!(
            "cheap={} full={}",
            cheap.0.merged_total_size, full.0.merged_total_size
        ),
    );
    check(
        "unpacked-arrays",
        cheap.0.unpacked_arrays == full.0.unpacked_arrays,
        format!(
            "cheap={} full={}",
            cheap.0.unpacked_arrays.len(),
            full.0.unpacked_arrays.len()
        ),
    );
    check(
        "events",
        cheap.1 == full.1,
        format!("cheap={} full={}", cheap.1.len(), full.1.len()),
    );

    if failures == 0 {
        println!(
            "stable-offset-entries={} working-entries={} events={}",
            cheap.0.offsets.len(),
            cheap.0.working_offsets.len(),
            cheap.1.len()
        );
    } else {
        diff_map("stable", &cheap.0.offsets, &full.0.offsets);
        diff_map("working", &cheap.0.working_offsets, &full.0.working_offsets);
        diff_map("sparse", &cheap.0.sparse_offsets, &full.0.sparse_offsets);
        diff_map("event", &cheap.1, &full.1);
    }
    failures
}

fn diff_map<K: Ord + std::fmt::Debug, V: PartialEq + std::fmt::Debug>(
    name: &str,
    cheap: &fxhash::FxHashMap<K, V>,
    full: &fxhash::FxHashMap<K, V>,
) {
    let cheap: BTreeMap<_, _> = cheap.iter().collect();
    let full: BTreeMap<_, _> = full.iter().collect();
    let mut shown = 0;
    for (key, c) in &cheap {
        match full.get(key) {
            Some(f) if f == c => {}
            other => {
                println!("{}-diff key={:?} cheap={:?} full={:?}", name, key, c, other);
                shown += 1;
                if shown >= 10 {
                    return;
                }
            }
        }
    }
    for key in full.keys() {
        if !cheap.contains_key(key) {
            println!("{}-missing-in-cheap key={:?}", name, key);
            shown += 1;
            if shown >= 10 {
                return;
            }
        }
    }
}

type EventIds = FxHashMap<(&'static str, AbsoluteAddr), usize>;
type LayoutAndEvents = (MemoryLayout, EventIds);

fn build(
    sources: &[(&str, &std::path::Path)],
    test: &str,
    metadata: &Metadata,
    level: OptLevel,
    four_state: bool,
) -> [LayoutAndEvents; 2] {
    let (program, _warnings) = compile_to_sir(
        sources,
        test,
        &[],
        &[],
        four_state,
        &celox::TraceOptions {
            sim_modules: false,
            pre_atomized_comb_blocks: false,
            atomized_comb_blocks: false,
            flattened_comb_blocks: false,
            scheduled_units: false,
            settle_plan: false,
            settle_plan_output: None,
            pre_optimized_sir: false,
            post_optimized_sir: false,
            analyzer_ir: false,
            pre_optimized_clif: false,
            post_optimized_clif: false,
            native: false,
            mir: false,
            native_profile_blocks: Vec::new(),
            output_to_stdout: false,
        },
        None,
        Some(metadata.clone()),
        None,
        None,
        &[],
        &OptimizeOptions::new(level),
    )
    .expect("compile to SIR");
    snapshots(&program, four_state)
}

fn snapshots(program: &OptimizedSir, four_state: bool) -> [LayoutAndEvents; 2] {
    let events = event_ids(
        &program.sir,
        &program.design.events.aliases,
        &program
            .design
            .events
            .execution_order(program.sir.updates.events.keys().copied()),
    );
    // Native tiered execution uses ElementStrided; VCD and Cranelift use
    // Packed. Check both without recompiling the SIR for each layout.
    [MemoryLayoutMode::Packed, MemoryLayoutMode::ElementStrided].map(|mode| {
        (
            MemoryLayout::build(program, four_state, mode),
            events.clone(),
        )
    })
}

fn event_ids(
    sir: &SirProgram,
    aliases: &FxHashMap<AbsoluteAddr, AbsoluteAddr>,
    order: &[AbsoluteAddr],
) -> EventIds {
    let addr_to_id: FxHashMap<_, _> = order
        .iter()
        .enumerate()
        .map(|(index, addr)| (*addr, index))
        .collect();
    let mut events = EventIds::default();
    for (group, units) in [
        ("eval-apply", UpdateEntry::Transition),
        ("eval-only", UpdateEntry::Sample),
        ("apply-only", UpdateEntry::Publish),
    ] {
        let mut group_ids = FxHashMap::default();
        for (addr, _) in sir.updates.groups(units) {
            let id = addr_to_id[aliases.get(addr).unwrap_or(addr)];
            group_ids.insert(*addr, id);
        }
        // Aliases reuse the canonical event's ID and are present only in
        // groups that can resolve the canonical address.
        for (alias, canonical) in aliases {
            if let Some(&id) = group_ids.get(canonical) {
                group_ids.insert(*alias, id);
            }
        }
        events.extend(group_ids.into_iter().map(|(addr, id)| ((group, addr), id)));
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox::{EventHandle, InterpBackend, SimBackend, SimulatorOptions};

    fn compile(source: &str, level: OptLevel, four_state: bool) -> OptimizedSir {
        compile_to_sir(
            &[(source, std::path::Path::new("layout-spike.veryl"))],
            "Top",
            &[],
            &[],
            four_state,
            &celox::TraceOptions::default(),
            None,
            None,
            None,
            None,
            &[],
            &OptimizeOptions::new(level),
        )
        .unwrap()
        .0
    }

    #[test]
    fn snapshots_detect_strided_differences_that_packed_layouts_hide() {
        const SOURCE: &str = r#"
            module Top (clk: input clock, data: input logic<9>, q: output logic<9>) {
                var mem: logic<9>[3];
                always_ff (clk) {
                    mem[0] = data;
                    mem[1] = mem[0];
                    mem[2] = mem[1];
                }
                assign q = mem[2];
            }
        "#;
        for four_state in [false, true] {
            let cheap = snapshots(&compile(SOURCE, OptLevel::O0, four_state), four_state);
            let full = snapshots(&compile(SOURCE, OptLevel::O2, four_state), four_state);
            for layouts in [&cheap, &full] {
                assert_eq!(layouts[0].0.mode, MemoryLayoutMode::Packed);
                assert_eq!(layouts[1].0.mode, MemoryLayoutMode::ElementStrided);
                assert!(layouts[0].0.unpacked_arrays.is_empty());
                assert_eq!(layouts[0].1, layouts[1].1);
            }
            assert!(cheap[1].0.unpacked_arrays.values().any(|array| {
                array.element_width == 9
                    && array.element_count == 3
                    && array.element_stride == 2
                    && array.plane_size == 6
            }));
            // O2 coalesces accesses across the padded elements. Packed
            // layouts still agree, but strided selection and offsets do not.
            assert_eq!(compare(&cheap[0], &full[0]), 0);
            assert!(compare(&cheap[1], &full[1]) > 0);
        }
    }

    fn address(id: u32) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: celox_design::InstanceId(0),
            var_id: celox_design::StateObjectId(id),
        }
    }

    fn empty_sir() -> SirProgram {
        SirProgram {
            updates: Default::default(),
            parallel: None,
            processes: Vec::new(),
            functions: Vec::new(),
        }
    }

    #[test]
    fn event_ids_share_one_space_and_expand_aliases_per_group() {
        let [a, b, c, alias_a, alias_b, unresolved] = [0, 1, 2, 3, 4, 5].map(address);
        let mut sir = empty_sir();
        for (addr, entry) in [
            (a, UpdateEntry::Transition),
            (a, UpdateEntry::Sample),
            (b, UpdateEntry::Sample),
            (a, UpdateEntry::Publish),
            (c, UpdateEntry::Publish),
        ] {
            sir.updates
                .events
                .entry(addr)
                .or_default()
                .set(entry, Vec::new());
        }
        let aliases = [(alias_a, a), (alias_b, b), (unresolved, address(99))]
            .into_iter()
            .collect();
        assert_eq!(
            event_ids(&sir, &aliases, &[a, b, c]),
            [
                (("eval-apply", a), 0),
                (("eval-apply", alias_a), 0),
                (("eval-only", a), 0),
                (("eval-only", alias_a), 0),
                (("eval-only", b), 1),
                (("eval-only", alias_b), 1),
                (("apply-only", a), 0),
                (("apply-only", alias_a), 0),
                (("apply-only", c), 2),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn event_ids_are_independent_of_map_iteration_order() {
        let mut cheap = empty_sir();
        for id in 0..16 {
            cheap
                .updates
                .events
                .entry(address(id))
                .or_default()
                .set(UpdateEntry::Transition, Vec::new());
        }
        let mut full = empty_sir();
        full.updates.events.reserve(256);
        full.updates.events.extend(cheap.updates.events.clone());
        assert_eq!(cheap.updates.events.len(), full.updates.events.len());
        assert_ne!(
            cheap.updates.events.keys().collect::<Vec<_>>(),
            full.updates.events.keys().collect::<Vec<_>>()
        );
        let order = (0..16).map(address).collect::<Vec<_>>();
        let cheap_ids = event_ids(&cheap, &FxHashMap::default(), &order);
        let full_ids = event_ids(&full, &FxHashMap::default(), &order);
        assert!(cheap_ids.keys().all(|key| full_ids.contains_key(key)));
        assert_eq!(cheap_ids, full_ids);
    }

    #[test]
    fn assigned_ids_match_the_interpreter_including_event_aliases() {
        let program = compile(
            r#"
                module Counter (clk: input clock, q: output logic<8>) {
                    always_ff (clk) { q = q + 1; }
                }
                module Top (clk0: input 'a clock, clk1: input 'b clock,
                            q0: output 'a logic<8>, q1: output 'b logic<8>) {
                    inst counter_a: Counter (clk: clk0, q: q0);
                    inst counter_b: Counter (clk: clk1, q: q1);
                }
            "#,
            OptLevel::O2,
            false,
        );
        assert!(!program.design.events.aliases.is_empty());
        let actual = event_ids(
            &program.sir,
            &program.design.events.aliases,
            &program
                .design
                .events
                .execution_order(program.sir.updates.events.keys().copied()),
        );
        let laid_out = program.into_laid_out_with_mode(false, MemoryLayoutMode::ElementStrided);
        let interpreter = InterpBackend::new(&laid_out, &SimulatorOptions::default()).unwrap();
        let mut expected = EventIds::default();
        for addr in laid_out
            .sir
            .updates
            .groups(UpdateEntry::Transition)
            .map(|(event, _)| event)
            .chain(
                laid_out
                    .sir
                    .updates
                    .groups(UpdateEntry::Sample)
                    .map(|(event, _)| event),
            )
            .chain(
                laid_out
                    .sir
                    .updates
                    .groups(UpdateEntry::Publish)
                    .map(|(event, _)| event),
            )
            .chain(laid_out.design.events.aliases.keys())
        {
            for (group, event) in [
                ("eval-apply", interpreter.resolve_event_opt(addr)),
                ("eval-only", interpreter.resolve_eval_only_event(addr)),
                ("apply-only", interpreter.resolve_apply_event(addr)),
            ] {
                if let Some(event) = event {
                    expected.insert((group, *addr), event.id());
                }
            }
        }
        assert_eq!(actual, expected);
    }
}
