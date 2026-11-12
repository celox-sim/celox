use super::*;

fn default_opts() -> ParsedOptions {
    ParsedOptions {
        common: ParsedOptionsCommon {
            diagnostic_policy: Default::default(),
            four_state: false,
            plusargs: Vec::new(),
            allow_inferred_latches: false,
            optimize_options: celox::OptimizeOptions::all(),
            waveform: None,
            false_loops: vec![],
            true_loops: vec![],
            clock_type: None,
            reset_type: None,
            extra_source: None,
            parameters: vec![],
        },
        cranelift_options: celox::CraneliftOptions::default(),
        dead_store_policy: celox::DeadStorePolicy::Off,
        hotspot_jit: true,
    }
}

fn make_sources(pairs: &[(&str, &str)]) -> Vec<(String, std::path::PathBuf)> {
    pairs
        .iter()
        .map(|(content, path)| (content.to_string(), std::path::PathBuf::from(path)))
        .collect()
}

fn napi_sources(source: &str) -> Vec<NapiSourceFile> {
    vec![NapiSourceFile {
        content: source.to_string(),
        path: "top.veryl".to_string(),
    }]
}

const NO_EVENTS_SOURCE: &str = "module Top () {}";
const TWO_EVENTS_SOURCE: &str = r#"
        module Top (
            clk: input clock,
            rst: input reset,
            en: input logic,
            count: output logic<8>,
        ) {
            var count_r: logic<8>;

            always_ff (clk, rst) {
                if_reset {
                    count_r = 0;
                } else if en {
                    count_r = count_r + 1;
                }
            }

            always_comb {
                count = count_r;
            }
        }
    "#;

#[test]
fn raw_event_methods_reject_eventless_handles_with_the_same_error() {
    let mut simulator =
        NativeSimulatorHandle::new(napi_sources(NO_EVENTS_SOURCE), "Top".into(), None).unwrap();
    let mut simulation =
        NativeSimulationHandle::new(napi_sources(NO_EVENTS_SOURCE), "Top".into(), None).unwrap();

    let errors = [
        simulator.tick(0).unwrap_err(),
        simulator.tick_n(0, 0).unwrap_err(),
        simulation.add_clock(0, 10.0, 0.0).unwrap_err(),
        simulation.schedule(0, 0.0, 1.0).unwrap_err(),
    ];
    let expected = celox::RuntimeErrorCode::NotAnEvent("event_id=0".to_string()).to_string();

    for error in errors {
        assert_eq!(error.reason, expected);
    }
}

#[test]
fn raw_event_methods_reject_the_first_out_of_bounds_id() {
    let mut simulator =
        NativeSimulatorHandle::new(napi_sources(TWO_EVENTS_SOURCE), "Top".into(), None).unwrap();
    let mut simulation =
        NativeSimulationHandle::new(napi_sources(TWO_EVENTS_SOURCE), "Top".into(), None).unwrap();

    let errors = [
        simulator.tick(2).unwrap_err(),
        simulator.tick_n(2, 0).unwrap_err(),
        simulation.add_clock(2, 10.0, 0.0).unwrap_err(),
        simulation.schedule(2, 0.0, 1.0).unwrap_err(),
    ];
    let expected = celox::RuntimeErrorCode::NotAnEvent("event_id=2".to_string()).to_string();

    for error in errors {
        assert_eq!(error.reason, expected);
    }
}

#[test]
fn disposed_errors_take_precedence_over_event_validation() {
    let mut simulator =
        NativeSimulatorHandle::new(napi_sources(NO_EVENTS_SOURCE), "Top".into(), None).unwrap();
    simulator.dispose().unwrap();
    assert_eq!(
        simulator.tick(0).unwrap_err().reason,
        "Simulator has been disposed"
    );
    assert_eq!(
        simulator.tick_n(0, 0).unwrap_err().reason,
        "Simulator has been disposed"
    );

    let mut simulation =
        NativeSimulationHandle::new(napi_sources(NO_EVENTS_SOURCE), "Top".into(), None).unwrap();
    simulation.dispose().unwrap();
    assert_eq!(
        simulation.add_clock(0, 10.0, 0.0).unwrap_err().reason,
        "Simulation has been disposed"
    );
    assert_eq!(
        simulation.schedule(0, 0.0, 1.0).unwrap_err().reason,
        "Simulation has been disposed"
    );
}

