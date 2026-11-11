use celox_frontend_sdk::{
    BinaryOp, Constant, Edge, EventEdge, EventTerm, ExprId, FrontendArtifact, ModuleBuilder,
    Statement, ValueType,
};
use num_bigint::BigUint;

use super::Simulation;
use crate::SimBackend;

/// The levels of a clock walking all twelve changes between 0, 1, x and
/// z (an Euler circuit from 0), as (value, mask), with the posedges and
/// negedges counted after each change.
const WALK: [((u8, u8), u8, u8); 12] = [
    ((1, 0), 1, 0),
    ((0, 0), 1, 1),
    ((1, 1), 2, 1),
    ((0, 0), 2, 2),
    ((0, 1), 3, 2),
    ((1, 0), 4, 2),
    ((1, 1), 4, 3),
    ((1, 0), 5, 3),
    ((0, 1), 5, 4),
    ((1, 1), 5, 4),
    ((0, 1), 5, 4),
    ((0, 0), 5, 5),
];

/// Build `$check` against a timed simulation of each backend.
macro_rules! on_every_backend {
    ($builder:expr, $check:ident) => {{
        let builder = || $builder.four_state(true).emit_triggers();
        $check(builder().build_interpreter().unwrap());
        #[cfg(feature = "cranelift")]
        $check(builder().build_cranelift().unwrap());
        #[cfg(feature = "wasm-runtime")]
        $check(builder().build_wasm().unwrap());
        $check(builder().build_tiered().unwrap());
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        $check(builder().build_native().unwrap());
    }};
}

fn set<B: SimBackend>(sim: &mut Simulation<B>, name: &str, (value, mask): (u8, u8)) {
    let signal = sim.signal(name);
    sim.modify(|io| io.set_four_state(signal, value.into(), mask.into()))
        .unwrap();
}

fn get<B: SimBackend>(sim: &mut Simulation<B>, name: &str) -> BigUint {
    let signal = sim.signal(name);
    sim.get(signal)
}

/// Two processes drive `cp` and `cn` through [`WALK`], one change per
/// time unit; registers count the posedges of `cp` and the negedges of
/// `cn`.
fn process_design() -> FrontendArtifact {
    let bit = ValueType::new(1, false, true).unwrap();
    let word = ValueType::new(8, false, true).unwrap();
    let mut module = ModuleBuilder::new("Top").unwrap();
    let cp = module.internal("cp", bit).unwrap();
    let cn = module.internal("cn", bit).unwrap();
    let pos = module.output("pos", word).unwrap();
    let neg = module.output("neg", word).unwrap();
    for (signal, width) in [(cp, 1), (cn, 1), (pos, 8), (neg, 8)] {
        module
            .set_initial(signal, Constant::two_state(0u8, width).unwrap())
            .unwrap();
    }
    let one = module.constant(Constant::two_state(1u8, 8).unwrap());
    for (counter, clock, edge) in [(pos, cp, Edge::Posedge), (neg, cn, Edge::Negedge)] {
        let read = module.read(counter).unwrap();
        let next = module.binary(BinaryOp::Add, read, one, word).unwrap();
        let target = module.whole(counter).unwrap();
        module
            .register(target, next, clock, edge, None, None)
            .unwrap();
    }
    let delay = module.constant(Constant::two_state(1u8, 8).unwrap());
    for clock in [cp, cn] {
        let target = module.whole(clock).unwrap();
        let mut body = Vec::new();
        for ((value, mask), _, _) in WALK {
            let level: ExprId = module.constant(Constant::four_state(value, mask, 1).unwrap());
            body.push(Statement::Delay { amount: delay });
            body.push(Statement::Assign {
                target,
                value: level,
            });
        }
        module.process(body).unwrap();
    }
    module.finish()
}

