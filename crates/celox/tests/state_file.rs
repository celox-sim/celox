use celox::{
    BigUint, OptLevel, RuntimeEvent, Simulation, Simulator, StateDifference, StateError, StateFile,
    StateRole,
};

/// Registers, a memory written through a run-time index, combinational
/// outputs, and an internal net nothing reads (removed by dead store
/// elimination at O2).
const DESIGN: &str = r#"
module Top (clk: input clock, din: input logic<8>, cnt: output logic<8>, acc: output logic<16>, mo: output logic<8>, comb: output logic<8>) {
    var mem: logic<8> [4];
    var c: logic<8>;
    var a: logic<16>;
    var dead: logic<8>;
    always_ff (clk) {
        c = c + 8'd1;
        a = a + {8'h00, din} + {8'h00, c};
        mem[c[1:0]] = c ^ din;
    }
    assign dead = c * 8'd3;
    assign cnt = c;
    assign acc = a;
    assign mo = mem[c[1:0]];
    assign comb = c + din;
}
"#;

const OUTPUTS: [&str; 4] = ["cnt", "acc", "mo", "comb"];

fn stimulus(cycle: u32) -> u8 {
    (cycle.wrapping_mul(37) ^ 0x5a) as u8
}

macro_rules! run_cycles {
    ($sim:expr, $cycles:expr) => {{
        let clk = $sim.event("clk");
        let din = $sim.signal("din");
        let outputs = OUTPUTS.map(|name| $sim.signal(name));
        let mut trace = Vec::new();
        for cycle in $cycles {
            $sim.modify(|io| io.set(din, stimulus(cycle))).unwrap();
            $sim.tick(clk).unwrap();
            trace.push(
                outputs
                    .iter()
                    .map(|&signal| $sim.get(signal))
                    .collect::<Vec<BigUint>>(),
            );
        }
        trace
    }};
}

fn through_bytes(file: &StateFile) -> StateFile {
    let mut bytes = Vec::new();
    file.write_to(&mut bytes).unwrap();
    StateFile::read_from(bytes.as_slice()).unwrap()
}

fn object<'a>(file: &'a StateFile, path: &str) -> &'a celox::StateObject {
    file.objects
        .iter()
        .find(|object| object.path == path)
        .unwrap_or_else(|| panic!("`{path}` is not in the state file"))
}

#[test]
fn classifies_state_and_combinational_objects() {
    let mut sim = Simulator::builder(DESIGN, "Top").build().unwrap();
    let file = sim.save_state().unwrap();
    for path in ["c", "a", "mem", "din"] {
        assert_eq!(object(&file, path).role, StateRole::State, "{path}");
    }
    for path in ["cnt", "comb", "dead"] {
        assert_eq!(object(&file, path).role, StateRole::Comb, "{path}");
    }
}

#[test]
fn loads_into_a_fresh_simulator() {
    let mut source = Simulator::builder(DESIGN, "Top").build().unwrap();
    run_cycles!(source, 0..7);
    let file = through_bytes(&source.save_state().unwrap());
    let mut target = Simulator::builder(DESIGN, "Top").build().unwrap();
    target.load_state(&file).unwrap();
    assert_eq!(run_cycles!(target, 7..30), run_cycles!(source, 7..30));
}

macro_rules! cross_load {
    ($name:ident, $source:expr, $target:expr) => {
        #[test]
        fn $name() {
            let mut source = $source;
            run_cycles!(source, 0..7);
            let file = through_bytes(&source.save_state().unwrap());
            let mut target = $target;
            target.load_state(&file).unwrap();
            assert_eq!(run_cycles!(target, 7..30), run_cycles!(source, 7..30));
        }
    };
}

cross_load!(
    native_o2_into_interpreter_o0,
    Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O2)
        .build()
        .unwrap(),
    Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O0)
        .build_interpreter()
        .unwrap()
);
cross_load!(
    interpreter_o0_into_native_o2,
    Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O0)
        .build_interpreter()
        .unwrap(),
    Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O2)
        .build()
        .unwrap()
);
cross_load!(
    cranelift_into_wasm,
    Simulator::builder(DESIGN, "Top").build_cranelift().unwrap(),
    Simulator::builder(DESIGN, "Top").build_wasm().unwrap()
);