#[test]
fn native_handle_dumps_consume_activity_until_memory_is_exposed() {
    // A tick writes `clk` as well as `q`. The wide inputs keep those two
    // trace groups below half of the design, where a dump scans every
    // signal.
    let source = "module Top (
            clk: input clock, q: output logic<64>,
            a: input logic<512>, b: input logic<512>, c: input logic<512>,
            d: input logic<512>, e: input logic<512>,
        ) { always_ff (clk) { q += 1; } }";
    let dir = tempfile::tempdir().unwrap();
    // Warm the uninstrumented cache first. A traced build must not reuse it.
    NativeSimulatorHandle::new(napi_sources(source), "Top".into(), None)
        .unwrap()
        .dispose()
        .unwrap();
    for mode in ["fresh", "cached", "tiered"] {
        let path = dir.path().join(format!("{mode}.vcd"));
        let options = Some(NapiOptions {
            vcd: Some(path.to_str().unwrap().to_owned()),
            fst: None,
            four_state: None,
            plusargs: None,
            allow_inferred_latches: None,
            opt_level: None,
            pass_overrides: None,
            optimize: None,
            optimize_options: None,
            cranelift_opt_level: None,
            regalloc_algorithm: None,
            enable_alias_analysis: None,
            enable_verifier: None,
            false_loops: None,
            true_loops: None,
            clock_type: None,
            reset_type: None,
            extra_source: None,
            parameters: None,
            dead_store_policy: None,
            threads: None,
            hotspot_jit: None,
        });
        let mut handle = if mode == "tiered" {
            NativeSimulatorHandle::new_tiered(napi_sources(source), "Top".into(), options)
        } else {
            NativeSimulatorHandle::new(napi_sources(source), "Top".into(), options)
        }
        .unwrap();
        handle.dump(0.0).unwrap();
        let initial = handle.vcd_writer.as_ref().unwrap().statistics();
        assert!(initial.comparisons > 0);
        handle.dump(1.0).unwrap();
        assert_eq!(
            handle.vcd_writer.as_ref().unwrap().statistics().comparisons,
            initial.comparisons,
            "{mode}: idle dumps must not rescan memory"
        );
        handle.tick(0).unwrap();
        handle.dump(2.0).unwrap();
        let changed = handle.vcd_writer.as_ref().unwrap().statistics();
        assert!(changed.changes > initial.changes);
        assert!(changed.comparisons - initial.comparisons < initial.comparisons);
        handle.dump(3.0).unwrap();
        assert_eq!(
            handle.vcd_writer.as_ref().unwrap().statistics().comparisons,
            changed.comparisons
        );
        // shared_memory() obtains its pointer through this same dispatch.
        handle.backend.as_mut().unwrap().memory_as_mut_ptr();
        handle.dump(4.0).unwrap();
        assert_eq!(
            handle.vcd_writer.as_ref().unwrap().statistics().comparisons,
            changed.comparisons + initial.comparisons
        );
        handle.dispose().unwrap();
        assert!(std::fs::read_to_string(path).unwrap().contains("\nb1 "));
    }
}

