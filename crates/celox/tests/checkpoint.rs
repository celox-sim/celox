use celox::SimBackend as _;
use celox::{
    BigUint, CheckpointError, OptLevel, RuntimeEvent, Simulation, Simulator, TierPromotion,
};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

/// Registers, a memory written through a run-time index, and combinational
/// outputs that depend on both.
const DESIGN: &str = r#"
module Top (clk: input clock, din: input logic<8>, cnt: output logic<8>, acc: output logic<16>, mo: output logic<8>, comb: output logic<8>) {
    var mem: logic<8> [4];
    var c: logic<8>;
    var a: logic<16>;
    always_ff (clk) {
        c = c + 8'd1;
        a = a + {8'h00, din} + {8'h00, c};
        mem[c[1:0]] = c ^ din;
    }
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

/// Drive `cycles` clock cycles and record the outputs after each one.
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

macro_rules! outputs_now {
    ($sim:expr) => {
        OUTPUTS
            .map(|name| $sim.signal(name))
            .iter()
            .map(|&signal| $sim.get(signal))
            .collect::<Vec<BigUint>>()
    };
}

all_backends! {
    fn restore_replays_the_same_cycles(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @build Simulator::builder(DESIGN, "Top");
        run_cycles!(sim, 0..5);
        let checkpoint = sim.checkpoint().unwrap();
        let at_checkpoint = outputs_now!(sim);
        let expected = run_cycles!(sim, 5..40);
        for _ in 0..2 {
            sim.restore(&checkpoint).unwrap();
            assert_eq!(outputs_now!(sim), at_checkpoint);
            assert_eq!(run_cycles!(sim, 5..40), expected);
        }
    }

    fn restore_keeps_four_state_values(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @build Simulator::builder(
            r#"
            module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
                always_ff (clk) { q = d; }
            }
            "#,
            "Top",
        )
        .four_state(true);
        let (clk, d, q) = (sim.event("clk"), sim.signal("d"), sim.signal("q"));
        sim.modify(|io| io.set_four_state(d, 0x30u8.into(), 0x0fu8.into())).unwrap();
        sim.tick(clk).unwrap();
        let saved = sim.get_four_state(q);
        let checkpoint = sim.checkpoint().unwrap();
        sim.modify(|io| io.set(d, 0x55u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_ne!(sim.get_four_state(q), saved);
        sim.restore(&checkpoint).unwrap();
        assert_eq!(sim.get_four_state(q), saved);
    }

    fn restore_replays_runtime_events(sim) {
        @omit_veryl;
        @ignore_on(sv);
        @build Simulator::builder(
            r#"
            module Top (clk: input clock, a: input logic<8>, q: output logic<8>) {
                always_ff (clk) {
                    q = a;
                    $display("a=%0d", a);
                }
            }
            "#,
            "Top",
        );
        let (clk, a) = (sim.event("clk"), sim.signal("a"));
        let checkpoint = sim.checkpoint().unwrap();
        sim.modify(|io| io.set(a, 7u8)).unwrap();
        sim.tick(clk).unwrap();
        let first = sim.drain_runtime_events();
        assert_eq!(first, vec![RuntimeEvent::Display { message: "a=7".into() }]);
        sim.restore(&checkpoint).unwrap();
        // Events already emitted are not withdrawn or repeated by a restore;
        // re-executing the cycle emits them again.
        assert!(sim.drain_runtime_events().is_empty());
        sim.modify(|io| io.set(a, 7u8)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.drain_runtime_events(), first);
    }
}

#[test]
fn restore_with_dead_store_elimination() {
    let mut sim = Simulator::builder(DESIGN, "Top")
        .opt_level(OptLevel::O2)
        .build()
        .unwrap();
    run_cycles!(sim, 0..5);
    let checkpoint = sim.checkpoint().unwrap();
    let expected = run_cycles!(sim, 5..40);
    sim.restore(&checkpoint).unwrap();
    assert_eq!(run_cycles!(sim, 5..40), expected);
}

#[test]
fn tiered_checkpoint_survives_promotion() {
    let mut reference = Simulator::builder(DESIGN, "Top").build().unwrap();
    let mut sim = Simulator::builder(DESIGN, "Top")
        .tier_promotion(TierPromotion::AfterSteps(200))
        .build_tiered()
        .unwrap();
    run_cycles!(sim, 0..5);
    run_cycles!(reference, 0..5);
    assert!(
        !sim.is_compiled(),
        "the checkpoint must come from the interpreter tier"
    );
    let checkpoint = sim.checkpoint().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let clk = sim.event("clk");
    while !sim.is_compiled() {
        assert!(
            std::time::Instant::now() < deadline,
            "promotion did not happen"
        );
        sim.tick(clk).unwrap();
    }
    sim.restore(&checkpoint).unwrap();
    assert_eq!(run_cycles!(sim, 5..40), run_cycles!(reference, 5..40));
}

#[test]
fn checkpoint_forks_into_another_simulator() {
    let mut source = Simulator::builder(DESIGN, "Top").build().unwrap();
    let mut fork = Simulator::builder(DESIGN, "Top").build().unwrap();
    run_cycles!(source, 0..9);
    let checkpoint = source.checkpoint().unwrap();
    fork.restore(&checkpoint).unwrap();
    assert_eq!(run_cycles!(fork, 9..30), run_cycles!(source, 9..30));
}

#[test]
fn restore_rejects_another_design() {
    let source = Simulator::builder(DESIGN, "Top").build().unwrap();
    let mut other = Simulator::builder(
        r#"
        module Top (clk: input clock, d: input logic<8>, q: output logic<8>) {
            always_ff (clk) { q = d; }
        }
        "#,
        "Top",
    )
    .build()
    .unwrap();
    let checkpoint = source.checkpoint().unwrap();
    assert_eq!(
        other.restore(&checkpoint),
        Err(CheckpointError::DesignMismatch)
    );
}

const COUNTER: &str = r#"
module Top (clk: input clock, q: output logic<8>) {
    always_ff (clk) { q = q + 8'd1; }
}
"#;

/// The value lines a VCD file records at `time`.
fn values_at(vcd: &str, time: u64) -> Vec<&str> {
    let marker = format!("#{time}");
    vcd.lines()
        .skip_while(|line| *line != marker)
        .skip(1)
        .take_while(|line| !line.starts_with('#'))
        .collect()
}

#[test]
fn restore_with_vcd_records_the_jump_and_keeps_change_tracking() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wave.vcd");
    let mut sim = Simulator::builder(COUNTER, "Top")
        .vcd(&path)
        .build()
        .unwrap();
    let clk = sim.event("clk");
    for time in 0..=10 {
        sim.dump(time);
        if time == 5 {
            let checkpoint = sim.checkpoint().unwrap();
            for _ in 0..5 {
                sim.tick(clk).unwrap();
                sim.dump(time + 1);
            }
            sim.restore(&checkpoint).unwrap();
            break;
        }
        sim.tick(clk).unwrap();
    }
    assert!(sim.backend_ref().vcd_tracking_enabled());
    // q was 5 at the checkpoint and 10 when it was restored.
    sim.try_dump(11).unwrap();
    sim.flush_vcd().unwrap();
    let vcd = std::fs::read_to_string(&path).unwrap();
    assert!(
        values_at(&vcd, 11)
            .iter()
            .any(|line| line.starts_with("b101 ")),
        "{vcd}"
    );

    let error = sim.try_dump(3).unwrap_err();
    assert!(
        matches!(&error, celox::DumpError::Io(io) if io.kind() == std::io::ErrorKind::InvalidInput),
        "{error}"
    );
}

#[test]
fn switch_vcd_records_a_rewound_simulation_in_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut sim = Simulator::builder(COUNTER, "Top")
        .vcd(dir.path().join("first.vcd"))
        .build()
        .unwrap();
    let clk = sim.event("clk");
    sim.tick(clk).unwrap();
    let checkpoint = sim.checkpoint().unwrap();
    for time in 0..4 {
        sim.tick(clk).unwrap();
        sim.dump(time);
    }
    sim.restore(&checkpoint).unwrap();
    let second = dir.path().join("second.vcd");
    sim.switch_vcd(&second).unwrap();
    sim.try_dump(0).unwrap();
    sim.flush_vcd().unwrap();
    let vcd = std::fs::read_to_string(&second).unwrap();
    assert!(vcd.contains("$enddefinitions"), "{vcd}");
    assert!(
        values_at(&vcd, 0)
            .iter()
            .any(|line| line.starts_with("b1 ")),
        "{vcd}"
    );

    let mut plain = Simulator::builder(COUNTER, "Top").build().unwrap();
    assert!(plain.switch_vcd(dir.path().join("none.vcd")).is_err());
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
fn simulation_restore_rewinds_time_and_clocks() {
    let mut sim = Simulation::builder(DESIGN, "Top").build().unwrap();
    sim.add_clock("clk", 10, 5);
    timed_trace(&mut sim, 100);
    let checkpoint = sim.checkpoint().unwrap();
    assert_eq!(checkpoint.time(), sim.time());
    let expected = timed_trace(&mut sim, 400);
    sim.restore(&checkpoint).unwrap();
    assert_eq!(sim.time(), checkpoint.time());
    assert_eq!(timed_trace(&mut sim, 400), expected);
}

#[test]
fn simulation_checkpoint_forks_into_another_simulation() {
    let mut source = Simulation::builder(DESIGN, "Top").build().unwrap();
    let mut fork = Simulation::builder(DESIGN, "Top").build().unwrap();
    source.add_clock("clk", 10, 5);
    timed_trace(&mut source, 100);
    fork.restore(&source.checkpoint().unwrap()).unwrap();
    assert_eq!(fork.time(), source.time());
    assert_eq!(timed_trace(&mut fork, 400), timed_trace(&mut source, 400));
}
