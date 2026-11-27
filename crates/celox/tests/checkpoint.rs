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
        assert_eq!(first, vec![RuntimeEvent::Display { message: b"a=7".to_vec() }]);
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
    let bytes = checkpoint.to_bytes().unwrap();
    // SAFETY: this is the unchanged encoding of this simulation's checkpoint.
    let checkpoint = unsafe { sim.checkpoint_from_bytes(&bytes) }.unwrap();
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

#[test]
fn simulation_restore_refuses_to_rewind_vcd_until_switched() {
    let dir = tempfile::tempdir().unwrap();
    let mut sim = Simulation::builder(COUNTER, "Top")
        .vcd(dir.path().join("first.vcd"))
        .build()
        .unwrap();
    sim.add_clock("clk", 10, 5);
    sim.run_until(50).unwrap();
    let checkpoint = sim.checkpoint().unwrap();
    sim.run_until(100).unwrap();
    assert_eq!(
        sim.restore(&checkpoint),
        Err(CheckpointError::VcdRewind {
            time: 50,
            last_dumped: 100
        })
    );
    assert_eq!(sim.time(), 100, "a rejected restore changes nothing");

    let second = dir.path().join("second.vcd");
    sim.switch_vcd(&second).unwrap();
    sim.restore(&checkpoint).unwrap();
    sim.run_until(80).unwrap();
    sim.flush_vcd().unwrap();
    let vcd = std::fs::read_to_string(&second).unwrap();
    assert!(vcd.contains("#55") && vcd.contains("#80"), "{vcd}");

    // Running "until" an earlier time does not dump into the past.
    sim.run_until(60).unwrap();
    sim.flush_vcd().unwrap();
    assert_eq!(std::fs::read_to_string(&second).unwrap(), vcd);
}