#[test]
fn fst_option_records_a_parseable_waveform_and_conflicts_with_vcd() {
    fn options(vcd: Option<String>, fst: Option<String>) -> Option<NapiOptions> {
        Some(NapiOptions {
            vcd,
            fst,
            four_state: None,
            plusargs: None,
            allow_inferred_latches: None,
            opt_level: None,
            pass_overrides: None,
            optimize: None,
            optimize_options: None,
            cranelift_opt_level: None,
            regalloc_algorithm: None,
            enable_alias_analysis: None,
            enable_verifier: None,
            false_loops: None,
            true_loops: None,
            clock_type: None,
            reset_type: None,
            extra_source: None,
            parameters: None,
            dead_store_policy: None,
            threads: None,
            hotspot_jit: None,
        })
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("handle.fst");
    let source =
        "module Top (clk: input clock, q: output logic<8>) { always_ff (clk) { q += 1; } }";
    let mut handle = NativeSimulatorHandle::new(
        napi_sources(source),
        "Top".into(),
        options(None, Some(path.to_str().unwrap().to_owned())),
    )
    .unwrap();
    assert!(matches!(
        handle.vcd_writer,
        Some(celox::WaveformWriter::Fst(_))
    ));
    handle.dump(0.0).unwrap();
    handle.tick(0).unwrap();
    handle.dump(1.0).unwrap();
    handle.dispose().unwrap();

    let wave = wellen::simple::read(&path).unwrap();
    assert_eq!(wave.time_table(), [0, 1]);

    let both = NativeSimulatorHandle::new(
        napi_sources(TWO_EVENTS_SOURCE),
        "Top".into(),
        options(Some("a.vcd".into()), Some("a.fst".into())),
    );
    let Err(error) = both else {
        panic!("vcd + fst must be rejected");
    };
    assert!(
        error.reason.contains("mutually exclusive"),
        "{}",
        error.reason
    );
}

#[test]
fn standard_event_metadata_is_dense_and_valid_ids_still_work() {
    let mut simulator =
        NativeSimulatorHandle::new(napi_sources(TWO_EVENTS_SOURCE), "Top".into(), None).unwrap();
    let simulator_events: HashMap<String, u32> =
        serde_json::from_str(&simulator.events_json()).unwrap();
    let mut simulator_ids = simulator_events.values().copied().collect::<Vec<_>>();
    simulator_ids.sort_unstable();
    assert_eq!(simulator_ids, [0, 1]);
    assert_eq!(
        simulator.backend.as_ref().unwrap().event_count(),
        simulator_ids.len()
    );
    simulator.tick(0).unwrap();
    simulator.tick_n(1, 0).unwrap();

    let mut simulation =
        NativeSimulationHandle::new(napi_sources(TWO_EVENTS_SOURCE), "Top".into(), None).unwrap();
    let simulation_events: HashMap<String, u32> =
        serde_json::from_str(&simulation.events_json()).unwrap();
    let mut simulation_ids = simulation_events.values().copied().collect::<Vec<_>>();
    simulation_ids.sort_unstable();
    assert_eq!(simulation_ids, [0, 1]);
    simulation.add_clock(0, 10.0, 0.0).unwrap();
    simulation.schedule(1, 5.0, 1.0).unwrap();
}

/// `tick` and `tickN` run the domains of clocks derived from the ticked
/// one, on freshly built, cached and tiered handles.
#[test]
fn native_handle_ticks_fire_derived_clock_domains() {
    let source = "module Top (
            clk: input clock, en: input logic,
            half_count: output logic<8>, gated_count: output logic<8>,
        ) {
            var half: logic;
            always_ff (clk) { half = ~half; }
            let half_clk: '_ clock = half;
            always_ff (half_clk) { half_count += 1; }
            let gated: '_ clock = clk & en;
            always_ff (gated) { gated_count += 1; }
        }";
    for mode in ["fresh", "cached", "tiered"] {
        let mut handle = if mode == "tiered" {
            NativeSimulatorHandle::new_tiered(napi_sources(source), "Top".into(), None)
        } else {
            NativeSimulatorHandle::new(napi_sources(source), "Top".into(), None)
        }
        .unwrap();
        let events: HashMap<String, u32> = serde_json::from_str(&handle.events_json()).unwrap();
        let layout: serde_json::Value = serde_json::from_str(&handle.layout_json()).unwrap();
        let offset = |name: &str| layout[name]["offset"].as_u64().unwrap() as usize;
        let (en, half_count, gated_count) =
            (offset("en"), offset("half_count"), offset("gated_count"));
        let memory = |handle: &mut NativeSimulatorHandle| {
            let (ptr, len) = handle.backend.as_mut().unwrap().memory_as_mut_ptr();
            unsafe { std::slice::from_raw_parts_mut(ptr, len) }
        };
        memory(&mut handle)[en] = 1;
        handle.tick(events["clk"]).unwrap();
        handle.tick_n(events["clk"], 3).unwrap();
        assert_eq!(memory(&mut handle)[half_count], 2, "{mode}: half clock");
        assert_eq!(memory(&mut handle)[gated_count], 4, "{mode}: gated clock");
        memory(&mut handle)[en] = 0;
        handle.tick_n(events["clk"], 2).unwrap();
        assert_eq!(memory(&mut handle)[half_count], 3, "{mode}: half clock");
        assert_eq!(memory(&mut handle)[gated_count], 4, "{mode}: closed gate");
        handle.dispose().unwrap();
    }
}