/// A process drives `d` through [`WALK`] and pulses `clk` after each
/// change; registers `c` and `c2` follow `d` on the rising edges of
/// `clk`. Event-control waits `@(posedge c)` and `@(negedge c)` and
/// registers on the posedges of `c` and the negedges of `c2` count the
/// edges of the register clock.
fn wait_design() -> FrontendArtifact {
    let bit = ValueType::new(1, false, true).unwrap();
    let word = ValueType::new(8, false, true).unwrap();
    let mut module = ModuleBuilder::new("Top").unwrap();
    let clk = module.internal("clk", bit).unwrap();
    let d = module.internal("d", bit).unwrap();
    let c = module.internal("c", bit).unwrap();
    let c2 = module.internal("c2", bit).unwrap();
    let counters =
        ["wait_pos", "wait_neg", "ff_pos", "ff_neg"].map(|name| module.output(name, word).unwrap());
    for signal in [clk, d, c, c2] {
        module
            .set_initial(signal, Constant::two_state(0u8, 1).unwrap())
            .unwrap();
    }
    for counter in counters {
        module
            .set_initial(counter, Constant::two_state(0u8, 8).unwrap())
            .unwrap();
    }
    let d_expr = module.read(d).unwrap();
    for register in [c, c2] {
        let target = module.whole(register).unwrap();
        module
            .register(target, d_expr, clk, Edge::Posedge, None, None)
            .unwrap();
    }
    let one = module.constant(Constant::two_state(1u8, 8).unwrap());
    let increment = |module: &mut ModuleBuilder, counter| {
        let read = module.read(counter).unwrap();
        let next = module.binary(BinaryOp::Add, read, one, word).unwrap();
        (module.whole(counter).unwrap(), next)
    };
    let [wait_pos, wait_neg, ff_pos, ff_neg] = counters;
    for (counter, clock, edge) in [(ff_pos, c, Edge::Posedge), (ff_neg, c2, Edge::Negedge)] {
        let (target, next) = increment(&mut module, counter);
        module
            .register(target, next, clock, edge, None, None)
            .unwrap();
    }
    for (counter, edge) in [
        (wait_pos, EventEdge::Posedge),
        (wait_neg, EventEdge::Negedge),
    ] {
        let (target, value) = increment(&mut module, counter);
        module
            .process(vec![Statement::Forever {
                body: vec![
                    Statement::WaitEvent {
                        terms: vec![EventTerm { signal: c, edge }],
                    },
                    Statement::Assign { target, value },
                ],
            }])
            .unwrap();
    }
    let delay = module.constant(Constant::two_state(1u8, 8).unwrap());
    let high = module.constant(Constant::two_state(1u8, 1).unwrap());
    let low = module.constant(Constant::two_state(0u8, 1).unwrap());
    let d_target = module.whole(d).unwrap();
    let clk_target = module.whole(clk).unwrap();
    let mut body = Vec::new();
    for ((value, mask), _, _) in WALK {
        let level = module.constant(Constant::four_state(value, mask, 1).unwrap());
        body.push(Statement::Assign {
            target: d_target,
            value: level,
        });
        body.push(Statement::Delay { amount: delay });
        body.push(Statement::Assign {
            target: clk_target,
            value: high,
        });
        body.push(Statement::Delay { amount: delay });
        body.push(Statement::Assign {
            target: clk_target,
            value: low,
        });
    }
    module.process(body).unwrap();
    module.finish()
}

/// Event-control waits and flip-flop domains see the same Table 9-2
/// edges of a register clock.
#[test]
fn waits_on_register_clocks_follow_table_9_2() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        for (step, (_, pos, neg)) in (0u64..).zip(WALK) {
            let time = 2 * step + 1;
            sim.run_until(time).unwrap();
            let counts =
                ["wait_pos", "wait_neg", "ff_pos", "ff_neg"].map(|name| get(&mut sim, name));
            assert_eq!(
                counts,
                [pos, neg, pos, neg].map(BigUint::from),
                "wait_pos, wait_neg, ff_pos, ff_neg at {time}"
            );
        }
    }
    on_every_backend!(Simulation::from_frontend(wait_design()), check);
}

#[test]
fn process_driven_clocks_follow_table_9_2() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        for (time, (_, pos, neg)) in (1..).zip(WALK) {
            sim.run_until(time).unwrap();
            assert_eq!(get(&mut sim, "pos"), pos.into(), "posedges at {time}");
            assert_eq!(get(&mut sim, "neg"), neg.into(), "negedges at {time}");
        }
    }
    on_every_backend!(Simulation::from_frontend(process_design()), check);
}