mod persisted_core {
    use std::path::Path;
    const SOURCE: &str = r#"
module Top(output int value, output string text);
 timeunit 1ns; timeprecision 1ns;
 initial begin
   text = "checkpoint";
   fork
     begin #2; value += 3; text = {text, "-child"}; end
     begin #4; value += 7; end
   join
   #1; value += 10;
 end
endmodule
"#;
    macro_rules! case {
        ($name:ident, $build:ident) => {
            #[test]
            fn $name() {
                let build = || {
                    celox::Simulation::from_sv_sources(
                        vec![(SOURCE, Path::new("persisted.sv"))],
                        "Top",
                    )
                    .$build()
                    .unwrap()
                };
                let mut source = build();
                source.run_until(1).unwrap();
                let bytes = source.checkpoint().unwrap().to_bytes().unwrap();
                source.run_until(6).unwrap();
                let expected = source.capture_state().unwrap();
                let mut restored = build();
                // SAFETY: unchanged bytes from the same build's checkpoint encoder.
                let checkpoint = unsafe { restored.checkpoint_from_bytes(&bytes) }.unwrap();
                restored.restore(&checkpoint).unwrap();
                assert_eq!(restored.time(), 1);
                restored.run_until(6).unwrap();
                assert_eq!(restored.capture_state().unwrap(), expected);
                for length in [0, 8, 23, bytes.len() - 1] {
                    assert!(unsafe { restored.checkpoint_from_bytes(&bytes[..length]) }.is_err());
                }
                let mut corrupt = bytes.clone();
                *corrupt.last_mut().unwrap() ^= 1;
                assert!(unsafe { restored.checkpoint_from_bytes(&corrupt) }.is_err());
                // A well-formed file of version 13, before the transition
                // bins of covergroups: the version is the first field of the
                // payload, a one-byte varint, and the checksum is FNV-1a.
                assert_eq!(bytes[24], 14);
                let mut older = bytes.clone();
                older[24] = 13;
                let hash = older[24..]
                    .iter()
                    .fold(0xcbf29ce484222325u64, |hash, byte| {
                        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                    });
                older[16..24].copy_from_slice(&hash.to_le_bytes());
                assert!(matches!(
                    unsafe { restored.checkpoint_from_bytes(&older) },
                    Err(celox::CheckpointFileError::InvalidContainer)
                ));
            }
        };
    }
    case!(interpreter, build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(native, build_native);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(tiered, build_tiered);
    #[cfg(feature = "cranelift")]
    case!(cranelift, build_cranelift);
    #[cfg(feature = "wasm-runtime")]
    case!(wasm, build_wasm);
}

/// The samplers of covergroup instances that a procedure constructs (IEEE
/// 1800-2023 19.3), and of one embedded in a class (19.4), are live
/// activations of their own: a checkpoint keeps them, and a simulation
/// restored from its bytes samples on as the one that took it does.
mod covergroup_samplers {
    use std::path::Path;
    const SOURCE: &str = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    bit [3:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 4'd1;
    covergroup cg @(posedge clk); coverpoint q { bins b[] = {[0:15]}; } endgroup
    class K;
        covergroup kc @(negedge clk); coverpoint q { bins b[] = {[0:15]}; } endgroup
        function new(); kc = new; endfunction
    endclass
    cg c;
    K k;
    initial begin
        c = new;
        #12 k = new;
    end
endmodule
"#;
    /// The samples of each instance of each covergroup type of `report`.
    fn samples(report: &celox::CoverageReport) -> Vec<(String, Vec<u64>)> {
        report
            .covergroups
            .iter()
            .map(|group| {
                (
                    group.name.clone(),
                    group.instances.iter().map(|i| i.samples).collect(),
                )
            })
            .collect()
    }
    macro_rules! case {
        ($name:ident, $build:ident) => {
            #[test]
            fn $name() {
                let build = || {
                    celox::Simulation::from_sv_sources(
                        vec![(SOURCE, Path::new("samplers.sv"))],
                        "Top",
                    )
                    .$build()
                    .unwrap()
                };
                let mut source = build();
                source.run_until(22).unwrap();
                let checkpoint = source.checkpoint().unwrap();
                let bytes = checkpoint.to_bytes().unwrap();
                source.run_until(62).unwrap();
                let expected = source.coverage();
                // Rising edges at 5, ..., 55, falling ones from 20 to 60.
                assert_eq!(
                    samples(&expected),
                    [
                        ("K::kc".to_owned(), vec![5]),
                        ("Top.cg".to_owned(), vec![6])
                    ]
                );
                let mut restored = build();
                // SAFETY: unchanged bytes from the same build's checkpoint encoder.
                let decoded = unsafe { restored.checkpoint_from_bytes(&bytes) }.unwrap();
                restored.restore(&decoded).unwrap();
                assert_eq!(restored.time(), 22);
                restored.run_until(62).unwrap();
                assert_eq!(restored.coverage(), expected);
                source.restore(&checkpoint).unwrap();
                source.run_until(62).unwrap();
                assert_eq!(source.coverage(), expected);
            }
        };
    }
    case!(interpreter, build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(native, build_native);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(tiered, build_tiered);
    #[cfg(feature = "cranelift")]
    case!(cranelift, build_cranelift);
    #[cfg(feature = "wasm-runtime")]
    case!(wasm, build_wasm);
}

/// The instances of covergroups with `ref` formals that declarations
/// construct (IEEE 1800-2023 19.3) read the variables of their actuals:
/// a simulation restored from a checkpoint samples the same variables as
/// the one that took it, at the clocking event and through `sample()`.
mod covergroup_references {
    use std::path::Path;
    const SOURCE: &str = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    bit [3:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 4'd1;
    logic [3:0] up, down;
    assign up = q;
    assign down = 4'd15 - q;
    covergroup cg(ref logic [3:0] v) @(posedge clk); coverpoint v { bins b[] = {[0:15]}; } endgroup
    cg a = new(up), b = new(down);
    covergroup plain(ref logic [3:0] v); coverpoint v { bins b[] = {[0:15]}; } endgroup
    plain p = new(down);
    always @(negedge clk) p.sample();
endmodule
"#;
    /// The hits of each bin of each instance of each covergroup type of
    /// `report` that was hit.
    fn hits(report: &celox::CoverageReport) -> Vec<(String, Vec<Vec<(String, u64)>>)> {
        report
            .covergroups
            .iter()
            .map(|group| {
                (
                    group.name.clone(),
                    group
                        .instances
                        .iter()
                        .map(|instance| {
                            instance
                                .items
                                .iter()
                                .flat_map(|item| &item.bins)
                                .filter(|bin| bin.hits != 0)
                                .map(|bin| (bin.name.clone(), bin.hits))
                                .collect()
                        })
                        .collect(),
                )
            })
            .collect()
    }
    fn bins(values: std::ops::Range<u64>) -> Vec<(String, u64)> {
        values.map(|value| (format!("b[{value}]"), 1)).collect()
    }
    macro_rules! case {
        ($name:ident, $build:ident) => {
            #[test]
            fn $name() {
                let build = || {
                    celox::Simulation::from_sv_sources(
                        vec![(SOURCE, Path::new("references.sv"))],
                        "Top",
                    )
                    .$build()
                    .unwrap()
                };
                let mut source = build();
                source.run_until(22).unwrap();
                let checkpoint = source.checkpoint().unwrap();
                let bytes = checkpoint.to_bytes().unwrap();
                source.run_until(62).unwrap();
                let expected = source.coverage();
                // Rising edges at 5, ..., 55 see q = 0..5; falling ones at
                // 10, ..., 60 see q = 1..6.
                assert_eq!(
                    hits(&expected),
                    [
                        ("Top.cg".to_owned(), vec![bins(0..6), bins(10..16)]),
                        ("Top.plain".to_owned(), vec![bins(9..15)]),
                    ]
                );
                let mut restored = build();
                // SAFETY: unchanged bytes from the same build's checkpoint encoder.
                let decoded = unsafe { restored.checkpoint_from_bytes(&bytes) }.unwrap();
                restored.restore(&decoded).unwrap();
                assert_eq!(restored.time(), 22);
                restored.run_until(62).unwrap();
                assert_eq!(restored.coverage(), expected);
                source.restore(&checkpoint).unwrap();
                source.run_until(62).unwrap();
                assert_eq!(source.coverage(), expected);
            }
        };
    }
    case!(interpreter, build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(native, build_native);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(tiered, build_tiered);
    #[cfg(feature = "cranelift")]
    case!(cranelift, build_cranelift);
    #[cfg(feature = "wasm-runtime")]
    case!(wasm, build_wasm);
}

/// The transitions of a coverpoint (IEEE 1800-2023 19.5.2) that are under
/// way are part of a checkpoint: a simulation restored in the middle of a
/// sequence, from the checkpoint or from its bytes, ends it as the one
/// that took it does.
mod covergroup_transitions {
    use std::path::Path;
    const SOURCE: &str = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    bit [3:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 4'd1;
    covergroup cg @(posedge clk);
        coverpoint q {
            bins t = (0 => 1 => 2 => 3);
            bins g = (1 => 4 [-> 1] => 5);
            bins r[] = (1 => [2:3] [* 1:2]);
            bins d = default sequence;
        }
    endgroup
    cg c = new;
endmodule
"#;
    /// The hits of each bin of the coverpoint.
    fn hits(report: &celox::CoverageReport) -> Vec<(String, u64)> {
        report.covergroups[0].instances[0].items[0]
            .bins
            .iter()
            .filter(|bin| bin.hits != 0)
            .map(|bin| (bin.name.clone(), bin.hits))
            .collect()
    }
    macro_rules! case {
        ($name:ident, $build:ident) => {
            #[test]
            fn $name() {
                let build = || {
                    celox::Simulation::from_sv_sources(
                        vec![(SOURCE, Path::new("transitions.sv"))],
                        "Top",
                    )
                    .$build()
                    .unwrap()
                };
                let mut source = build();
                // The rising edges at 5 and 15 sample 0 and 1: `t`, `g`
                // and the bins of `r` are under way.
                source.run_until(22).unwrap();
                assert!(hits(&source.coverage()).is_empty());
                let checkpoint = source.checkpoint().unwrap();
                let bytes = checkpoint.to_bytes().unwrap();
                source.run_until(62).unwrap();
                let expected = source.coverage();
                // The edges up to 55 sample 0 to 5; every sample but the
                // first goes on with `t` or `g`, so `d` has none.
                assert_eq!(
                    hits(&expected),
                    [
                        ("t".to_owned(), 1),
                        ("g".to_owned(), 1),
                        ("r[1=>2]".to_owned(), 1),
                        ("r[1=>2=>3]".to_owned(), 1)
                    ]
                );
                let mut restored = build();
                // SAFETY: unchanged bytes from the same build's checkpoint encoder.
                let decoded = unsafe { restored.checkpoint_from_bytes(&bytes) }.unwrap();
                restored.restore(&decoded).unwrap();
                assert_eq!(restored.time(), 22);
                restored.run_until(62).unwrap();
                assert_eq!(restored.coverage(), expected);
                source.restore(&checkpoint).unwrap();
                source.run_until(62).unwrap();
                assert_eq!(source.coverage(), expected);
            }
        };
    }
    case!(interpreter, build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(native, build_native);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(tiered, build_tiered);
    #[cfg(feature = "cranelift")]
    case!(cranelift, build_cranelift);
    #[cfg(feature = "wasm-runtime")]
    case!(wasm, build_wasm);
}

/// The instances of covergroups declared in interfaces (IEEE 1800-2023
/// 19.3) belong to each instance of the interface: a simulation restored
/// from a checkpoint samples them on as the one that took it does, those a
/// procedure of the interface constructs at its clocking event, those of
/// a declaration with an initializer and those its processes sample.
mod covergroup_interfaces {
    use std::path::Path;
    const SOURCE: &str = r#"`timescale 1ns/1ns
interface ci(input bit clk);
    bit [3:0] q = 0;
    always_ff @(posedge clk) q <= q + 4'd1;
    covergroup cg @(posedge clk); coverpoint q { bins b[] = {[0:15]}; } endgroup
    covergroup plain; coverpoint q { bins b[] = {[0:15]}; } endgroup
    cg c;
    cg d = new;
    plain p[2];
    initial begin
        foreach (p[i]) p[i] = new;
        #12 c = new;
    end
    always @(negedge clk) p[q[0]].sample();
endinterface
module Top;
    bit clk = 0;
    always #5 clk = ~clk;
    ci a(.clk(clk));
    ci b(.clk(clk));
endmodule
"#;
    /// The samples of each instance of each covergroup type of `report`.
    fn samples(report: &celox::CoverageReport) -> Vec<(String, Vec<u64>)> {
        report
            .covergroups
            .iter()
            .map(|group| {
                (
                    group.name.clone(),
                    group.instances.iter().map(|i| i.samples).collect(),
                )
            })
            .collect()
    }
    macro_rules! case {
        ($name:ident, $build:ident) => {
            #[test]
            fn $name() {
                let build = || {
                    celox::Simulation::from_sv_sources(
                        vec![(SOURCE, Path::new("interfaces.sv"))],
                        "Top",
                    )
                    .$build()
                    .unwrap()
                };
                let mut source = build();
                source.run_until(22).unwrap();
                let checkpoint = source.checkpoint().unwrap();
                let bytes = checkpoint.to_bytes().unwrap();
                source.run_until(62).unwrap();
                let expected = source.coverage();
                // Rising edges at 5, ..., 55: six for the declarations, five for the
                // instances constructed at 12; falling ones at 10, ..., 60 sample
                // `p[q[0]]`, q = 1..6.
                assert_eq!(
                    samples(&expected),
                    [
                        ("ci.cg".to_owned(), vec![6, 6, 5, 5]),
                        ("ci.plain".to_owned(), vec![3, 3, 3, 3])
                    ]
                );
                let mut restored = build();
                // SAFETY: unchanged bytes from the same build's checkpoint encoder.
                let decoded = unsafe { restored.checkpoint_from_bytes(&bytes) }.unwrap();
                restored.restore(&decoded).unwrap();
                assert_eq!(restored.time(), 22);
                restored.run_until(62).unwrap();
                assert_eq!(restored.coverage(), expected);
                source.restore(&checkpoint).unwrap();
                source.run_until(62).unwrap();
                assert_eq!(source.coverage(), expected);
            }
        };
    }
    case!(interpreter, build_interpreter);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(native, build_native);
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    case!(tiered, build_tiered);
    #[cfg(feature = "cranelift")]
    case!(cranelift, build_cranelift);
    #[cfg(feature = "wasm-runtime")]
    case!(wasm, build_wasm);
}