/// `edges` moves the clock and runs the domains of each direction, on
/// freshly built, cached and tiered handles.
#[test]
fn native_handle_edges_run_both_directions() {
    let source = "module Top (
            clk: input clock, rising: output logic<8>, falling: output logic<8>,
        ) {
            always_ff (clk) { rising += 1; }
            let inverted: '_ clock = ~clk;
            always_ff (inverted) { falling += 1; }
        }";
    for mode in ["fresh", "cached", "tiered"] {
        let mut handle = if mode == "tiered" {
            NativeSimulatorHandle::new_tiered(napi_sources(source), "Top".into(), None)
        } else {
            NativeSimulatorHandle::new(napi_sources(source), "Top".into(), None)
        }
        .unwrap();
        let events: HashMap<String, u32> = serde_json::from_str(&handle.events_json()).unwrap();
        let layout: serde_json::Value = serde_json::from_str(&handle.layout_json()).unwrap();
        let offset = |name: &str| layout[name]["offset"].as_u64().unwrap() as usize;
        let (clk, rising, falling) = (offset("clk"), offset("rising"), offset("falling"));
        let memory = |handle: &mut NativeSimulatorHandle| {
            let (ptr, len) = handle.backend.as_mut().unwrap().memory_as_mut_ptr();
            unsafe { std::slice::from_raw_parts_mut(ptr, len) }
        };
        let clk_event = events["clk"];
        assert!(handle.active_edge_is_rising(clk_event).unwrap());
        let _ = handle.edges(vec![clk_event], vec![false], None).unwrap();
        let start = (memory(&mut handle)[rising], memory(&mut handle)[falling]);
        assert!(handle.edges(vec![clk_event], vec![true], None).unwrap());
        assert_eq!(memory(&mut handle)[clk], 1, "{mode}: clk stays high");
        assert_eq!(
            memory(&mut handle)[rising],
            start.0.wrapping_add(1),
            "{mode}"
        );
        assert_eq!(memory(&mut handle)[falling], start.1, "{mode}");
        let _ = handle.edges(vec![clk_event], vec![false], None).unwrap();
        assert_eq!(
            memory(&mut handle)[rising],
            start.0.wrapping_add(1),
            "{mode}"
        );
        assert_eq!(
            memory(&mut handle)[falling],
            start.1.wrapping_add(1),
            "{mode}"
        );
        assert!(handle.edges(vec![clk_event], vec![], None).is_err());
        handle.dispose().unwrap();
    }
}