#[test]
fn diff_hides_values_left_stale_by_dead_store_elimination() {
    let mut optimized = Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O2)
        .build()
        .unwrap();
    let mut plain = Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O0)
        .build()
        .unwrap();
    run_cycles!(optimized, 0..7);
    run_cycles!(plain, 0..7);
    let optimized = optimized.save_state().unwrap();
    let plain = plain.save_state().unwrap();
    assert_eq!(optimized.diff(&plain, false), []);
    // `dead` is never written at O2, so its saved value is stale there.
    let stale: Vec<_> = optimized
        .diff(&plain, true)
        .into_iter()
        .filter_map(|difference| match difference {
            StateDifference::Changed { path, .. } => Some(path),
            _ => None,
        })
        .collect();
    assert_eq!(stale, ["dead"]);

    let mut diverged = Simulator::builder(DESIGN, "Top").build().unwrap();
    run_cycles!(diverged, 0..8);
    let paths: Vec<_> = plain
        .diff(&diverged.save_state().unwrap(), false)
        .into_iter()
        .map(|difference| match difference {
            StateDifference::Changed { path, .. } => path,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert!(paths.contains(&"c".to_string()), "{paths:?}");
}

#[test]
fn mismatched_design_is_rejected_without_changes() {
    let mut source = Simulator::builder(DESIGN, "Top").build().unwrap();
    let file = source.save_state().unwrap();
    let other_design = r#"
        module Top (clk: input clock, din: input logic<8>, q: output logic<4>) {
            var c: logic<4>;
            always_ff (clk) { c = c + din[3:0]; }
            assign q = c;
        }
    "#;
    let mut other = Simulator::builder(other_design, "Top").build().unwrap();
    let (clk, din, q) = (other.event("clk"), other.signal("din"), other.signal("q"));
    other.modify(|io| io.set(din, 3u8)).unwrap();
    other.tick(clk).unwrap();
    let StateError::Mismatch(mismatch) = other.load_state(&file).unwrap_err() else {
        panic!("expected a mismatch");
    };
    assert_eq!(mismatch.width_mismatches, [("c".to_string(), 8, 4)]);
    assert!(mismatch.missing_in_file.is_empty());
    assert!(mismatch.missing_in_design.contains(&"a".to_string()));
    assert!(mismatch.missing_in_design.contains(&"mem".to_string()));
    assert_eq!(other.get(q), 3u8.into());
}

#[test]
fn unknown_values_survive_a_four_state_round_trip() {
    let design = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
        }
    "#;
    let mut source = Simulator::builder(design, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let (clk, d, q) = (source.event("clk"), source.signal("d"), source.signal("q"));
    source
        .modify(|io| io.set_four_state(d, 0x30u8.into(), 0x0fu8.into()))
        .unwrap();
    source.tick(clk).unwrap();
    let file = through_bytes(&source.save_state().unwrap());

    let mut four_state = Simulator::builder(design, "Top")
        .four_state(true)
        .build()
        .unwrap();
    four_state.load_state(&file).unwrap();
    let saved = source.get_four_state(q);
    assert_eq!(saved, (0x30u8.into(), 0x0fu8.into()));
    let q = four_state.signal("q");
    assert_eq!(four_state.get_four_state(q), saved);

    // A two-state simulation reads the unknown bits as zero.
    let mut two_state = Simulator::builder(design, "Top").build().unwrap();
    two_state.load_state(&file).unwrap();
    let q = two_state.signal("q");
    assert_eq!(two_state.get(q), 0x30u8.into());
}

#[test]
fn loading_does_not_report_combinational_display_changes() {
    let design = r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
            always_comb { $display("q=%0d", q); }
        }
    "#;
    let mut source = Simulator::builder(design, "Top").build().unwrap();
    let (clk, d) = (source.event("clk"), source.signal("d"));
    source.modify(|io| io.set(d, 7u8)).unwrap();
    source.tick(clk).unwrap();
    let file = source.save_state().unwrap();

    let mut target = Simulator::builder(design, "Top").build().unwrap();
    target.drain_runtime_events();
    target.load_state(&file).unwrap();
    assert!(target.drain_runtime_events().is_empty());
    let (clk, d) = (target.event("clk"), target.signal("d"));
    target.modify(|io| io.set(d, 9u8)).unwrap();
    target.tick(clk).unwrap();
    assert_eq!(
        target.drain_runtime_events(),
        [RuntimeEvent::Display {
            message: "q=9".into()
        }]
    );
}

