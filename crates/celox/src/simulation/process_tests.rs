use celox_frontend_sdk::{
    BinaryOp, Constant, Edge, ExprId, FrontendArtifact, ModuleBuilder, Statement, UnaryOp,
    ValueType,
};

use super::Simulation;
use crate::{SimBackend, Simulator, SimulatorBuilder};

fn constant(module: &mut ModuleBuilder, value: u64, width: usize) -> ExprId {
    module.constant(Constant::two_state(value, width).unwrap())
}

/// `forever #5 clk = ~clk;` drives a counter, and a second process counts
/// `steps` one-unit delays into `ticks` before finishing the simulation.
fn design(four_state: bool, steps: usize) -> FrontendArtifact {
    let bit = ValueType::new(1, false, four_state).unwrap();
    let word = ValueType::new(16, false, four_state).unwrap();
    let mut module = ModuleBuilder::new("Processes").unwrap();
    let clk = module.internal("clk", bit).unwrap();
    let count = module.output("count", word).unwrap();
    let ticks = module.output("ticks", word).unwrap();
    for signal in [clk, count, ticks] {
        let width = if signal == clk { 1 } else { 16 };
        module
            .set_initial(signal, Constant::two_state(0u8, width).unwrap())
            .unwrap();
    }
    let count_expr = module.read(count).unwrap();
    let one = constant(&mut module, 1, 16);
    let next = module.binary(BinaryOp::Add, count_expr, one, word).unwrap();
    let count_target = module.whole(count).unwrap();
    module
        .register(count_target, next, clk, Edge::Posedge, None, None)
        .unwrap();

    let five = constant(&mut module, 5, 8);
    let clk_expr = module.read(clk).unwrap();
    let toggled = module.unary(UnaryOp::BitNot, clk_expr, bit).unwrap();
    let clk_target = module.whole(clk).unwrap();
    module
        .process(vec![Statement::Forever {
            body: vec![
                Statement::Delay { amount: five },
                Statement::Assign {
                    target: clk_target,
                    value: toggled,
                },
            ],
        }])
        .unwrap();

    // Straight-line suspensions, so a long body needs the two-level
    // resume dispatch.
    let unit = constant(&mut module, 1, 8);
    let ticks_expr = module.read(ticks).unwrap();
    let incremented = module.binary(BinaryOp::Add, ticks_expr, one, word).unwrap();
    let ticks_target = module.whole(ticks).unwrap();
    let mut body = Vec::new();
    for _ in 0..steps {
        body.push(Statement::Delay { amount: unit });
        body.push(Statement::Assign {
            target: ticks_target,
            value: incremented,
        });
    }
    body.push(Statement::Finish);
    module.process(body).unwrap();
    module.finish()
}

fn check<B: SimBackend>(mut sim: Simulation<B>, steps: u64) {
    let count = sim.signal("count");
    let ticks = sim.signal("ticks");
    // Rising edges at 5, 15 and 25.
    sim.run_until(30).unwrap();
    assert!(!sim.is_finished());
    assert_eq!(sim.get(count), 3u16.into());
    assert_eq!(sim.get(ticks), 30u16.into());
    sim.run_until(u64::MAX - 1).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.time(), steps);
    assert_eq!(sim.get(ticks), steps.into());
    assert_eq!(
        sim.get_four_state(count),
        (((steps + 5) / 10).into(), 0u8.into())
    );
}

fn builder(four_state: bool, steps: usize) -> SimulatorBuilder<'static, Simulator> {
    Simulator::from_frontend(design(four_state, steps))
        .four_state(four_state)
        .emit_triggers()
}

fn check_all_backends(four_state: bool, steps: usize) {
    let timed = || Simulation::from_frontend(design(four_state, steps)).four_state(four_state);
    check(timed().build_interpreter().unwrap(), steps as u64);
    #[cfg(feature = "cranelift")]
    check(timed().build_cranelift().unwrap(), steps as u64);
    #[cfg(feature = "wasm-runtime")]
    check(timed().build_wasm().unwrap(), steps as u64);
    #[cfg(feature = "cranelift")]
    check(
        timed()
            .threads(4)
            .parallel_partition(crate::ParallelPartition::Always)
            .build_cranelift()
            .unwrap(),
        steps as u64,
    );
    check(timed().build_tiered().unwrap(), steps as u64);
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    {
        check(timed().build_native().unwrap(), steps as u64);
        check(
            timed()
                .threads(4)
                .parallel_partition(crate::ParallelPartition::Always)
                .build_native()
                .unwrap(),
            steps as u64,
        );
        // A native image starts an untimed Simulator, which runs no
        // process.
        let error = builder(four_state, steps)
            .compile_native()
            .map(|_| ())
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            crate::SimulatorErrorKind::ProcessRequiresSimulation(_)
        ));
    }
}