#[test]
fn native_handle_edges_preserve_settledness_for_unused_clocks_and_wiring() {
    for (seen, settled, value) in [
        ("1'b1", true, Some(1)),
        ("clk", true, Some(0)),
        ("~clk", false, None),
    ] {
        let source = format!(
            "module Top (clk: input clock, count: output logic<8>, seen: output logic) {{
                    always_ff (clk) {{ count += 1; }}
                    assign seen = {seen};
                }}"
        );
        let mut handle =
            NativeSimulatorHandle::new(napi_sources(&source), "Top".into(), None).unwrap();
        let events: HashMap<String, u32> = serde_json::from_str(&handle.events_json()).unwrap();
        let clk = events["clk"];
        assert!(handle.edges(vec![clk], vec![true], None).unwrap());
        // Unused clocks and direct wiring remain settled after an
        // inactive edge. Other clock-dependent logic still needs a settle.
        assert_eq!(
            handle.edges(vec![clk], vec![false], Some(false)).unwrap(),
            settled,
            "{seen}"
        );
        if let Some(value) = value {
            let layout: serde_json::Value = serde_json::from_str(&handle.layout_json()).unwrap();
            let offset = layout["seen"]["offset"].as_u64().unwrap() as usize;
            let (memory, len) = handle.backend.as_mut().unwrap().memory_as_mut_ptr();
            assert!(offset < len);
            // Read the stored result without implicitly evaluating comb.
            assert_eq!(unsafe { *memory.add(offset) }, value, "{seen}");
        }
        assert!(handle.edges(vec![clk], vec![true], Some(true)).unwrap());
        assert!(!handle.edges(vec![clk], vec![false], Some(true)).unwrap());
        handle.dispose().unwrap();
    }
}

#[test]
fn simulator_handle_derives_four_state_metadata_from_layout() {
    let source = "module Top (a: input logic<1>) {}";
    let path = std::path::Path::new("top.veryl");

    for four_state in [false, true] {
        let simulator = celox::Simulator::from_sources(vec![(source, path)], "Top")
            .four_state(four_state)
            .build()
            .unwrap();
        let handle = NativeSimulatorHandle::from_simulator(simulator, None).unwrap();
        let layout: serde_json::Value = serde_json::from_str(&handle.layout_json()).unwrap();

        assert_eq!(layout["a"]["is_4state"].as_bool(), Some(four_state));
    }
}

#[test]
fn project_diagnostic_policy_changes_invalidate_cached_compilation() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = \"diagnostic_policy\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let source = r#"module Top (tmp: output logic<8>, out: output logic) {
            function f(a: output logic<8>, b: output logic<8>) -> logic {
                a = 1; b = 2; return 1'b1;
            }
            always_comb { tmp = 0; out = f(b: tmp, a: tmp) & 1'b1; }
        }"#;
    std::fs::write(project.path().join("src/top.veryl"), source).unwrap();
    let path = project.path().to_str().unwrap().to_string();
    let config = project.path().join("celox.toml");
    for level in ["allow", "warn", "error"] {
        std::fs::write(
            &config,
            format!("[diagnostics]\nunspecified_output_copy_order = '{level}'\n"),
        )
        .unwrap();
        let result = NativeSimulatorHandle::from_project(path.clone(), "Top".into(), None);
        if level == "error" {
            assert!(
                result
                    .err()
                    .unwrap()
                    .reason
                    .contains("unspecified_output_copy_order")
            );
        } else {
            let handle = result.unwrap();
            let warnings: Vec<String> = serde_json::from_str(&handle.warnings_json()).unwrap();
            assert_eq!(warnings.len(), usize::from(level == "warn"));
        }
    }
}