fn timed_trace(sim: &mut Simulation, until: u64) -> Vec<(u64, Vec<BigUint>)> {
    let din = sim.signal("din");
    let outputs = OUTPUTS.map(|name| sim.signal(name));
    let mut trace = Vec::new();
    while let Some(time) = sim.next_event_time() {
        if time > until {
            break;
        }
        sim.modify(|io| io.set(din, stimulus(time as u32))).unwrap();
        sim.step().unwrap();
        trace.push((
            sim.time(),
            outputs.iter().map(|&signal| sim.get(signal)).collect(),
        ));
    }
    trace
}

#[test]
fn simulation_state_includes_time_clocks_and_events() {
    let mut source = Simulation::builder(DESIGN, "Top").build().unwrap();
    source.add_clock("clk", 10, 5);
    timed_trace(&mut source, 100);
    let file = through_bytes(&source.save_state().unwrap());

    // The target registers no clock of its own: the file supplies it.
    let mut target = Simulation::builder(DESIGN, "Top")
        .opt_level(OptLevel::O0)
        .build()
        .unwrap();
    target.load_state(&file).unwrap();
    assert_eq!(target.time(), source.time());
    assert_eq!(timed_trace(&mut target, 400), timed_trace(&mut source, 400));
}

#[test]
fn simulation_requires_a_schedule() {
    let mut simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let file = simulator.save_state().unwrap();
    let mut simulation = Simulation::builder(DESIGN, "Top").build().unwrap();
    assert!(matches!(
        simulation.load_state(&file),
        Err(StateError::MissingSchedule)
    ));
}

#[test]
fn loading_into_a_vcd_simulator_records_the_loaded_values() {
    let design = r#"
        module Top (clk: input clock, q: output logic<8>) {
            always_ff (clk) { q = q + 8'd1; }
        }
    "#;
    let mut source = Simulator::builder(design, "Top").build().unwrap();
    let clk = source.event("clk");
    for _ in 0..6 {
        source.tick(clk).unwrap();
    }
    let file = source.save_state().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wave.vcd");
    let mut target = Simulator::builder(design, "Top")
        .vcd(&path)
        .build()
        .unwrap();
    target.dump(0);
    target.load_state(&file).unwrap();
    target.dump(1);
    target.flush_vcd().unwrap();
    let vcd = std::fs::read_to_string(&path).unwrap();
    let after_load = vcd.split("#1\n").nth(1).unwrap();
    assert!(
        after_load.lines().any(|line| line.starts_with("b110 ")),
        "{vcd}"
    );
}

#[test]
fn simulation_schedule_names_clocks_by_their_own_path() {
    // `core.clk` sorts before `sys_clk` and may share its storage when
    // identity aliases are merged; the saved clock must still be `sys_clk`.
    let design = r#"
        module Child (clk: input clock, q: output logic<8>) {
            always_ff (clk) { q = q + 8'd1; }
        }
        module Top (sys_clk: input clock, q: output logic<8>) {
            inst core: Child (clk: sys_clk, q);
        }
    "#;
    let trace = |sim: &mut Simulation| {
        let q = sim.signal("q");
        (0..10)
            .map(|_| {
                sim.step().unwrap();
                (sim.time(), sim.get(q))
            })
            .collect::<Vec<_>>()
    };
    let mut source = Simulation::builder(design, "Top")
        .opt_level(OptLevel::O2)
        .build()
        .unwrap();
    source.add_clock("sys_clk", 10, 5);
    trace(&mut source);
    let file = through_bytes(&source.save_state().unwrap());
    let schedule = file.schedule.as_ref().unwrap();
    assert!(
        schedule
            .events
            .iter()
            .all(|event| event.signal == "sys_clk"),
        "{schedule:?}"
    );

    let mut target = Simulation::builder(design, "Top")
        .opt_level(OptLevel::O0)
        .build()
        .unwrap();
    target.load_state(&file).unwrap();
    assert_eq!(trace(&mut target), trace(&mut source));
}

#[test]
fn simulation_load_refuses_to_rewind_vcd() {
    let mut source = Simulation::builder(DESIGN, "Top").build().unwrap();
    source.add_clock("clk", 10, 5);
    source.run_until(30).unwrap();
    let file = source.save_state().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mut target = Simulation::builder(DESIGN, "Top")
        .vcd(dir.path().join("wave.vcd"))
        .build()
        .unwrap();
    target.add_clock("clk", 10, 5);
    target.run_until(100).unwrap();
    assert!(matches!(
        target.load_state(&file),
        Err(StateError::Checkpoint(
            celox::CheckpointError::VcdRewind { .. }
        ))
    ));
    target.switch_vcd(dir.path().join("loaded.vcd")).unwrap();
    target.load_state(&file).unwrap();
    assert_eq!(target.time(), 30);
}