const REGISTER_CLOCK: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    d  : input logic,
    pos: output logic<8>,
    neg: output logic<8>,
) {
    var c: logic;
    always_ff (clk) {
        c = d;
    }
    let cp: '_ clock         = c;
    let cn: '_ clock_negedge = c;
    var p : logic<8>;
    var n : logic<8>;
    always_ff (cp, rst) {
        if_reset {
            p = 0;
        } else {
            p = p + 8'd1;
        }
    }
    always_ff (cn, rst) {
        if_reset {
            n = 0;
        } else {
            n = n + 8'd1;
        }
    }
    assign pos = p;
    assign neg = n;
}
"#;

/// A clock that is a register follows `d` through [`WALK`], one change
/// per rising edge of `clk`. `clk` starts at x, so its first rise, x to
/// 1, is a posedge too.
#[test]
fn register_clocks_follow_table_9_2() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        // x -> 0 asserts the reset.
        sim.schedule("rst", 0, 0).unwrap();
        sim.schedule("rst", 1, 1).unwrap();
        set(&mut sim, "d", (0, 0));
        sim.add_clock("clk", 10, 5);
        // The register leaves x for 0: a negedge.
        sim.run_until(5).unwrap();
        assert_eq!(get(&mut sim, "pos"), 0u8.into());
        assert_eq!(get(&mut sim, "neg"), 1u8.into());
        for (time, (level, pos, neg)) in (15..).step_by(10).zip(WALK) {
            set(&mut sim, "d", level);
            sim.run_until(time).unwrap();
            assert_eq!(get(&mut sim, "pos"), pos.into(), "posedges at {time}");
            assert_eq!(get(&mut sim, "neg"), (neg + 1).into(), "negedges at {time}");
        }
    }
    on_every_backend!(Simulation::builder(REGISTER_CLOCK, "Top"), check);
}

/// A clock computed combinationally from a register, `e ? a & b : 'z`,
/// with the register written whole so the clock changes once per edge.
#[test]
fn combinational_clocks_follow_table_9_2() {
    const SOURCE: &str = r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    d  : input logic<3>,
    pos: output logic<8>,
    neg: output logic<8>,
) {
    var r: logic<3>;
    always_ff (clk) {
        r = d;
    }
    let cp: '_ clock         = if r[2] ? r[1] & r[0] : 1'bz;
    let cn: '_ clock_negedge = if r[2] ? r[1] & r[0] : 1'bz;
    var p : logic<8>;
    var n : logic<8>;
    always_ff (cp, rst) {
        if_reset {
            p = 0;
        } else {
            p = p + 8'd1;
        }
    }
    always_ff (cn, rst) {
        if_reset {
            n = 0;
        } else {
            n = n + 8'd1;
        }
    }
    assign pos = p;
    assign neg = n;
}
"#;
    /// `{e, a, b}` for each level of [`WALK`], as (value, mask).
    const INPUTS: [(u8, u8); 12] = [
        (0b111, 0),
        (0b100, 0),
        (0b110, 0b001), // 1 & z
        (0b110, 0),
        (0b011, 0), // e = 0
        (0b111, 0),
        (0b111, 0b010), // 1 & x
        (0b111, 0),
        (0b000, 0),     // e = 0
        (0b111, 0b100), // e = x
        (0b001, 0b010), // e = 0
        (0b101, 0),
    ];
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.schedule("rst", 0, 0).unwrap();
        sim.schedule("rst", 1, 1).unwrap();
        set(&mut sim, "d", (0b101, 0));
        sim.add_clock("clk", 10, 5);
        sim.run_until(5).unwrap();
        assert_eq!(get(&mut sim, "pos"), 0u8.into());
        assert_eq!(get(&mut sim, "neg"), 1u8.into());
        for (time, (input, (_, pos, neg))) in (15..).step_by(10).zip(INPUTS.into_iter().zip(WALK)) {
            set(&mut sim, "d", input);
            sim.run_until(time).unwrap();
            assert_eq!(get(&mut sim, "pos"), pos.into(), "posedges at {time}");
            assert_eq!(get(&mut sim, "neg"), (neg + 1).into(), "negedges at {time}");
        }
    }
    on_every_backend!(Simulation::builder(SOURCE, "Top"), check);
}