#[test]
fn normal_project_loading_excludes_example_sources() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("src")).unwrap();
    std::fs::create_dir(project.path().join("examples")).unwrap();
    std::fs::write(
        project.path().join("Veryl.toml"),
        "[project]\nname = \"example_filter\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(project.path().join("src/top.veryl"), "module Top () {}\n").unwrap();
    std::fs::write(
        project.path().join("examples/demo.veryl"),
        "module Demo () {}\n",
    )
    .unwrap();

    let (sources, _, _) = load_project_sources(project.path().to_str().unwrap()).unwrap();
    let source_names = sources
        .iter()
        .map(|(_, path)| path.file_name().unwrap().to_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(source_names, ["top.veryl"]);
}

#[test]
fn same_inputs_produce_same_key() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let opts = default_opts();
    let k1 = build_cache_key(&src, "Top", &opts, None);
    let k2 = build_cache_key(&src, "Top", &opts, None);
    assert_eq!(k1, k2);
}

#[test]
fn high_index_sir_pass_changes_cache_key_without_bit_packing() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let enabled = default_opts();
    let mut disabled = default_opts();
    disabled.common.optimize_options = disabled
        .common
        .optimize_options
        .clone()
        .disable(celox::SirPass::IdentityStoreBypass);

    assert_ne!(
        build_cache_key(&src, "Top", &enabled, None),
        build_cache_key(&src, "Top", &disabled, None)
    );
}

#[test]
fn native_memory_width_changes_cache_key() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut narrow = default_opts();
    narrow.common.optimize_options = narrow
        .common
        .optimize_options
        .clone()
        .with_max_native_memory_width(64);
    let mut wide = default_opts();
    wide.common.optimize_options = wide
        .common
        .optimize_options
        .clone()
        .with_max_native_memory_width(128);

    assert_ne!(
        build_cache_key(&src, "Top", &narrow, None),
        build_cache_key(&src, "Top", &wide, None)
    );
}

#[test]
fn different_source_content_different_key() {
    let s1 = make_sources(&[("module A {}", "a.veryl")]);
    let s2 = make_sources(&[("module B {}", "a.veryl")]);
    let opts = default_opts();
    assert_ne!(
        build_cache_key(&s1, "Top", &opts, None),
        build_cache_key(&s2, "Top", &opts, None),
    );
}

#[test]
fn different_source_path_different_key() {
    let s1 = make_sources(&[("module A {}", "a.veryl")]);
    let s2 = make_sources(&[("module A {}", "b.veryl")]);
    let opts = default_opts();
    assert_ne!(
        build_cache_key(&s1, "Top", &opts, None),
        build_cache_key(&s2, "Top", &opts, None),
    );
}

#[test]
fn different_top_different_key() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let opts = default_opts();
    assert_ne!(
        build_cache_key(&src, "Top", &opts, None),
        build_cache_key(&src, "Other", &opts, None),
    );
}

#[test]
fn four_state_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.four_state = false;
    o2.common.four_state = true;
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn inferred_latch_policy_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let strict = default_opts();
    let mut permissive = default_opts();
    permissive.common.allow_inferred_latches = true;
    assert_ne!(
        build_cache_key(&src, "Top", &strict, None),
        build_cache_key(&src, "Top", &permissive, None),
    );
}

#[test]
fn inferred_latch_cache_key_uses_effective_explicit_policy() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    for level in [
        celox::DiagnosticLevel::Error,
        celox::DiagnosticLevel::Warn,
        celox::DiagnosticLevel::Allow,
    ] {
        let mut explicit = default_opts();
        explicit
            .common
            .diagnostic_policy
            .set("inferred_latch", level);
        let mut compatibility = default_opts();
        compatibility
            .common
            .diagnostic_policy
            .set("inferred_latch", level);
        compatibility.common.allow_inferred_latches = true;
        assert_eq!(
            build_cache_key(&src, "Top", &explicit, None),
            build_cache_key(&src, "Top", &compatibility, None),
        );
    }
}

