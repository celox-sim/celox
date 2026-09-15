//! Stage 0 spike: compare the physical layout + event space produced from
//! cheap (O0) SIR against full (O2) SIR on the same design.
//!
//! The tiered fast-start plan needs the Tier-0 interpreter (built on cheap
//! SIR) to share the memory image layout and trigger-ID space with the
//! background full-pipeline compilation. If the layouts agree, promotion can
//! reuse the existing `tier_transfer` whole-image handoff.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use celox::{MemoryLayout, MemoryLayoutMode, OptLevel, OptimizeOptions, compile_to_sir};
use veryl_metadata::Metadata;

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
    let mut check = |name: &str, same: bool, detail: String| {
        println!("{} {} {}", if same { "SAME " } else { "DIFFER" }, name, detail);
        if !same {
            failures += 1;
        }
    };

    check(
        "stable-offsets",
        cheap.0.offsets == full.0.offsets,
        format!("cheap={} full={}", cheap.0.offsets.len(), full.0.offsets.len()),
    );
    check(
        "stable-widths",
        cheap.0.widths == full.0.widths,
        format!("cheap={} full={}", cheap.0.widths.len(), full.0.widths.len()),
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
        let cheap_ids: BTreeSet<_> = cheap.1.iter().collect();
        let full_ids: BTreeSet<_> = full.1.iter().collect();
        for id in cheap_ids.symmetric_difference(&full_ids).take(10) {
            println!("event-only-on-one-side {:?}", id);
        }
    }
    println!("layout-spike {}", if failures == 0 { "AGREE" } else { "DISAGREE" });
    std::process::exit(if failures == 0 { 0 } else { 1 });
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

type LayoutAndEvents = (MemoryLayout, Vec<(String, usize)>);

fn build(
    sources: &[(&str, &std::path::Path)],
    test: &str,
    metadata: &Metadata,
    level: OptLevel,
    four_state: bool,
) -> LayoutAndEvents {
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
    let layout = MemoryLayout::build(&program, four_state, MemoryLayoutMode::Packed);
    // Event ID space: mirror InterpBackend::new — ids are assigned by
    // first-seen order over eval_apply_ffs, eval_only_ffs, apply_ffs keys,
    // with aliases resolved to their canonical entry afterwards. This is
    // the space TieredEventRef ids live in, so both tiers must agree here.
    use std::collections::HashMap as StdHashMap;
    let addr_to_id: StdHashMap<String, usize> = StdHashMap::new();
    let next_id = 0usize;
    let intern = |_addr: &celox::AbsoluteAddr| -> usize { next_id };
    // NOTE: production interns ids by FxHashMap iteration order over the
    // three schedule-group key sets (deterministic for a fixed hasher and
    // identical maps, but an internal detail). Here we compare the
    // *address membership* per group, which is what promotion needs:
    // same events visible to each schedule group in both pipelines.
    let mut events: Vec<(String, usize)> = Vec::new();
    for (group, units) in [
        ("apply", &program.sir.eval_apply_ffs),
        ("eval-only", &program.sir.eval_only_ffs),
        ("apply-only", &program.sir.apply_ffs),
    ] {
        let mut keys: Vec<_> = units.keys().collect();
        keys.sort_by_key(|addr| format!("{:?}", addr));
        for addr in keys {
            events.push((format!("{}:{:?}", group, addr), 0));
        }
    }
    // Include the exact program units an EU-group scan would see, so a pass
    // that merges/splits schedule groups shows up here.
    events.sort();
    events.dedup();
    let _ = (next_id, addr_to_id, intern);
    (layout, events)
}