#[test]
fn timed_tiered_promotion_preserves_suspended_processes_and_nba() {
    let source = r#"
            `timescale 1fs/1fs
            module Top(output logic [7:0] count);
                logic clk = 0;
                initial count = 0;
                always #5 clk = ~clk;
                always @(posedge clk) count <= count + 1;
                initial begin #36; $finish; end
            endmodule
        "#;
    for four_state in [false, true] {
        for threads in [1, 4] {
            let mut sim = Simulation::from_sv_sources(
                vec![(source, std::path::Path::new("tiered.sv"))],
                "Top",
            )
            .four_state(four_state)
            .threads(threads)
            .parallel_partition(crate::ParallelPartition::Always)
            .tier_promotion(crate::TierPromotion::AfterSteps(1000))
            .build_tiered()
            .unwrap();
            let count = sim.signal("count");
            sim.run_until(10).unwrap();
            assert!(!sim.simulator.is_compiled());
            assert_eq!(sim.get(count), 1u8.into());
            // Poll at the same simulation time while future delayed processes
            // remain suspended. The tier transition must retain their resume
            // positions and subsequent Active/NBA ordering (IEEE 1800-2023 4.5).
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !sim.simulator.is_compiled() {
                sim.eval_comb().unwrap();
                assert!(
                    sim.simulator.promotion_error().is_none(),
                    "{:?}",
                    sim.simulator.promotion_error()
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "tiered compilation timed out"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(sim.time(), 10);
            assert_eq!(sim.get_four_state(count), (1u8.into(), 0u8.into()));
            sim.run_until(40).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.get_four_state(count), (4u8.into(), 0u8.into()));
            assert!(sim.simulator.tiered_execution_stats().compiled_evaluations > 0);
        }
    }
}

#[test]
fn processes_run_on_every_backend() {
    check_all_backends(false, 37);
}

#[test]
fn four_state_processes_run_on_every_backend() {
    check_all_backends(true, 37);
}

#[test]
fn long_processes_dispatch_through_two_switch_levels() {
    check_all_backends(false, 300);
}
#[test]
fn odd_clock_phase_survives_checkpoint_across_backends() {
    fn check<B: crate::SimBackend>(mut backend: crate::Simulator<B>) {
        // Use a known low initial clock in both state representations.
        let clock = backend.signal("clk");
        let count = backend.signal("count");
        backend
            .modify(|io| {
                io.set(clock, 0u8);
                io.set(count, 0u8);
            })
            .unwrap();
        let mut sim = Simulation::new(backend);
        sim.add_clock("clk", 3, 5);
        sim.run_until(11).unwrap();
        assert_eq!(sim.get(count), 3u8.into());
        assert_eq!(sim.get(clock), 1u8.into());
        assert_eq!(sim.next_event_time(), Some(13));
        let saved = sim.checkpoint().unwrap();
        sim.run_until(12).unwrap();
        assert_eq!(sim.get(clock), 1u8.into());
        sim.run_until(13).unwrap();
        assert_eq!(sim.get(clock), 0u8.into());
        sim.run_until(14).unwrap();
        assert_eq!(sim.get(count), 4u8.into());
        sim.restore(&saved).unwrap();
        assert_eq!(sim.time(), 11);
        assert_eq!(sim.get(count), 3u8.into());
        assert_eq!(sim.get(clock), 1u8.into());
        assert_eq!(sim.next_event_time(), Some(13));
        sim.run_until(12).unwrap();
        assert_eq!(sim.get(clock), 1u8.into());
        sim.run_until(13).unwrap();
        assert_eq!(sim.get(clock), 0u8.into());
        sim.run_until(14).unwrap();
        assert_eq!(sim.get(count), 4u8.into());
    }
    let code =
        "module Top (clk: input clock, count: output logic<8>) { always_ff (clk) { count += 1; } }";
    for four_state in [false, true] {
        let build = || {
            crate::Simulator::builder(code, "Top")
                .four_state(four_state)
                .emit_triggers()
        };
        check(build().build_interpreter().unwrap());
        #[cfg(feature = "cranelift")]
        check(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        check(build().build_wasm().unwrap());
        check(build().build_tiered().unwrap());
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        check(build().build_native().unwrap());
    }
}
/// A clock taken from one bit of a register is edge-sensed on that bit on
/// every backend, not on the register as a whole. In four-state mode the
/// WebAssembly backend leaves the X-initialized bits above the one-bit
/// clock set, which the scheduler must not read as a high level.
#[test]
fn register_bit_clocks_follow_the_selected_bit_on_every_backend() {
    fn check<B: crate::SimBackend>(backend: crate::Simulator<B>, step: u64, bit: u64) {
        let mut sim = Simulation::new(backend);
        let a = sim.signal("cnt1");
        let b = sim.signal("cnt2");
        sim.schedule("rst", 0, 0).unwrap();
        sim.schedule("clk", 0, 0).unwrap();
        sim.step().unwrap();
        sim.schedule("rst", 5, 1).unwrap();
        sim.step().unwrap();
        let mut count = 0u64;
        for edge in 1..=12u64 {
            sim.schedule("clk", edge * 20 - 10, 1).unwrap();
            sim.step().unwrap();
            let (before, after) = (((edge - 1) * step) >> bit, (edge * step) >> bit);
            count += u64::from(before & 1 == 0 && after & 1 == 1);
            assert_eq!(sim.get(a), ((edge * step) & 0xff).into());
            assert_eq!(sim.get(b), count.into(), "a[{bit}] after edge {edge}");
            sim.schedule("clk", edge * 20, 0).unwrap();
            sim.step().unwrap();
        }
    }
    for (step, bit) in [(1, 0), (2, 0), (1, 2)] {
        let code = format!(
            "module Top (clk: input '_ clock, rst: input '_ reset_async_low,
                             cnt1: output logic<8>, cnt2: output logic<8>) {{
                    var a: logic<8>;
                    var b: logic<8>;
                    always_ff (clk, rst) {{ if_reset {{ a = 0; }} else {{ a = a + 8'd{step}; }} }}
                    let divided: '_ clock = a[{bit}];
                    always_ff (divided, rst) {{ if_reset {{ b = 0; }} else {{ b = b + 8'd1; }} }}
                    assign cnt1 = a;
                    assign cnt2 = b;
                }}"
        );
        for four_state in [false, true] {
            let build = || {
                crate::Simulator::builder(&code, "Top")
                    .four_state(four_state)
                    .emit_triggers()
            };
            check(build().build_interpreter().unwrap(), step, bit);
            #[cfg(feature = "cranelift")]
            check(build().build_cranelift().unwrap(), step, bit);
            #[cfg(feature = "wasm-runtime")]
            check(build().build_wasm().unwrap(), step, bit);
            check(build().build_tiered().unwrap(), step, bit);
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            check(build().build_native().unwrap(), step, bit);
        }
    }
}

/// Clocks computed from `clk` share its edge: `clk & en` is gated by the
/// enable from before the edge (here a register of `clk`'s own domain)
/// and its domain reads the state from before `clk`'s domain commits; an
/// edge raising `en` raises the gate after the commit, a further edge
/// that reads the committed state. `~clk` rises when `clk` falls. Timed
/// steps and event-based ticks, one clock period each, agree on every
/// backend, in both state modes; the four-state ticks judge the gate by
/// settling the design.
#[test]
fn gated_clocks_share_their_source_edge_on_every_backend() {
    const CODE: &str = "module Top (clk: input '_ clock, rst: input '_ reset_async_low,
                en_in: input logic, cnt1: output logic<8>, sampled: output logic<8>,
                fired: output logic<8>, inverted_sampled: output logic<8>) {
            var a: logic<8>;
            var en: logic;
            var b: logic<8>;
            var c: logic<8>;
            var d: logic<8>;
            always_ff (clk, rst) {
                if_reset { a = 0; en = 1; } else { a = a + 8'd1; en = en_in; }
            }
            let gated: '_ clock = clk & en;
            always_ff (gated, rst) {
                if_reset { b = 0; c = 0; } else { b = a; c = c + 8'd1; }
            }
            let inverted: '_ clock = ~clk;
            always_ff (inverted, rst) { if_reset { d = 0; } else { d = a; } }
            assign cnt1 = a;
            assign sampled = b;
            assign fired = c;
            assign inverted_sampled = d;
        }";
    const ENABLES: [u8; 8] = [1, 1, 0, 0, 1, 1, 0, 1];
    fn expected() -> Vec<[u64; 4]> {
        let (mut gate, mut sampled, mut fired) = (1, 0, 0);
        ENABLES
            .iter()
            .zip(1u64..)
            .map(|(&enable, edge)| {
                if gate == 1 {
                    // The edge passes the open gate before any commit.
                    sampled = edge - 1;
                    fired += 1;
                } else if enable == 1 {
                    // The commit raises `en` while `clk` is high.
                    sampled = edge;
                    fired += 1;
                }
                gate = enable;
                [edge, sampled, fired, edge]
            })
            .collect()
    }
    fn observe<B: crate::SimBackend>(sim: &mut crate::Simulator<B>) -> [u64; 4] {
        ["cnt1", "sampled", "fired", "inverted_sampled"].map(|name| {
            let signal = sim.signal(name);
            sim.get(signal).try_into().unwrap()
        })
    }
    fn timed<B: crate::SimBackend>(backend: crate::Simulator<B>) -> Vec<[u64; 4]> {
        let mut sim = Simulation::new(backend);
        let en_in = sim.signal("en_in");
        // Assert the reset with an edge, then release it.
        for (port, time, value) in [("rst", 0, 1), ("clk", 0, 0), ("rst", 2, 0), ("rst", 5, 1)] {
            sim.schedule(port, time, value).unwrap();
            sim.step().unwrap();
        }
        ENABLES
            .iter()
            .zip(1u64..)
            .map(|(&enable, edge)| {
                sim.modify(|io| io.set(en_in, enable)).unwrap();
                sim.schedule("clk", edge * 20 - 10, 1).unwrap();
                sim.step().unwrap();
                sim.schedule("clk", edge * 20, 0).unwrap();
                sim.step().unwrap();
                observe(&mut sim.simulator)
            })
            .collect()
    }
    fn ticked<B: crate::SimBackend>(mut sim: crate::Simulator<B>) -> Vec<[u64; 4]> {
        let clk = sim.event("clk");
        let (rst, en_in) = (sim.signal("rst"), sim.signal("en_in"));
        sim.modify(|io| io.set(rst, 0u8)).unwrap();
        sim.tick(clk).unwrap();
        sim.modify(|io| io.set(rst, 1u8)).unwrap();
        ENABLES
            .iter()
            .map(|&enable| {
                sim.modify(|io| io.set(en_in, enable)).unwrap();
                sim.tick(clk).unwrap();
                observe(&mut sim)
            })
            .collect()
    }
    let want = expected();
    for four_state in [false, true] {
        let build = || {
            crate::Simulator::builder(CODE, "Top")
                .four_state(four_state)
                .emit_triggers()
        };
        let context = |mode: &str| format!("{mode}, four_state={four_state}");
        assert_eq!(
            timed(build().build_interpreter().unwrap()),
            want,
            "{}",
            context("interp timed")
        );
        assert_eq!(
            ticked(build().build_interpreter().unwrap()),
            want,
            "{}",
            context("interp tick")
        );
        #[cfg(feature = "cranelift")]
        assert_eq!(
            timed(build().build_cranelift().unwrap()),
            want,
            "{}",
            context("cranelift timed")
        );
        #[cfg(feature = "cranelift")]
        assert_eq!(
            ticked(build().build_cranelift().unwrap()),
            want,
            "{}",
            context("cranelift tick")
        );
        #[cfg(feature = "wasm-runtime")]
        assert_eq!(
            timed(build().build_wasm().unwrap()),
            want,
            "{}",
            context("wasm timed")
        );
        #[cfg(feature = "wasm-runtime")]
        assert_eq!(
            ticked(build().build_wasm().unwrap()),
            want,
            "{}",
            context("wasm tick")
        );
        assert_eq!(
            timed(build().build_tiered().unwrap()),
            want,
            "{}",
            context("tiered timed")
        );
        assert_eq!(
            ticked(build().build_tiered().unwrap()),
            want,
            "{}",
            context("tiered tick")
        );
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            assert_eq!(
                timed(build().build_native().unwrap()),
                want,
                "{}",
                context("native timed")
            );
            assert_eq!(
                ticked(build().build_native().unwrap()),
                want,
                "{}",
                context("native tick")
            );
        }
    }
}

const GENERATED_ALIAS_SOURCE: &str = r#"
module Generator(input logic clk, output logic g);
    initial g = 0;
    always_ff @(posedge clk) g <= ~g;
endmodule
module Top(input logic cp, cn, output logic [7:0] p, n);
    logic gp, gn;
    Generator up(.clk(cp), .g(gp));
    Generator un(.clk(cn), .g(gn));
    initial begin p = 0; n = 0; end
    always_ff @(posedge gp) p <= p + 1;
    always_ff @(negedge gn) n <= n + 1;
endmodule
"#;

#[test]
fn generated_alias_edges_keep_their_instances_and_stable_phases() {
    fn check<B: crate::SimBackend>(mut backend: crate::Simulator<B>) {
        for (expected, address) in backend
            .program
            .design
            .events
            .ordered_events
            .iter()
            .enumerate()
        {
            let event = backend.backend.resolve_event(address);
            assert_eq!(
                celox_runtime::backend::EventHandle::id(&event),
                expected,
                "event handles must match IDs embedded in trigger instructions"
            );
        }
        let cp = backend.signal("cp");
        let cn = backend.signal("cn");
        backend
            .modify(|io| {
                io.set(cp, 0u8);
                io.set(cn, 0u8);
            })
            .unwrap();
        let mut sim = Simulation::new(backend);
        sim.add_clock("cp", 10, 2);
        sim.add_clock("cn", 10, 7);
        sim.run_until(27).unwrap();
        assert_eq!(sim.get(sim.signal("p")), 2u8.into());
        assert_eq!(sim.get(sim.signal("n")), 1u8.into());
    }
    let source = GENERATED_ALIAS_SOURCE;
    for four_state in [false, true] {
        let build = || {
            crate::Simulator::from_sv_sources(
                vec![(source, std::path::Path::new("generated_alias.sv"))],
                "Top",
            )
            .four_state(four_state)
            .emit_triggers()
        };
        check(build().build_interpreter().unwrap());
        #[cfg(feature = "cranelift")]
        check(build().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        check(build().build_wasm().unwrap());
        check(build().build_tiered().unwrap());
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        check(build().build_native().unwrap());
    }
}
#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn timed_alias_edges_and_checkpoint_survive_tiered_promotion() {
    for four_state in [false, true] {
        let mut backend = crate::Simulator::from_sv_sources(
            vec![(
                GENERATED_ALIAS_SOURCE,
                std::path::Path::new("promoted_alias.sv"),
            )],
            "Top",
        )
        .four_state(four_state)
        .emit_triggers()
        .tier_promotion(crate::TierPromotion::AfterSteps(32))
        .build_tiered()
        .unwrap();
        let cp = backend.signal("cp");
        let cn = backend.signal("cn");
        backend
            .modify(|io| {
                io.set(cp, 0u8);
                io.set(cn, 0u8);
            })
            .unwrap();
        let mut sim = Simulation::new(backend);
        sim.add_clock("cp", 10, 2);
        sim.add_clock("cn", 10, 7);
        let saved = sim.checkpoint().unwrap();
        assert!(!sim.simulator.is_compiled());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut until = 27u64;
        loop {
            sim.run_until(until).unwrap();
            assert_eq!(
                sim.get(sim.signal("p")),
                (((until - 2) / 20 + 1) & 255).into()
            );
            assert_eq!(
                sim.get(sim.signal("n")),
                (((until - 17) / 20 + 1) & 255).into()
            );
            if sim.simulator.is_compiled() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed tiered backend did not promote"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            until += 10;
        }
        sim.restore(&saved).unwrap();
        assert!(sim.simulator.is_compiled());
        sim.run_until(27).unwrap();
        assert_eq!(sim.get(sim.signal("p")), 2u8.into());
        assert_eq!(sim.get(sim.signal("n")), 1u8.into());
    }
}