#[test]
fn optimize_options_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.optimize_options = celox::OptimizeOptions::all();
    o2.common.optimize_options = celox::OptimizeOptions::none();
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn cranelift_opt_level_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.cranelift_options.opt_level = celox::CraneliftOptLevel::Speed;
    o2.cranelift_options.opt_level = celox::CraneliftOptLevel::None;
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn regalloc_algorithm_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.cranelift_options.regalloc_algorithm = celox::RegallocAlgorithm::Backtracking;
    o2.cranelift_options.regalloc_algorithm = celox::RegallocAlgorithm::SinglePass;
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn dead_store_policy_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.dead_store_policy = celox::DeadStorePolicy::Off;
    o2.dead_store_policy = celox::DeadStorePolicy::PreserveTopPorts;
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn clock_type_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.clock_type = None;
    o2.common.clock_type = Some(celox::ClockType::NegEdge);
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn reset_type_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.reset_type = None;
    o2.common.reset_type = Some(celox::ResetType::SyncHigh);
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn parameters_differ() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.parameters = vec![("WIDTH".into(), 8)];
    o2.common.parameters = vec![("WIDTH".into(), 16)];
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn source_order_independent() {
    let s1 = make_sources(&[("aaa", "a.veryl"), ("bbb", "b.veryl")]);
    let s2 = make_sources(&[("bbb", "b.veryl"), ("aaa", "a.veryl")]);
    let opts = default_opts();
    assert_eq!(
        build_cache_key(&s1, "Top", &opts, None),
        build_cache_key(&s2, "Top", &opts, None),
    );
}

#[test]
fn cached_waveforms_retain_explicit_time_precision() {
    let dir = tempfile::tempdir().unwrap();
    let source = "module CachedPrecision(y: output logic) {}";
    let sources = make_sources(&[(source, "cached_precision.veryl")]);
    let key = build_cache_key(&sources, "CachedPrecision", &default_opts(), None);
    let sim = celox::Simulator::builder(source, "CachedPrecision")
        .vcd(dir.path().join("initial.vcd"))
        .build()
        .unwrap();
    let handle = NativeSimulatorHandle::build_and_cache(sim, None, Some(key.clone())).unwrap();
    drop(handle);
    let mut cached = JIT_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key)
        .unwrap();
    // A frontend with a non-default scale supplies this cached metadata.
    Arc::get_mut(&mut cached).unwrap().time_precision =
        celox_design::TimePrecision::try_from(-15).unwrap();
    let path = dir.path().join("cached.vcd");
    let mut handle = NativeSimulatorHandle::from_cached(
        &cached,
        path.to_str().map(|p| (p, celox::WaveformFormat::Vcd)),
    )
    .unwrap();
    handle.dump(123.0).unwrap();
    handle.vcd_writer.as_mut().unwrap().flush().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("$timescale 1fs $end"), "{text}");
    assert!(text.contains("#123\n"));
}

#[test]
fn vcd_paths_share_compiled_code_but_tracing_modes_do_not() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    // VCD instrumentation affects compilation, but its destination does not.
    o1.common.waveform = None;
    o2.common.waveform = Some(("/tmp/dump.vcd".into(), celox::WaveformFormat::Vcd));
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
    o1.common.waveform = Some(("/tmp/another.vcd".into(), celox::WaveformFormat::Vcd));
    assert_eq!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
    // FST needs the same traced layout as VCD, so format is not keyed either.
    o1.common.waveform = Some(("/tmp/dump.fst".into(), celox::WaveformFormat::Fst));
    assert_eq!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn false_loops_differ() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.false_loops = vec![];
    o2.common.false_loops = vec![((vec![], vec!["a".into()]), (vec![], vec!["b".into()]))];
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn true_loops_differ() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let mut o1 = default_opts();
    let mut o2 = default_opts();
    o1.common.true_loops = vec![];
    o2.common.true_loops = vec![((vec![], vec!["x".into()]), (vec![], vec!["y".into()]), 4)];
    assert_ne!(
        build_cache_key(&src, "Top", &o1, None),
        build_cache_key(&src, "Top", &o2, None),
    );
}