/// Asynchronous resets that are registers. An edge to x or z runs the
/// register's clocked branch (+1), an edge to the active level clears
/// it, and every clear follows an increment.
#[test]
fn register_async_resets_follow_table_9_2() {
    const SOURCE: &str = r#"
module Top (
    clk   : input '_ clock,
    k     : input '_ clock,
    clr   : input logic,
    dh    : input logic,
    dl    : input logic,
    hits_h: output logic<8>,
    hits_l: output logic<8>,
) {
    var qh: logic;
    var ql: logic;
    always_ff (clk) {
        qh = dh;
        ql = dl;
    }
    let rh: '_ reset_async_high = qh;
    let rl: '_ reset_async_low  = ql;
    var h : logic<8>;
    var l : logic<8>;
    always_ff (k, rh) {
        if_reset {
            h = 0;
        } else if clr {
            h = 8'h80;
        } else {
            h = h + 8'h01;
        }
    }
    always_ff (k, rl) {
        if_reset {
            l = 0;
        } else if clr {
            l = 8'h80;
        } else {
            l = l + 8'h01;
        }
    }
    assign hits_h = h;
    assign hits_l = l;
}
"#;
    /// `rh` levels 1, 0, x, 1, x, 0, z, 1, z, x, z, 0 (`rl` has 0 and 1
    /// swapped), and the counter after each change.
    const STEPS: [((u8, u8), u8); 12] = [
        ((1, 0), 0),
        ((0, 0), 0),
        ((1, 1), 1),
        ((1, 0), 0),
        ((1, 1), 0),
        ((0, 0), 0),
        ((0, 1), 1),
        ((1, 0), 0),
        ((0, 1), 0),
        ((1, 1), 0),
        ((0, 1), 0),
        ((0, 0), 0),
    ];
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        set(&mut sim, "dh", (0, 0));
        set(&mut sim, "dl", (1, 0));
        set(&mut sim, "clr", (1, 0));
        sim.schedule("k", 0, 0).unwrap();
        sim.add_clock("clk", 10, 5);
        // x -> 0 is no posedge of `rh`, x -> 1 no negedge of `rl`.
        sim.schedule("k", 7, 1).unwrap();
        sim.run_until(7).unwrap();
        set(&mut sim, "clr", (0, 0));
        assert_eq!(get(&mut sim, "hits_h"), 0x80u8.into());
        assert_eq!(get(&mut sim, "hits_l"), 0x80u8.into());
        for (time, ((value, mask), hits)) in (15..).step_by(10).zip(STEPS) {
            let low = if mask == 0 { value ^ 1 } else { value };
            set(&mut sim, "dh", (value, mask));
            set(&mut sim, "dl", (low, mask));
            sim.run_until(time).unwrap();
            assert_eq!(get(&mut sim, "hits_h"), hits.into(), "rh at {time}");
            assert_eq!(get(&mut sim, "hits_l"), hits.into(), "rl at {time}");
        }
    }
    on_every_backend!(Simulation::builder(SOURCE, "Top"), check);
}

/// The edge baseline of a clock left at x survives a checkpoint: the
/// following rise, x to 1, is a posedge.
#[test]
fn unknown_edge_baselines_survive_checkpoints() {
    fn check<B: SimBackend>(mut sim: Simulation<B>) {
        sim.schedule("rst", 0, 0).unwrap();
        sim.schedule("rst", 1, 1).unwrap();
        set(&mut sim, "d", (1, 1));
        sim.add_clock("clk", 10, 5);
        // The register leaves x for x: no edge.
        sim.run_until(5).unwrap();
        assert_eq!(get(&mut sim, "pos"), 0u8.into());
        let saved = sim.checkpoint().unwrap();
        let state = sim.save_state().unwrap();
        set(&mut sim, "d", (1, 0));
        sim.run_until(15).unwrap();
        assert_eq!(get(&mut sim, "pos"), 1u8.into());
        sim.restore(&saved).unwrap();
        set(&mut sim, "d", (1, 0));
        sim.run_until(15).unwrap();
        assert_eq!(get(&mut sim, "pos"), 1u8.into(), "after a restore");
        sim.load_state(&state).unwrap();
        set(&mut sim, "d", (1, 0));
        sim.run_until(15).unwrap();
        assert_eq!(get(&mut sim, "pos"), 1u8.into(), "after a state load");
    }
    on_every_backend!(Simulation::builder(REGISTER_CLOCK, "Top"), check);
}