#[test]
fn metadata_clock_reset_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let opts = default_opts();

    let mut m1 = Metadata::create_default("prj").unwrap();
    let mut m2 = Metadata::create_default("prj").unwrap();
    m1.build.clock_type = celox::ClockType::PosEdge;
    m2.build.clock_type = celox::ClockType::NegEdge;
    assert_ne!(
        build_cache_key(&src, "Top", &opts, Some(&m1)),
        build_cache_key(&src, "Top", &opts, Some(&m2)),
    );

    let mut m3 = Metadata::create_default("prj").unwrap();
    let mut m4 = Metadata::create_default("prj").unwrap();
    m3.build.reset_type = celox::ResetType::AsyncLow;
    m4.build.reset_type = celox::ResetType::SyncHigh;
    assert_ne!(
        build_cache_key(&src, "Top", &opts, Some(&m3)),
        build_cache_key(&src, "Top", &opts, Some(&m4)),
    );
}

#[test]
fn no_metadata_vs_metadata_differs() {
    let src = make_sources(&[("module Top {}", "a.veryl")]);
    let opts = default_opts();
    let m = Metadata::create_default("prj").unwrap();
    // No metadata vs with metadata should differ (metadata adds clock/reset info)
    assert_ne!(
        build_cache_key(&src, "Top", &opts, None),
        build_cache_key(&src, "Top", &opts, Some(&m)),
    );
}

#[test]
fn initial_memory_image_reads_readmem_x_and_z_as_zero_in_two_state() {
    let dir = tempfile::tempdir().unwrap();
    let mem_path = dir.path().join("xz.mem");
    std::fs::write(&mem_path, "aa\n@2\nx5\nz3\n").unwrap();
    let source = format!(
        r#"
        module Top (
            out2: output logic<8>,
        ) {{
            #[allow(initial_assign)]
            var mem: logic<8>[4];
            initial {{
                $readmemh("{}", mem);
            }}
            assign out2 = mem[2];
        }}
        "#,
        mem_path.to_string_lossy().replace('\\', "\\\\")
    );
    // (four_state, value bytes of mem[0], mem[2], mem[3], mask byte of mem[2])
    for (four_state, values, mask2) in [
        // Two-state storage holds no X or Z: their bits are zero
        // (IEEE 1800-2023 6.11.2, 21.4.2).
        (false, [0xaau8, 0x05, 0x03], None),
        // X is `(1, 1)` and Z is `(0, 1)`.
        (true, [0xaau8, 0xf5, 0x03], Some(0xf0u8)),
    ] {
        let (sir, _) = celox::compile_to_sir(
            &[(source.as_str(), std::path::Path::new("top.veryl"))],
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
            &celox::OptimizeOptions::all(),
        )
        .unwrap();
        // The layout mode of the WASM handle.
        let program =
            sir.into_laid_out_with_mode(four_state, celox::MemoryLayoutMode::ElementStrided);
        let layout = program.layout();
        let memory = crate::initial_memory::initial_memory_bytes(&program, layout, four_state);
        let initial = program
            .design
            .initial_state
            .iter()
            .find(|initial| matches!(initial.data, celox_design::InitialStateData::MemoryFile(_)))
            .expect("$readmemh loads mem as the image is built");
        let offset = layout.offsets[&initial.address];
        let element_byte = |element: usize| {
            let (byte, intra) = layout.map_static_bit_offset(&initial.address, element * 8);
            assert_eq!(intra, 0);
            offset + byte
        };
        for (element, value) in [0usize, 2, 3].into_iter().zip(values) {
            assert_eq!(
                memory[element_byte(element)],
                value,
                "four_state={four_state} mem[{element}]"
            );
        }
        if let Some(mask2) = mask2 {
            let plane_size = layout.plane_size(&initial.address);
            assert_eq!(memory[element_byte(2) + plane_size], mask2);
        }
    }
}
