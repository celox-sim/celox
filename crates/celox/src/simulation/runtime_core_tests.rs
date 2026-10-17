//! The event-driven process phase of the timed scheduler on every backend:
//! waits, counted signals, the inactive and NBA regions, finish and
//! checkpoints. Designs are built with the frontend SDK, whose processes are
//! lowered by `ProcessKernelBuilder`.

use celox_frontend_sdk::{
    BinaryOp, Constant, Edge, EventEdge, EventTerm, ExprId, FrontendArtifact, ModuleBuilder,
    SignalId, Statement, UnaryOp, ValueType,
};

use super::Simulation;
use crate::{SimBackend, Simulator, SimulatorBuilder};
use celox_runtime::backend::EventHandle as _;

/// Run `$body` with `$sim` bound to a timed simulation of `$builder` on
/// every backend, including a native program image that went through its
/// container.
macro_rules! on_every_backend {
    ($builder:expr, $top:expr, |$sim:ident| $body:block) => {{
        fn run<B: SimBackend>(mut $sim: Simulation<B>) $body
        run(Simulation::new($builder.build_interpreter().unwrap()));
        run(Simulation::new($builder.build_cranelift().unwrap()));
        run(Simulation::new($builder.build_wasm().unwrap()));
        run(Simulation::new($builder.build_tiered().unwrap()));
        #[cfg(any(
            all(target_arch = "x86_64", not(feature = "arm64-codegen")),
            all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
        ))]
        {
            run(Simulation::new($builder.build_native().unwrap()));
            let image = $builder.compile_native().unwrap().into_program_image();
            let image = crate::NativeProgramImage::from_container_bytes(
                &image.to_container_bytes().unwrap(),
            )
            .unwrap();
            run(Simulation::new(
                Simulator::from_sources(Vec::new(), $top)
                    .emit_triggers()
                    .build_native_from_image(image)
                    .unwrap(),
            ));
        }
    }};
}

/// A flat SDK module under construction.
struct Design {
    module: ModuleBuilder,
    four_state: bool,
}

impl Design {
    fn new(name: &str, four_state: bool) -> Self {
        Self {
            module: ModuleBuilder::new(name).unwrap(),
            four_state,
        }
    }

    fn ty(&self, width: usize) -> ValueType {
        ValueType::new(width, false, self.four_state).unwrap()
    }

    /// An internal signal starting at `initial`.
    fn internal(&mut self, name: &str, width: usize, initial: u64) -> SignalId {
        let ty = self.ty(width);
        let signal = self.module.internal(name, ty).unwrap();
        self.init(signal, width, initial);
        signal
    }

    fn output(&mut self, name: &str, width: usize, initial: u64) -> SignalId {
        let ty = self.ty(width);
        let signal = self.module.output(name, ty).unwrap();
        self.init(signal, width, initial);
        signal
    }

    fn input(&mut self, name: &str, width: usize) -> SignalId {
        let ty = self.ty(width);
        self.module.input(name, ty).unwrap()
    }

    fn init(&mut self, signal: SignalId, width: usize, value: u64) {
        self.module
            .set_initial(signal, Constant::two_state(value, width).unwrap())
            .unwrap();
    }

    fn constant(&mut self, value: u64, width: usize) -> ExprId {
        self.module
            .constant(Constant::two_state(value, width).unwrap())
    }

    fn read(&mut self, signal: SignalId) -> ExprId {
        self.module.read(signal).unwrap()
    }

    fn width(&self, signal: SignalId) -> usize {
        self.module.whole(signal).unwrap().width()
    }

    /// `signal = value;`
    fn set(&mut self, signal: SignalId, value: u64) -> Statement {
        let width = self.width(signal);
        let value = self.constant(value, width);
        self.assign(signal, value)
    }

    fn assign(&mut self, signal: SignalId, value: ExprId) -> Statement {
        Statement::Assign {
            target: self.module.whole(signal).unwrap(),
            value,
        }
    }

    /// `signal <= value;`
    fn nonblocking(&mut self, signal: SignalId, value: ExprId) -> Statement {
        Statement::Nonblocking {
            target: self.module.whole(signal).unwrap(),
            value,
        }
    }

    fn plus_one(&mut self, signal: SignalId) -> ExprId {
        let width = self.width(signal);
        let current = self.read(signal);
        let one = self.constant(1, width);
        let ty = self.ty(width);
        self.module.binary(BinaryOp::Add, current, one, ty).unwrap()
    }

    /// `signal = signal + 1;`
    fn increment(&mut self, signal: SignalId) -> Statement {
        let next = self.plus_one(signal);
        self.assign(signal, next)
    }

    fn delay(&mut self, amount: u64) -> Statement {
        Statement::Delay {
            amount: self.constant(amount, 8),
        }
    }

    /// `#amount` with the IEEE inactive-region meaning of `#0`.
    fn inactive_delay(&mut self, amount: u64) -> Statement {
        Statement::InactiveDelay {
            amount: self.constant(amount, 8),
        }
    }

    /// `always_ff @(edge clock) target <= next;`
    fn register(&mut self, target: SignalId, next: ExprId, clock: SignalId, edge: Edge) {
        let target = self.module.whole(target).unwrap();
        self.module
            .register(target, next, clock, edge, None, None)
            .unwrap();
    }

    /// `always_ff @(posedge clock) target <= target + 1;`
    fn counter(&mut self, target: SignalId, clock: SignalId) {
        let next = self.plus_one(target);
        self.register(target, next, clock, Edge::Posedge);
    }

    fn finish(self) -> FrontendArtifact {
        self.module.finish()
    }
}

fn wait(signal: SignalId, edge: EventEdge) -> Statement {
    Statement::WaitEvent {
        terms: vec![EventTerm::new(signal, edge)],
    }
}

fn builder(artifact: &FrontendArtifact, four_state: bool) -> SimulatorBuilder<'static, Simulator> {
    Simulator::from_frontend(artifact.clone())
        .four_state(four_state)
        .emit_triggers()
}

fn value<B: SimBackend>(sim: &mut Simulation<B>, name: &str) -> u64 {
    let signal = sim.signal(name);
    let value = sim.get(signal);
    u64::try_from(value).unwrap()
}

/// A process writes 0→1→0 to `clk` without suspending: `@(posedge clk)`
/// and the flip-flops of `clk` see the pulse exactly once.
fn pulse(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Pulse", four_state);
    let clk = d.internal("clk", 1, 0);
    let waits = d.output("waits", 8, 0);
    let ffs = d.output("ffs", 8, 0);
    d.counter(ffs, clk);
    let body = vec![d.delay(1), d.set(clk, 1), d.set(clk, 0)];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(clk, EventEdge::Posedge), d.increment(waits)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn a_pulse_within_one_process_run_wakes_posedge_once() {
    for four_state in [false, true] {
        let artifact = pulse(four_state);
        on_every_backend!(builder(&artifact, four_state), "Pulse", |sim| {
            sim.run_until(0).unwrap();
            assert_eq!(value(&mut sim, "waits"), 0);
            sim.run_until(5).unwrap();
            assert_eq!(value(&mut sim, "clk"), 0);
            assert_eq!(value(&mut sim, "waits"), 1, "the wait sees the pulse once");
            assert_eq!(
                value(&mut sim, "ffs"),
                1,
                "the flip-flop sees the pulse once"
            );
        });
    }
}

/// `clk` rises twice in one time slot, separated by `#0`: the flip-flop
/// domain evaluates twice before the NBA region and both updates publish
/// in order.
fn repeated_edges(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Repeated", four_state);
    let clk = d.internal("clk", 1, 0);
    let data = d.internal("data", 8, 0);
    let q = d.output("q", 8, 0);
    let wakes = d.output("wakes", 8, 0);
    let read = d.read(data);
    d.register(q, read, clk, Edge::Posedge);
    let body = vec![
        d.delay(1),
        d.set(data, 1),
        d.set(clk, 1),
        d.inactive_delay(0),
        d.set(clk, 0),
        d.set(data, 2),
        d.inactive_delay(0),
        d.set(clk, 1),
        d.inactive_delay(0),
        d.set(clk, 0),
    ];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(clk, EventEdge::Posedge), d.increment(wakes)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn repeated_edges_in_one_slot_trigger_a_domain_repeatedly() {
    for four_state in [false, true] {
        let artifact = repeated_edges(four_state);
        on_every_backend!(builder(&artifact, four_state), "Repeated", |sim| {
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "wakes"), 2);
            // Both evaluations publish, the later one last.
            assert_eq!(value(&mut sim, "q"), 2);
        });
    }
}

/// Host writes of an input clock trigger its flip-flops and wake a
/// process waiting on it at the current time.
fn host_clock(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("HostClock", four_state);
    let clk = d.input("clk", 1);
    let ffs = d.output("ffs", 8, 0);
    let waits = d.output("waits", 8, 0);
    d.counter(ffs, clk);
    let body = vec![Statement::Forever {
        body: vec![wait(clk, EventEdge::Posedge), d.increment(waits)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn host_writes_trigger_registers_and_wake_waits() {
    for four_state in [false, true] {
        let artifact = host_clock(four_state);
        on_every_backend!(builder(&artifact, four_state), "HostClock", |sim| {
            let clk = sim.signal("clk");
            sim.modify(|io| io.set(clk, 0u8)).unwrap();
            sim.run_until(0).unwrap();
            for edge in 1..=3u64 {
                sim.modify(|io| io.set(clk, 1u8)).unwrap();
                sim.run_until(2 * edge - 1).unwrap();
                assert_eq!(value(&mut sim, "ffs"), edge);
                assert_eq!(value(&mut sim, "waits"), edge);
                sim.modify(|io| io.set(clk, 0u8)).unwrap();
                sim.run_until(2 * edge).unwrap();
                assert_eq!(value(&mut sim, "ffs"), edge);
            }
            // A pulse inside one host update wakes the wait as well.
            sim.modify(|io| {
                io.set(clk, 1u8);
                io.set(clk, 0u8);
            })
            .unwrap();
            sim.run_until(10).unwrap();
            assert_eq!(value(&mut sim, "waits"), 4);
        });
    }
}

/// `wait (ready)` blocks until another process sets `ready` and passes at
/// once when it already holds.
fn level(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Level", four_state);
    let ready = d.internal("ready", 1, 0);
    let first = d.output("first", 8, 0);
    let second = d.output("second", 8, 0);
    let condition = d.read(ready);
    let body = vec![Statement::WaitLevel { condition }, d.set(first, 1)];
    d.module.process(body).unwrap();
    let body = vec![d.delay(3), d.set(ready, 1)];
    d.module.process(body).unwrap();
    let body = vec![
        d.delay(5),
        Statement::WaitLevel { condition },
        d.set(second, 1),
    ];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn level_waits_block_until_their_condition_holds() {
    for four_state in [false, true] {
        let artifact = level(four_state);
        on_every_backend!(builder(&artifact, four_state), "Level", |sim| {
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "first"), 0);
            sim.run_until(3).unwrap();
            assert_eq!(value(&mut sim, "first"), 1);
            sim.run_until(4).unwrap();
            assert_eq!(value(&mut sim, "second"), 0);
            sim.run_until(5).unwrap();
            assert_eq!(value(&mut sim, "second"), 1);
            assert_eq!(sim.next_event_time(), None);
        });
    }
}

/// IEEE 1800-2023 4.5: the domains an edge triggers evaluate before an
/// inactive-region `#0` resumes, and nonblocking assignments publish only
/// after the inactive region; a zero `Delay` resumes after them.
fn zero_delays(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("ZeroDelays", four_state);
    let clk = d.internal("clk", 1, 0);
    let data = d.internal("data", 8, 3);
    let q = d.output("q", 8, 0);
    let x = d.internal("x", 8, 0);
    let y = d.internal("y", 8, 0);
    let inactive_seen = d.output("inactive_seen", 8, 9);
    let post_nba_seen = d.output("post_nba_seen", 8, 9);
    let read = d.read(data);
    d.register(q, read, clk, Edge::Posedge);
    let body = vec![
        d.delay(1),
        d.set(clk, 1),
        d.inactive_delay(0),
        d.set(data, 5),
    ];
    d.module.process(body).unwrap();
    let one = d.constant(1, 8);
    let read_x = d.read(x);
    let read_y = d.read(y);
    let body = vec![
        d.delay(1),
        d.nonblocking(x, one),
        d.inactive_delay(0),
        d.assign(inactive_seen, read_x),
    ];
    d.module.process(body).unwrap();
    let body = vec![
        d.delay(1),
        d.nonblocking(y, one),
        d.delay(0),
        d.assign(post_nba_seen, read_y),
    ];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn zero_delays_order_against_flip_flops_and_nonblocking_assignments() {
    for four_state in [false, true] {
        let artifact = zero_delays(four_state);
        on_every_backend!(builder(&artifact, four_state), "ZeroDelays", |sim| {
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "q"), 3, "the domain evaluated before #0");
            assert_eq!(
                value(&mut sim, "inactive_seen"),
                0,
                "#0 runs before the NBA region"
            );
            assert_eq!(
                value(&mut sim, "post_nba_seen"),
                1,
                "a zero Delay runs after it"
            );
            assert_eq!(value(&mut sim, "x"), 1);
        });
    }
}

/// Two nonblocking assignments to one watched destination in one slot
/// publish in order, each as its own update; so do three executions of a
/// single-site assignment.
fn nonblocking(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Nonblocking", four_state);
    let q = d.output("q", 8, 0);
    let r = d.output("r", 8, 0);
    let i = d.internal("i", 8, 0);
    let q_changes = d.output("q_changes", 8, 0);
    let r_changes = d.output("r_changes", 8, 0);
    let one = d.constant(1, 8);
    let zero = d.constant(0, 8);
    let body = vec![d.delay(1), d.nonblocking(q, one), d.nonblocking(q, zero)];
    d.module.process(body).unwrap();
    let read_i = d.read(i);
    let three = d.constant(3, 8);
    let ty = d.ty(1);
    let below = d
        .module
        .binary(BinaryOp::LessUnsigned, read_i, three, ty)
        .unwrap();
    let loop_body = vec![d.nonblocking(r, read_i), d.increment(i)];
    let body = vec![
        d.delay(2),
        Statement::While {
            condition: below,
            body: loop_body,
        },
    ];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(q, EventEdge::Change), d.increment(q_changes)],
    }];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(r, EventEdge::Change), d.increment(r_changes)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn nonblocking_assignments_publish_in_order() {
    for four_state in [false, true] {
        let artifact = nonblocking(four_state);
        on_every_backend!(builder(&artifact, four_state), "Nonblocking", |sim| {
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "q"), 0);
            assert_eq!(
                value(&mut sim, "q_changes"),
                1,
                "q was 1 between the updates"
            );
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "r"), 2, "the last assignment wins");
            assert_eq!(value(&mut sim, "r_changes"), 1);
        });
    }
}

/// A process writes `a`, finishes and would write it again; a register
/// counts clock edges. Nothing changes after the finish.
fn finishing(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Finishing", four_state);
    let clk = d.input("clk", 1);
    let a = d.output("a", 8, 0);
    let edges = d.output("edges", 8, 0);
    d.counter(edges, clk);
    let body = vec![d.delay(4), d.set(a, 1), Statement::Finish, d.set(a, 2)];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn finish_freezes_the_simulation() {
    for four_state in [false, true] {
        let artifact = finishing(four_state);
        on_every_backend!(builder(&artifact, four_state), "Finishing", |sim| {
            let clk = sim.signal("clk");
            sim.modify(|io| io.set(clk, 0u8)).unwrap();
            sim.add_clock("clk", 2, 1);
            sim.run_until(20).unwrap();
            assert!(sim.is_finished());
            assert_eq!(sim.time(), 4);
            assert_eq!(value(&mut sim, "a"), 1);
            // Edges at 1 and 3; the one at 5 is after the finish.
            assert_eq!(value(&mut sim, "edges"), 2);
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, 7u8)).unwrap();
            sim.eval_comb().unwrap();
            assert_eq!(value(&mut sim, "a"), 1, "host writes are ignored");
            assert_eq!(sim.step().unwrap(), None);
            assert_eq!(sim.next_event_time(), None);
        });
    }
}

/// A process waiting on `go` survives a checkpoint: restoring returns it to
/// waiting, and the next edge wakes it again.
fn waiting(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Waiting", four_state);
    let go = d.input("go", 1);
    let wakes = d.output("wakes", 8, 0);
    let body = vec![Statement::Forever {
        body: vec![wait(go, EventEdge::Posedge), d.increment(wakes)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn checkpoints_restore_a_waiting_process() {
    for four_state in [false, true] {
        let artifact = waiting(four_state);
        on_every_backend!(builder(&artifact, four_state), "Waiting", |sim| {
            let go = sim.signal("go");
            sim.modify(|io| io.set(go, 0u8)).unwrap();
            sim.run_until(1).unwrap();
            let saved = sim.checkpoint().unwrap();
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "wakes"), 1);
            sim.restore(&saved).unwrap();
            assert_eq!(sim.time(), 1);
            assert_eq!(value(&mut sim, "wakes"), 0);
            sim.run_until(3).unwrap();
            assert_eq!(value(&mut sim, "wakes"), 0, "restored waiting, not woken");
            sim.modify(|io| io.set(go, 1u8)).unwrap();
            sim.run_until(4).unwrap();
            assert_eq!(value(&mut sim, "wakes"), 1);
        });
    }
}

#[test]
fn state_files_carry_waiting_processes_across_backends() {
    // Saved on the interpreter, loaded into every backend.
    let artifact = waiting(true);
    let mut source = Simulation::new(builder(&artifact, true).build_interpreter().unwrap());
    let go = source.signal("go");
    source.modify(|io| io.set(go, 0u8)).unwrap();
    source.run_until(1).unwrap();
    source.modify(|io| io.set(go, 1u8)).unwrap();
    source.run_until(2).unwrap();
    source.modify(|io| io.set(go, 0u8)).unwrap();
    source.run_until(3).unwrap();
    let file = source.save_state().unwrap();
    let mut bytes = Vec::new();
    file.write_to(&mut bytes).unwrap();
    let file = celox_runtime::StateFile::read_from(bytes.as_slice()).unwrap();
    STATE.with(|state| *state.borrow_mut() = Some(file));
    on_every_backend!(builder(&artifact, true), "Waiting", |sim| {
        let file = STATE.with(|state| state.borrow().clone().unwrap());
        sim.load_state(&file).unwrap();
        assert_eq!(sim.time(), 3);
        assert_eq!(value(&mut sim, "wakes"), 1);
        sim.run_until(4).unwrap();
        assert_eq!(value(&mut sim, "wakes"), 1);
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 1u8)).unwrap();
        sim.run_until(5).unwrap();
        assert_eq!(
            value(&mut sim, "wakes"),
            2,
            "the loaded process still waits"
        );
    });
}

thread_local! {
    static STATE: std::cell::RefCell<Option<celox_runtime::StateFile>> =
        const { std::cell::RefCell::new(None) };
}

/// IEEE 1800-2023 4.5: a process woken by `@(posedge clk)` runs in the
/// active region of that edge, before the edge's NBA region, so it reads the
/// flip-flop output from before the edge.
fn edge_order(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("EdgeOrder", four_state);
    let clk = d.input("clk", 1);
    let q = d.output("q", 8, 0);
    let seen = d.output("seen", 8, 99);
    d.counter(q, clk);
    let read = d.read(q);
    let body = vec![Statement::Forever {
        body: vec![wait(clk, EventEdge::Posedge), d.assign(seen, read)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn edge_woken_processes_read_flip_flops_before_the_edge_publishes() {
    for four_state in [false, true] {
        let artifact = edge_order(four_state);
        on_every_backend!(builder(&artifact, four_state), "EdgeOrder", |sim| {
            let clk = sim.signal("clk");
            sim.modify(|io| io.set(clk, 0u8)).unwrap();
            sim.add_clock("clk", 10, 5);
            for edge in 1..=4u64 {
                sim.run_until(10 * edge - 5).unwrap();
                assert_eq!(value(&mut sim, "q"), edge);
                assert_eq!(value(&mut sim, "seen"), edge - 1);
            }
        });
    }
}

/// A combinational process writes `y = 0; if (a) y = 1;`. With `a` steady,
/// settling again and reading does not re-run it, so `@(posedge y)` and the
/// flip-flops of `y` are not woken; a change of another input re-runs it and
/// the 1→0→1 pulse is an edge (IEEE 1800-2023 9.2.2.2).
fn comb_pulse(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("CombPulse", four_state);
    let a = d.input("a", 1);
    let b = d.input("b", 1);
    let y = d.output("y", 1, 0);
    let z = d.output("z", 1, 0);
    let ffs = d.output("ffs", 8, 0);
    let waits = d.output("waits", 8, 0);
    let read_a = d.read(a);
    let read_b = d.read(b);
    let body = vec![
        d.set(y, 0),
        Statement::If {
            condition: read_a,
            then_body: vec![d.set(y, 1)],
            else_body: Vec::new(),
        },
        d.assign(z, read_b),
    ];
    d.module.comb_process(body).unwrap();
    d.counter(ffs, y);
    let body = vec![Statement::Forever {
        body: vec![wait(y, EventEdge::Posedge), d.increment(waits)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn comb_processes_pulse_only_when_an_input_changes() {
    for four_state in [false, true] {
        let artifact = comb_pulse(four_state);
        on_every_backend!(builder(&artifact, four_state), "CombPulse", |sim| {
            let a = sim.signal("a");
            let b = sim.signal("b");
            sim.modify(|io| {
                io.set(a, 0u8);
                io.set(b, 0u8);
            })
            .unwrap();
            sim.run_until(0).unwrap();
            sim.modify(|io| io.set(a, 1u8)).unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "y"), 1);
            assert_eq!(value(&mut sim, "ffs"), 1);
            assert_eq!(value(&mut sim, "waits"), 1);
            // Steady inputs: settles and reads do not re-run the process.
            for time in 2..6 {
                sim.eval_comb().unwrap();
                assert_eq!(value(&mut sim, "y"), 1);
                sim.run_until(time).unwrap();
            }
            assert_eq!(value(&mut sim, "ffs"), 1, "no spurious edge");
            assert_eq!(value(&mut sim, "waits"), 1, "no spurious wakeup");
            sim.modify(|io| io.set(b, 1u8)).unwrap();
            sim.run_until(6).unwrap();
            assert_eq!(value(&mut sim, "z"), 1);
            assert_eq!(value(&mut sim, "ffs"), 2, "the re-run pulses y");
            assert_eq!(value(&mut sim, "waits"), 2);
        });
    }
}

#[cfg(feature = "systemverilog")]
mod systemverilog {
    use super::*;

    /// An imperative SystemVerilog combinational unit (one calling
    /// `$urandom`) keeps its default-then-override writes; it is gated on
    /// its inputs, so a steady input never pulses `y`.
    const SOURCE: &str = r#"
module Top(input logic a, b, output logic y, output logic [31:0] r, output logic [7:0] n);
    always_comb begin
        y = 1'b0;
        if (a) y = 1'b1;
        r = b ? $urandom(7) : 32'd0;
    end
    initial n = 8'd0;
    always_ff @(posedge y) n <= n + 8'd1;
endmodule
"#;

    #[test]
    fn imperative_comb_units_pulse_only_when_an_input_changes() {
        for four_state in [false, true] {
            let build = || {
                Simulator::from_sv_sources(
                    vec![(SOURCE, std::path::Path::new("comb_pulse.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .emit_triggers()
            };
            on_every_backend!(build(), "Top", |sim| {
                let a = sim.signal("a");
                let b = sim.signal("b");
                sim.modify(|io| {
                    io.set(a, 0u8);
                    io.set(b, 0u8);
                })
                .unwrap();
                sim.run_until(0).unwrap();
                sim.modify(|io| io.set(a, 1u8)).unwrap();
                sim.run_until(1).unwrap();
                assert_eq!(value(&mut sim, "n"), 1);
                for time in 2..6 {
                    sim.eval_comb().unwrap();
                    assert_eq!(value(&mut sim, "y"), 1);
                    sim.run_until(time).unwrap();
                }
                assert_eq!(value(&mut sim, "n"), 1, "no spurious edge");
                sim.modify(|io| io.set(b, 1u8)).unwrap();
                sim.run_until(6).unwrap();
                assert_eq!(value(&mut sim, "n"), 2, "the re-run pulses y");
            });
        }
    }
}

/// Records the entry points a timed step calls on a simulator.
struct Recording<'a, B: SimBackend> {
    sim: &'a mut Simulator<B>,
    calls: Vec<&'static str>,
}

impl<B: SimBackend> celox_runtime::SimulationExecutor for Recording<'_, B> {
    type Backend = B;

    fn backend(&self) -> &B {
        &self.sim.backend
    }

    fn backend_mut(&mut self) -> &mut B {
        &mut self.sim.backend
    }

    fn eval_comb(&mut self) -> Result<(), celox_runtime::Halt> {
        self.calls.push("comb");
        celox_runtime::SimulationExecutor::eval_comb(self.sim)
    }

    fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), celox_runtime::Halt> {
        self.calls.push("eval_apply");
        celox_runtime::SimulationExecutor::eval_apply_ff_at(self.sim, event)
    }

    fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), celox_runtime::Halt> {
        self.calls.push("eval_only");
        celox_runtime::SimulationExecutor::eval_only_ff_at(self.sim, event)
    }

    fn apply_ff_at(&mut self, event: B::Event) -> Result<(), celox_runtime::Halt> {
        self.calls.push("apply");
        celox_runtime::SimulationExecutor::apply_ff_at(self.sim, event)
    }

    fn run_process(&mut self, index: usize) -> Result<(), celox_runtime::Halt> {
        self.calls.push("process");
        celox_runtime::SimulationExecutor::run_process(self.sim, index)
    }
}

/// Two registers on `clk`, a combinational output, and with `testbench`
/// a process that counts rising edges of `clk` and one that sleeps.
fn rtl(testbench: bool) -> FrontendArtifact {
    let mut d = Design::new("Rtl", false);
    let clk = d.input("clk", 1);
    let a = d.output("a", 8, 0);
    let b = d.output("b", 8, 0);
    let sum = d.output("sum", 8, 0);
    d.counter(a, clk);
    let read_a = d.read(a);
    d.register(b, read_a, clk, Edge::Posedge);
    let read_b = d.read(b);
    let ty = d.ty(8);
    let added = d.module.binary(BinaryOp::Add, read_a, read_b, ty).unwrap();
    let target = d.module.whole(sum).unwrap();
    d.module.assign(target, added).unwrap();
    if testbench {
        let edges = d.output("edges", 8, 0);
        let body = vec![Statement::Forever {
            body: vec![wait(clk, EventEdge::Posedge), d.increment(edges)],
        }];
        d.module.process(body).unwrap();
        let body = vec![d.inactive_delay(200)];
        d.module.process(body).unwrap();
    }
    d.finish()
}

fn recorded_steps<B: SimBackend>(mut sim: Simulation<B>, steps: usize) -> Vec<Vec<&'static str>> {
    let clk = sim.signal("clk");
    sim.modify(|io| io.set(clk, 0u8)).unwrap();
    sim.add_clock("clk", 10, 5);
    sim.run_until(1).unwrap();
    (0..steps)
        .map(|_| {
            let mut recording = Recording {
                sim: &mut sim.simulator,
                calls: Vec::new(),
            };
            sim.state.step(&mut recording).unwrap();
            recording.calls
        })
        .collect()
}

#[test]
fn processes_leave_the_flip_flop_path_of_a_time_slot_unchanged() {
    let check = |threads: usize| {
        let build = |testbench| {
            builder(&rtl(testbench), false)
                .threads(threads)
                .build_native()
                .unwrap()
        };
        let plain = recorded_steps(Simulation::new(build(false)), 4);
        let timed = recorded_steps(Simulation::new(build(true)), 4);
        // Rising and falling edges of a plain design: one settle, the fused
        // evaluate-and-apply of the domain, and the settle after it.
        assert_eq!(plain[0], ["comb", "eval_apply", "comb"]);
        assert_eq!(plain[1], ["comb"]);
        // With a waiting testbench process, the rising edge additionally runs
        // the woken process in front of the same flip-flop path; no extra
        // settle or split evaluation.
        for (plain, timed) in plain.iter().zip(&timed).step_by(2) {
            let mut expected = vec!["process"];
            expected.extend(plain);
            assert_eq!(timed, &expected);
        }
        for (plain, timed) in plain.iter().zip(&timed).skip(1).step_by(2) {
            assert_eq!(timed, plain, "a falling edge wakes nothing");
        }
    };
    #[cfg(any(
        all(target_arch = "x86_64", not(feature = "arm64-codegen")),
        all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
    ))]
    for threads in [1, 4] {
        check(threads);
    }
}

/// A domain evaluated twice before the NBA region publishes 1 and then 0
/// to `q`: a process waiting for any change of `q` sees the pulse although
/// `q` ends at its old value.
fn republished(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Republished", four_state);
    let clk = d.internal("clk", 1, 0);
    let data = d.internal("data", 1, 0);
    let q = d.output("q", 1, 0);
    let changes = d.output("changes", 8, 0);
    let read = d.read(data);
    d.register(q, read, clk, Edge::Posedge);
    let body = vec![
        d.delay(1),
        d.set(data, 1),
        d.set(clk, 1),
        d.inactive_delay(0),
        d.set(clk, 0),
        d.set(data, 0),
        d.inactive_delay(0),
        d.set(clk, 1),
    ];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(q, EventEdge::Change), d.increment(changes)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn every_publication_of_a_domain_is_an_update() {
    for four_state in [false, true] {
        let artifact = republished(four_state);
        on_every_backend!(builder(&artifact, four_state), "Republished", |sim| {
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "q"), 0);
            assert_eq!(
                value(&mut sim, "changes"),
                1,
                "q was 1 between the publications"
            );
        });
    }
}

/// A queued nonblocking assignment returns to the runtime between the two
/// writes of `clk`; the pulse is still an edge for waits and registers.
fn pulse_around_nba(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("PulseAroundNba", four_state);
    let clk = d.internal("clk", 1, 0);
    let r = d.internal("r", 8, 0);
    let waits = d.output("waits", 8, 0);
    let ffs = d.output("ffs", 8, 0);
    d.counter(ffs, clk);
    let one = d.constant(1, 8);
    let two = d.constant(2, 8);
    // Two sites assign `r`, so both go through the queue.
    let body = vec![
        d.delay(1),
        d.set(clk, 1),
        d.nonblocking(r, one),
        d.set(clk, 0),
        d.nonblocking(r, two),
    ];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(clk, EventEdge::Posedge), d.increment(waits)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn a_pulse_around_a_queued_nonblocking_assignment_is_an_edge() {
    for four_state in [false, true] {
        let artifact = pulse_around_nba(four_state);
        on_every_backend!(builder(&artifact, four_state), "PulseAroundNba", |sim| {
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "r"), 2);
            assert_eq!(value(&mut sim, "waits"), 1);
            assert_eq!(value(&mut sim, "ffs"), 1);
        });
    }
}

#[test]
fn host_write_pulses_survive_checkpoints_and_state_files() {
    // A pulse written by the host and saved before the step that shows it
    // to the waits.
    let artifact = waiting(false);
    on_every_backend!(builder(&artifact, false), "Waiting", |sim| {
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 0u8)).unwrap();
        sim.run_until(1).unwrap();
        sim.modify(|io| {
            io.set(go, 1u8);
            io.set(go, 0u8);
        })
        .unwrap();
        let saved = sim.checkpoint().unwrap();
        let file = sim.save_state().unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(value(&mut sim, "wakes"), 1);
        sim.restore(&saved).unwrap();
        sim.run_until(2).unwrap();
        assert_eq!(
            value(&mut sim, "wakes"),
            1,
            "restored with the pending pulse"
        );
        sim.load_state(&file).unwrap();
        assert_eq!(value(&mut sim, "wakes"), 0);
        sim.run_until(2).unwrap();
        assert_eq!(value(&mut sim, "wakes"), 1, "loaded with the pending pulse");
    });
}

#[test]
fn loading_a_finished_state_freezes_the_simulation() {
    let artifact = finishing(false);
    on_every_backend!(builder(&artifact, false), "Finishing", |sim| {
        let clk = sim.signal("clk");
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        let start = sim.save_state().unwrap();
        sim.run_until(10).unwrap();
        assert!(sim.is_finished());
        let finished = sim.save_state().unwrap();
        sim.load_state(&start).unwrap();
        assert!(!sim.is_finished());
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 5u8)).unwrap();
        assert_eq!(
            value(&mut sim, "a"),
            5,
            "an unfinished state accepts writes"
        );
        sim.load_state(&finished).unwrap();
        assert!(sim.is_finished());
        sim.modify(|io| io.set(a, 7u8)).unwrap();
        assert_eq!(value(&mut sim, "a"), 1, "a finished state is frozen");
    });
}

#[cfg(feature = "systemverilog")]
#[test]
fn finish_during_a_direct_settle_ends_the_simulation() {
    const SOURCE: &str = r#"
module Top(input logic go, output logic [7:0] y);
    always_comb begin
        y = go ? 8'd1 : 8'd0;
        if (go) $finish;
    end
endmodule
"#;
    let build = || {
        Simulator::from_sv_sources(vec![(SOURCE, std::path::Path::new("finish.sv"))], "Top")
            .emit_triggers()
    };
    on_every_backend!(build(), "Top", |sim| {
        let go = sim.signal("go");
        sim.modify(|io| io.set(go, 0u8)).unwrap();
        sim.eval_comb().unwrap();
        assert!(!sim.is_finished());
        sim.modify(|io| io.set(go, 1u8)).unwrap();
        sim.eval_comb().unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.next_event_time(), None);
        sim.modify(|io| io.set(go, 0u8)).unwrap();
        assert_eq!(value(&mut sim, "y"), 1, "frozen at the finish");
    });
}

/// Two outputs of a domain evaluated twice publish 1 then 0 together; a
/// domain on the negative edge of the first sees its pulse although the
/// transitions of the two signals interleave.
fn interleaved(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("Interleaved", four_state);
    let clk = d.internal("clk", 1, 0);
    let data = d.internal("data", 1, 0);
    let qa = d.output("qa", 1, 0);
    let qb = d.output("qb", 1, 0);
    let falls = d.output("falls", 8, 0);
    let rises = d.output("rises", 8, 0);
    for q in [qa, qb] {
        let read = d.read(data);
        d.register(q, read, clk, Edge::Posedge);
    }
    let next = d.plus_one(falls);
    d.register(falls, next, qa, Edge::Negedge);
    d.counter(rises, qb);
    let body = vec![
        d.delay(1),
        d.set(data, 1),
        d.set(clk, 1),
        d.inactive_delay(0),
        d.set(clk, 0),
        d.set(data, 0),
        d.inactive_delay(0),
        d.set(clk, 1),
    ];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn interleaved_publications_keep_each_signals_edges() {
    for four_state in [false, true] {
        let artifact = interleaved(four_state);
        on_every_backend!(builder(&artifact, four_state), "Interleaved", |sim| {
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "qa"), 0);
            assert_eq!(value(&mut sim, "rises"), 1);
            assert_eq!(value(&mut sim, "falls"), 1, "qa fell after its pulse");
        });
    }
}

#[test]
fn state_files_reject_queued_assignments_outside_their_target() {
    let artifact = nonblocking(false);
    let mut sim = Simulation::new(builder(&artifact, false).build_interpreter().unwrap());
    sim.run_until(0).unwrap();
    let mut file = sim.save_state().unwrap();
    file.schedule
        .as_mut()
        .unwrap()
        .processes
        .nba_queue
        .push(celox_runtime::NbaRecord {
            target: "q".into(),
            lsb: 4,
            width: u64::MAX,
            value: vec![1],
            mask: Vec::new(),
        });
    assert!(matches!(
        sim.load_state(&file),
        Err(crate::StateError::Mismatch(_))
    ));
}

/// Two processes write a high clock low and then high again in one slot:
/// the second write is a positive edge although the clock ends where it
/// started.
fn low_then_high(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("LowThenHigh", four_state);
    let clk = d.internal("clk", 1, 1);
    let ffs = d.output("ffs", 8, 0);
    d.counter(ffs, clk);
    let body = vec![d.delay(1), d.set(clk, 0)];
    d.module.process(body).unwrap();
    let body = vec![d.delay(1), d.set(clk, 1)];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn a_signal_written_again_compares_with_its_preceding_value() {
    for four_state in [false, true] {
        let artifact = low_then_high(four_state);
        on_every_backend!(builder(&artifact, four_state), "LowThenHigh", |sim| {
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "clk"), 1);
            assert_eq!(value(&mut sim, "ffs"), 1);
        });
    }
}

/// `clk <= 1; clk <= 0;` from a low clock pulses it in the NBA region.
fn nba_pulse(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("NbaPulse", four_state);
    let clk = d.internal("clk", 1, 0);
    let ffs = d.output("ffs", 8, 0);
    d.counter(ffs, clk);
    let one = d.constant(1, 1);
    let zero = d.constant(0, 1);
    let body = vec![
        d.delay(1),
        d.nonblocking(clk, one),
        d.nonblocking(clk, zero),
    ];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn queued_nonblocking_pulses_trigger_flip_flops() {
    for four_state in [false, true] {
        let artifact = nba_pulse(four_state);
        on_every_backend!(builder(&artifact, four_state), "NbaPulse", |sim| {
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "clk"), 0);
            assert_eq!(value(&mut sim, "ffs"), 1);
        });
    }
}

/// A combinational process overwrites part of what it assigned.
fn comb_override(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("CombOverride", four_state);
    let a = d.input("a", 8);
    let low = d.input("low", 1);
    let y = d.output("y", 8, 0);
    let read_a = d.read(a);
    let read_low = d.read(low);
    let first = d.module.slice(y, 0, 1).unwrap();
    let body = vec![
        d.assign(y, read_a),
        Statement::Assign {
            target: first,
            value: read_low,
        },
    ];
    d.module.comb_process(body).unwrap();
    d.finish()
}

#[test]
fn comb_processes_may_overwrite_their_own_targets() {
    for four_state in [false, true] {
        let artifact = comb_override(four_state);
        on_every_backend!(builder(&artifact, four_state), "CombOverride", |sim| {
            let a = sim.signal("a");
            let low = sim.signal("low");
            sim.modify(|io| {
                io.set(a, 0xf0u8);
                io.set(low, 1u8);
            })
            .unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "y"), 0xf1);
        });
    }
}

#[test]
fn the_waveform_records_the_finish() {
    let artifact = finishing(false);
    let path = std::env::temp_dir().join(format!("celox-finish-{}.vcd", std::process::id()));
    let mut sim = Simulation::new(
        builder(&artifact, false)
            .vcd(&path)
            .build_interpreter()
            .unwrap(),
    );
    let clk = sim.signal("clk");
    sim.modify(|io| io.set(clk, 0u8)).unwrap();
    sim.add_clock("clk", 2, 1);
    sim.run_until(20).unwrap();
    assert!(sim.is_finished());
    sim.flush_vcd().unwrap();
    let vcd = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).ok();
    let last = vcd.lines().rfind(|line| line.starts_with('#'));
    assert_eq!(last, Some("#4"), "{vcd}");
}

#[test]
fn state_files_reject_baselines_of_the_wrong_kind() {
    let artifact = waiting(false);
    let mut sim = Simulation::new(builder(&artifact, false).build_interpreter().unwrap());
    let go = sim.signal("go");
    sim.modify(|io| io.set(go, 0u8)).unwrap();
    sim.run_until(1).unwrap();
    let mut file = sim.save_state().unwrap();
    let waiters = &mut file.schedule.as_mut().unwrap().processes.waiters;
    assert_eq!(waiters.len(), 1);
    // `go` is not counted: its wait compares values.
    waiters[0].baselines = vec![celox_runtime::BaselineRecord::Counter {
        seen: 0,
        value: vec![0],
        mask: vec![0],
    }];
    assert!(matches!(
        sim.load_state(&file),
        Err(crate::StateError::Mismatch(_))
    ));
}

#[cfg(feature = "systemverilog")]
#[test]
fn saving_state_after_a_finishing_write_records_the_finish() {
    const SOURCE: &str = r#"
module Top(input logic go, output logic [7:0] y);
    always_comb begin
        y = go ? 8'd1 : 8'd0;
        if (go) $finish;
    end
endmodule
"#;
    let mut sim = Simulation::new(
        Simulator::from_sv_sources(vec![(SOURCE, std::path::Path::new("finish.sv"))], "Top")
            .emit_triggers()
            .build_interpreter()
            .unwrap(),
    );
    let go = sim.signal("go");
    sim.modify(|io| io.set(go, 0u8)).unwrap();
    sim.run_until(0).unwrap();
    sim.modify(|io| io.set(go, 1u8)).unwrap();
    let file = sim.save_state().unwrap();
    assert!(sim.is_finished());
    assert!(file.schedule.unwrap().processes.finished);
}

/// `y = a & b` is read only by a wait: dead store elimination keeps its
/// store, so the wait still sees `y` rise.
fn wait_on_comb(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("WaitOnComb", four_state);
    let a = d.input("a", 1);
    let b = d.input("b", 1);
    let ty = d.ty(1);
    let y = d.module.internal("y", ty).unwrap();
    let hits = d.output("hits", 8, 0);
    let (read_a, read_b) = (d.read(a), d.read(b));
    let and = d.module.binary(BinaryOp::And, read_a, read_b, ty).unwrap();
    let target = d.module.whole(y).unwrap();
    d.module.assign(target, and).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(y, EventEdge::Posedge), d.increment(hits)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn dead_store_elimination_keeps_signals_a_wait_watches() {
    for four_state in [false, true] {
        let artifact = wait_on_comb(four_state);
        let optimized = || builder(&artifact, four_state).opt_level(crate::OptLevel::O2);
        on_every_backend!(optimized(), "WaitOnComb", |sim| {
            let (a, b) = (sim.signal("a"), sim.signal("b"));
            sim.modify(|io| {
                io.set(a, 0u8);
                io.set(b, 1u8);
            })
            .unwrap();
            sim.run_until(1).unwrap();
            assert_eq!(value(&mut sim, "hits"), 0);
            for (time, hits) in [(2, 1), (4, 2)] {
                sim.modify(|io| io.set(a, 1u8)).unwrap();
                sim.run_until(time).unwrap();
                assert_eq!(value(&mut sim, "hits"), hits);
                sim.modify(|io| io.set(a, 0u8)).unwrap();
                sim.run_until(time + 1).unwrap();
            }
        });
    }
}

/// A scheduled `clk` edge triggers one domain, and the process it wakes
/// pulses `p` with two nonblocking assignments: the domain of `p` sees the
/// pulse although only one domain evaluated in front of it.
fn nba_pulse_after_one_domain(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("NbaPulseOneDomain", four_state);
    let clk = d.input("clk", 1);
    let p = d.internal("p", 1, 0);
    let a_count = d.output("a_count", 8, 0);
    let b_count = d.output("b_count", 8, 0);
    d.counter(a_count, clk);
    d.counter(b_count, p);
    let one = d.constant(1, 1);
    let zero = d.constant(0, 1);
    let body = vec![Statement::Forever {
        body: vec![
            wait(clk, EventEdge::Posedge),
            d.nonblocking(p, one),
            d.nonblocking(p, zero),
        ],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn nonblocking_pulses_after_a_single_domain_trigger_their_domain() {
    for four_state in [false, true] {
        let artifact = nba_pulse_after_one_domain(four_state);
        on_every_backend!(builder(&artifact, four_state), "NbaPulseOneDomain", |sim| {
            sim.schedule("clk", 0, 0).unwrap();
            sim.run_until(0).unwrap();
            for edge in 1..=3u64 {
                sim.schedule("clk", 2 * edge - 1, 1).unwrap();
                sim.schedule("clk", 2 * edge, 0).unwrap();
                sim.run_until(2 * edge).unwrap();
                assert_eq!(value(&mut sim, "a_count"), edge);
                assert_eq!(value(&mut sim, "p"), 0);
                assert_eq!(value(&mut sim, "b_count"), edge, "edge {edge}");
            }
        });
    }
}

/// `s` falls in one wave of the active region and rises again in the next,
/// written by the process the fall woke: the domain of `s` sees the rise.
fn pulse_across_waves(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("PulseAcrossWaves", four_state);
    let s = d.internal("s", 1, 1);
    let rises = d.output("rises", 8, 0);
    d.counter(rises, s);
    let body = vec![d.delay(1), d.set(s, 0)];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(s, EventEdge::Negedge), d.set(s, 1)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn a_pulse_across_two_waves_of_a_slot_is_an_edge() {
    for four_state in [false, true] {
        let artifact = pulse_across_waves(four_state);
        on_every_backend!(builder(&artifact, four_state), "PulseAcrossWaves", |sim| {
            sim.run_until(0).unwrap();
            assert_eq!(value(&mut sim, "rises"), 0);
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "s"), 1);
            assert_eq!(value(&mut sim, "rises"), 1);
        });
    }
}

/// Processes that keep resuming at one time, through `#0`, by waking each
/// other or in a loop without a timing control, exhaust the slot's budget
/// instead of hanging a step.
fn zero_time_loops(four_state: bool) -> [FrontendArtifact; 3] {
    let mut d = Design::new("InactiveLoop", four_state);
    let body = vec![
        d.delay(1),
        Statement::Forever {
            body: vec![d.inactive_delay(0)],
        },
    ];
    d.module.process(body).unwrap();
    let inactive = d.finish();

    let mut d = Design::new("PingPong", four_state);
    let ping = d.internal("ping", 1, 0);
    let pong = d.internal("pong", 1, 0);
    let (read_ping, read_pong) = (d.read(ping), d.read(pong));
    let ty = d.ty(1);
    let not_ping = d.module.unary(UnaryOp::BitNot, read_ping, ty).unwrap();
    let not_pong = d.module.unary(UnaryOp::BitNot, read_pong, ty).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(ping, EventEdge::Change), d.assign(pong, not_pong)],
    }];
    d.module.process(body).unwrap();
    let body = vec![Statement::Forever {
        body: vec![wait(pong, EventEdge::Change), d.assign(ping, not_ping)],
    }];
    d.module.process(body).unwrap();
    let body = vec![d.delay(1), d.set(ping, 1)];
    d.module.process(body).unwrap();
    let ping_pong = d.finish();

    let mut d = Design::new("Spin", four_state);
    let count = d.output("count", 8, 0);
    let body = vec![
        d.delay(1),
        Statement::Forever {
            body: vec![d.increment(count)],
        },
    ];
    d.module.process(body).unwrap();
    [inactive, ping_pong, d.finish()]
}

#[test]
fn zero_time_loops_exhaust_the_slot_budget() {
    for four_state in [false, true] {
        for (artifact, top) in
            zero_time_loops(four_state)
                .into_iter()
                .zip(["InactiveLoop", "PingPong", "Spin"])
        {
            on_every_backend!(builder(&artifact, four_state), top, |sim| {
                sim.state.set_slot_budgets(256, 2);
                sim.run_until(0).unwrap();
                let error = sim.run_until(5).unwrap_err();
                assert!(
                    matches!(
                        &error,
                        crate::RuntimeErrorCode::Runtime { message, .. }
                            if message.contains("does not settle")
                                || message.contains("does not suspend")
                    ),
                    "{error}"
                );
                assert_eq!(sim.time(), 1);
            });
        }
    }
}

/// A process waiting on an input wakes at the time the host writes the
/// input through the raw memory view, as the TypeScript DUT does, once
/// the host reports the write.
fn raw_go(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("RawGo", four_state);
    let go = d.input("go", 1);
    let done = d.output("done", 8, 0);
    let body = vec![wait(go, EventEdge::Posedge), d.set(done, 1)];
    d.module.process(body).unwrap();
    d.finish()
}

/// Write `value` to `signal` through the raw memory view.
fn write_raw<B: SimBackend>(sim: &mut Simulation<B>, signal: crate::SignalRef, value: u8) {
    let four_state = sim.layout().four_state && signal.is_4state;
    let bytes = signal.width.div_ceil(8);
    let (pointer, size) = sim.memory_as_mut_ptr();
    assert!(signal.offset + 2 * bytes <= size);
    // Safety: the view holds `size` writable bytes.
    unsafe {
        *pointer.add(signal.offset) = value;
        if four_state {
            *pointer.add(signal.offset + bytes) = 0;
        }
    }
}

#[test]
fn raw_host_writes_wake_waits_at_the_current_time() {
    for four_state in [false, true] {
        let artifact = raw_go(four_state);
        on_every_backend!(builder(&artifact, four_state), "RawGo", |sim| {
            let go = sim.signal("go");
            write_raw(&mut sim, go, 0);
            sim.note_raw_writes();
            sim.run_until(3).unwrap();
            assert_eq!(value(&mut sim, "done"), 0);
            write_raw(&mut sim, go, 1);
            sim.note_raw_writes();
            assert_eq!(sim.next_event_time(), Some(3));
            sim.run_until(3).unwrap();
            assert_eq!(value(&mut sim, "done"), 1);
        });
    }
}

/// `v[1] = a` drives one bit of `v`, a combinational process copies it to
/// `v[0]`: the process reads the bit it does not write, so it runs again
/// when `a` changes (IEEE 1800-2023 9.2.2.2.1).
fn comb_reads_unwritten_bits(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("CombBits", four_state);
    let a = d.input("a", 1);
    let ty = d.ty(2);
    let v = d.module.internal("v", ty).unwrap();
    let y = d.output("y", 2, 0);
    let high = d.module.slice(v, 1, 1).unwrap();
    let read_a = d.read(a);
    d.module.assign(high, read_a).unwrap();
    let low = d.module.slice(v, 0, 1).unwrap();
    let read_v = d.read(v);
    let bit = d.module.expr_slice(read_v, 1, 1).unwrap();
    d.module
        .comb_process(vec![Statement::Assign {
            target: low,
            value: bit,
        }])
        .unwrap();
    let read_v = d.read(v);
    let whole_y = d.module.whole(y).unwrap();
    d.module.assign(whole_y, read_v).unwrap();
    d.finish()
}

#[test]
fn comb_processes_rerun_on_bits_they_do_not_write() {
    for four_state in [false, true] {
        let artifact = comb_reads_unwritten_bits(four_state);
        on_every_backend!(builder(&artifact, four_state), "CombBits", |sim| {
            let a = sim.signal("a");
            for (time, input, expected) in [(1, 0u8, 0u64), (2, 1, 3), (3, 0, 0), (4, 1, 3)] {
                sim.modify(|io| io.set(a, input)).unwrap();
                sim.run_until(time).unwrap();
                assert_eq!(value(&mut sim, "y"), expected, "a = {input}");
            }
        });
    }
}

/// `gclk = clk & en` clocks a counter, and a process waits on `clk`: the
/// woken process makes a second wave of the active region, or with `#0`
/// an inactive region, before the gated domain evaluates. The domain still
/// sees every edge.
fn gated_clock_with_process(four_state: bool, inactive: bool) -> FrontendArtifact {
    let mut d = Design::new("GatedClock", four_state);
    let clk = d.input("clk", 1);
    let en = d.input("en", 1);
    let ty = d.ty(1);
    let gclk = d.module.internal("gclk", ty).unwrap();
    let (read_clk, read_en) = (d.read(clk), d.read(en));
    let and = d
        .module
        .binary(BinaryOp::And, read_clk, read_en, ty)
        .unwrap();
    let target = d.module.whole(gclk).unwrap();
    d.module.assign(target, and).unwrap();
    let count = d.output("count", 8, 0);
    d.counter(count, gclk);
    let ticks = d.output("ticks", 8, 0);
    d.counter(ticks, clk);
    let wakes = d.output("wakes", 8, 0);
    let mut body = vec![wait(clk, EventEdge::Posedge)];
    if inactive {
        body.push(d.inactive_delay(0));
    }
    body.push(d.increment(wakes));
    d.module.process(vec![Statement::Forever { body }]).unwrap();
    d.finish()
}

#[test]
fn gated_clocks_see_their_edges_with_processes() {
    for four_state in [false, true] {
        for inactive in [false, true] {
            let artifact = gated_clock_with_process(four_state, inactive);
            on_every_backend!(builder(&artifact, four_state), "GatedClock", |sim| {
                let en = sim.signal("en");
                sim.modify(|io| io.set(en, 1u8)).unwrap();
                sim.schedule("clk", 0, 0).unwrap();
                sim.run_until(0).unwrap();
                for edge in 1..=3u64 {
                    sim.schedule("clk", 2 * edge - 1, 1).unwrap();
                    sim.schedule("clk", 2 * edge, 0).unwrap();
                    sim.run_until(2 * edge).unwrap();
                    assert_eq!(value(&mut sim, "ticks"), edge);
                    assert_eq!(value(&mut sim, "wakes"), edge);
                    assert_eq!(value(&mut sim, "count"), edge, "edge {edge}");
                }
            });
        }
    }
}

/// A process that has finished, or one waiting on a clock, does not keep
/// host writes from advancing time: each step settles them at the current
/// time and goes on to the next event.
fn host_writes_with_processes(four_state: bool, waiting: bool) -> FrontendArtifact {
    let mut d = Design::new("HostSteps", four_state);
    let clk = d.input("clk", 1);
    let din = d.input("d", 8);
    let q = d.output("q", 8, 0);
    let read = d.read(din);
    let target = d.module.whole(q).unwrap();
    d.module.assign(target, read).unwrap();
    let ticks = d.output("ticks", 8, 0);
    d.counter(ticks, clk);
    let done = d.output("done", 1, 0);
    let body = vec![d.delay(5), d.set(done, 1)];
    d.module.process(body).unwrap();
    if waiting {
        let rises = d.output("rises", 8, 0);
        let body = vec![Statement::Forever {
            body: vec![wait(clk, EventEdge::Posedge), d.increment(rises)],
        }];
        d.module.process(body).unwrap();
    }
    d.finish()
}

#[test]
fn host_writes_do_not_hold_steps_at_the_current_time() {
    for four_state in [false, true] {
        for waiting in [false, true] {
            let artifact = host_writes_with_processes(four_state, waiting);
            on_every_backend!(builder(&artifact, four_state), "HostSteps", |sim| {
                for (time, level) in [(10, 1), (20, 0), (30, 1)] {
                    sim.schedule("clk", time, level).unwrap();
                }
                sim.run_until(6).unwrap();
                assert_eq!(value(&mut sim, "done"), 1);
                let din = sim.signal("d");
                for (index, time) in [10u64, 20, 30].into_iter().enumerate() {
                    sim.modify(|io| io.set(din, index as u8 + 1)).unwrap();
                    assert_eq!(sim.step().unwrap(), Some(time));
                    assert_eq!(value(&mut sim, "q"), index as u64 + 1);
                }
            });
        }
    }
}

/// `v` is counted, since a process stores `v[0]` twice, and a wait watches
/// it; `v[1]` is driven combinationally from an input. The wait sees the
/// changes the uncounted writer makes too.
fn counted_with_comb_writer(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("CountedComb", four_state);
    let a = d.input("a", 1);
    let ty = d.ty(2);
    let v = d.module.internal("v", ty).unwrap();
    let high = d.module.slice(v, 1, 1).unwrap();
    let read_a = d.read(a);
    d.module.assign(high, read_a).unwrap();
    let changes = d.output("changes", 8, 0);
    let body = vec![Statement::Forever {
        body: vec![wait(v, EventEdge::Change), d.increment(changes)],
    }];
    d.module.process(body).unwrap();
    let low = d.module.slice(v, 0, 1).unwrap();
    let (one, zero) = (d.constant(1, 1), d.constant(0, 1));
    let body = vec![
        Statement::Assign {
            target: low,
            value: one,
        },
        Statement::Assign {
            target: low,
            value: zero,
        },
    ];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn waits_on_counted_signals_see_uncounted_writers() {
    for four_state in [false, true] {
        let artifact = counted_with_comb_writer(four_state);
        on_every_backend!(builder(&artifact, four_state), "CountedComb", |sim| {
            let a = sim.signal("a");
            sim.modify(|io| io.set(a, 0u8)).unwrap();
            sim.run_until(1).unwrap();
            let base = value(&mut sim, "changes");
            assert!(base >= 1, "the pulse of v[0] is a change");
            for (time, input, changes) in [(2, 1u8, base + 1), (4, 0, base + 2)] {
                sim.modify(|io| io.set(a, input)).unwrap();
                // Saving settles the write; the loaded wait still compares
                // with the value it saw before it.
                let file = sim.save_state().unwrap();
                sim.load_state(&file).unwrap();
                sim.run_until(time).unwrap();
                assert_eq!(value(&mut sim, "changes"), changes, "a = {input}");
            }
        });
    }
}

/// `a` toggles on `clk`, `b <= a` on `posedge a`, and `y = a & ~b`: when
/// `a` first rises, `y` is high between the two domains' publications and
/// low at the end of the slot. A process waiting on `posedge y` sees the
/// pulse like the flip-flop clocked by `y` does.
fn comb_pulse_between_waves(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("CombPulse", four_state);
    let clk = d.input("clk", 1);
    let a = d.internal("a", 1, 0);
    let b = d.internal("b", 1, 0);
    let ty = d.ty(1);
    let y = d.module.internal("y", ty).unwrap();
    let read_a = d.read(a);
    let not_a = d.module.unary(UnaryOp::BitNot, read_a, ty).unwrap();
    d.register(a, not_a, clk, Edge::Posedge);
    let read_a = d.read(a);
    d.register(b, read_a, a, Edge::Posedge);
    let (read_a, read_b) = (d.read(a), d.read(b));
    let not_b = d.module.unary(UnaryOp::BitNot, read_b, ty).unwrap();
    let and = d.module.binary(BinaryOp::And, read_a, not_b, ty).unwrap();
    let target = d.module.whole(y).unwrap();
    d.module.assign(target, and).unwrap();
    let ffs = d.output("ffs", 8, 0);
    d.counter(ffs, y);
    let wakes = d.output("wakes", 8, 0);
    let body = vec![Statement::Forever {
        body: vec![wait(y, EventEdge::Posedge), d.increment(wakes)],
    }];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn waits_see_combinational_pulses_between_flip_flop_waves() {
    for four_state in [false, true] {
        let artifact = comb_pulse_between_waves(four_state);
        on_every_backend!(builder(&artifact, four_state), "CombPulse", |sim| {
            sim.schedule("clk", 0, 0).unwrap();
            for edge in 1..=4u64 {
                sim.schedule("clk", 2 * edge - 1, 1).unwrap();
                sim.schedule("clk", 2 * edge, 0).unwrap();
            }
            sim.run_until(8).unwrap();
            assert_eq!(value(&mut sim, "ffs"), 1);
            assert_eq!(value(&mut sim, "wakes"), 1);
        });
    }
}

/// A loop that may not suspend yields only every 2^16 iterations: a long
/// finite loop finishes within a return budget of two.
fn long_loop(four_state: bool) -> FrontendArtifact {
    let mut d = Design::new("LongLoop", four_state);
    let i = d.output("i", 32, 0);
    let done = d.output("done", 1, 0);
    let read = d.read(i);
    let limit = d.constant(1 << 17, 32);
    let ty = d.ty(1);
    let condition = d
        .module
        .binary(BinaryOp::LessUnsigned, read, limit, ty)
        .unwrap();
    let body = vec![
        d.delay(1),
        Statement::While {
            condition,
            body: vec![d.increment(i)],
        },
        d.set(done, 1),
    ];
    d.module.process(body).unwrap();
    d.finish()
}

#[test]
fn long_finite_loops_yield_rarely() {
    for four_state in [false, true] {
        let artifact = long_loop(four_state);
        on_every_backend!(builder(&artifact, four_state), "LongLoop", |sim| {
            sim.state.set_slot_budgets(256, 2);
            sim.run_until(2).unwrap();
            assert_eq!(value(&mut sim, "i"), 1 << 17);
            assert_eq!(value(&mut sim, "done"), 1);
        });
    }
}

/// Records every call a timed step makes on a simulator, with its
/// arguments, and forwards it.
struct Tracing<'a, B: SimBackend> {
    sim: &'a mut Simulator<B>,
    calls: Vec<String>,
}

impl<B: SimBackend> celox_runtime::SimulationExecutor for Tracing<'_, B> {
    type Backend = B;

    fn backend(&self) -> &B {
        &self.sim.backend
    }

    fn backend_mut(&mut self) -> &mut B {
        &mut self.sim.backend
    }

    fn eval_comb(&mut self) -> Result<(), celox_runtime::Halt> {
        self.calls.push("comb".into());
        celox_runtime::SimulationExecutor::eval_comb(self.sim)
    }

    fn eval_apply_ff_at(&mut self, event: B::Event) -> Result<(), celox_runtime::Halt> {
        self.calls.push(format!("eval_apply(e{})", event.id()));
        celox_runtime::SimulationExecutor::eval_apply_ff_at(self.sim, event)
    }

    fn eval_only_ff_at(&mut self, event: B::Event) -> Result<(), celox_runtime::Halt> {
        self.calls.push(format!("eval_only(e{})", event.id()));
        celox_runtime::SimulationExecutor::eval_only_ff_at(self.sim, event)
    }

    fn apply_ff_at(&mut self, event: B::Event) -> Result<(), celox_runtime::Halt> {
        self.calls.push(format!("apply(e{})", event.id()));
        celox_runtime::SimulationExecutor::apply_ff_at(self.sim, event)
    }

    fn run_process(&mut self, index: usize) -> Result<(), celox_runtime::Halt> {
        self.calls.push(format!("process({index})"));
        celox_runtime::SimulationExecutor::run_process(self.sim, index)
    }

    fn take_host_writes(&mut self) -> Vec<(crate::SignalRef, celox_runtime::timing::Value)> {
        let writes = celox_runtime::SimulationExecutor::take_host_writes(self.sim);
        self.calls.push(format!("host_writes({})", writes.len()));
        writes
    }

    fn freeze(&mut self) {
        self.calls.push("freeze".into());
        celox_runtime::SimulationExecutor::freeze(self.sim);
    }

    fn stage_external_event(
        &mut self,
        event: B::Event,
        timestamp: u64,
    ) -> Result<(), crate::RuntimeErrorCode> {
        self.calls
            .push(format!("stage(e{}@{timestamp})", event.id()));
        celox_runtime::SimulationExecutor::stage_external_event(self.sim, event, timestamp)
    }

    fn fire_external_event(
        &mut self,
        event: B::Event,
        timestamp: u64,
    ) -> Result<(), crate::RuntimeErrorCode> {
        self.calls
            .push(format!("fire(e{}@{timestamp})", event.id()));
        celox_runtime::SimulationExecutor::fire_external_event(self.sim, event, timestamp)
    }

    fn finish_timed_step(&mut self, timestamp: u64) {
        self.calls.push(format!("finish_step({timestamp})"));
        celox_runtime::SimulationExecutor::finish_timed_step(self.sim, timestamp);
    }
}

/// One operation of a traced script.
#[derive(Clone)]
enum Op {
    /// Write inputs in one host update.
    Modify(Vec<(&'static str, u64)>),
    Schedule(&'static str, u64, u64),
    Clock(&'static str, u64, u64),
    /// [`Simulation::run_until`], step by step.
    Run(u64),
    /// Up to this many [`Simulation::step`]s.
    Steps(usize),
}

/// The executor calls of each step `ops` makes, with the values of
/// `observe` after each operation that runs time.
fn trace<B: SimBackend>(mut sim: Simulation<B>, ops: &[Op], observe: &[&str]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let step = |sim: &mut Simulation<B>, limit: u64, out: &mut String| -> bool {
        let mut tracing = Tracing {
            sim: &mut sim.simulator,
            calls: Vec::new(),
        };
        let result = sim.state.step_until(&mut tracing, limit);
        let calls = tracing.calls.join(" ");
        match result {
            Ok(time) => {
                writeln!(out, "  step -> {time:?}: {calls}").unwrap();
                time.is_some()
            }
            Err(error) => {
                writeln!(out, "  step -> error {error}: {calls}").unwrap();
                false
            }
        }
    };
    'ops: for op in ops {
        match op {
            Op::Modify(writes) => {
                writeln!(out, "modify {writes:?}").unwrap();
                let writes: Vec<_> = writes
                    .iter()
                    .map(|&(name, value)| (sim.signal(name), value))
                    .collect();
                if let Err(error) = sim.modify(|io| {
                    for (signal, value) in writes {
                        io.set_wide(signal, value.into());
                    }
                }) {
                    writeln!(out, "  error {error}").unwrap();
                    break 'ops;
                }
                continue;
            }
            Op::Schedule(name, time, value) => {
                writeln!(out, "schedule {name} @{time} = {value}").unwrap();
                if let Err(error) = sim.schedule(name, *time, *value) {
                    writeln!(out, "  error {error}").unwrap();
                }
                continue;
            }
            Op::Clock(name, period, delay) => {
                writeln!(out, "clock {name} period {period} delay {delay}").unwrap();
                sim.add_clock(name, *period, *delay);
                continue;
            }
            // Mirrors `Simulation::run_until`.
            Op::Run(end_time) => {
                writeln!(out, "run_until {end_time}").unwrap();
                if *end_time >= sim.state.time() {
                    while let Some(next_time) = sim.state.next_event_time() {
                        if next_time > *end_time {
                            break;
                        }
                        if !step(&mut sim, *end_time, &mut out) {
                            break 'ops;
                        }
                    }
                    if !sim.state.is_finished() {
                        sim.state.set_time(*end_time);
                        sim.simulator.dump_unless_rewound(*end_time);
                    }
                }
            }
            Op::Steps(count) => {
                writeln!(out, "steps {count}").unwrap();
                for _ in 0..*count {
                    if !step(&mut sim, u64::MAX, &mut out) {
                        break;
                    }
                }
            }
        }
        write!(out, "  time {} finished {}:", sim.time(), sim.is_finished()).unwrap();
        for name in observe {
            let signal = sim.signal(name);
            write!(out, " {name}={}", sim.get(signal)).unwrap();
        }
        out.push('\n');
    }
    out
}

/// Host updates of `inputs` in a fixed pattern, a pulse within one update,
/// scheduled edges and a periodic clock on `clk`, then free steps.
fn stimulus(inputs: &[&'static str]) -> Vec<Op> {
    let mut ops = vec![Op::Run(0)];
    if inputs.is_empty() {
        ops.extend([Op::Run(1), Op::Run(5), Op::Steps(12)]);
        return ops;
    }
    ops.insert(
        0,
        Op::Modify(inputs.iter().map(|&name| (name, 0)).collect()),
    );
    for t in 1..=6u64 {
        let writes = inputs
            .iter()
            .enumerate()
            .map(|(index, &name)| (name, (t >> index) & 1 | (t * 37 + index as u64) & !1))
            .collect();
        ops.push(Op::Modify(writes));
        ops.push(Op::Run(2 * t));
    }
    ops.push(Op::Modify(vec![(inputs[0], 1), (inputs[0], 0)]));
    ops.push(Op::Run(14));
    if inputs.contains(&"clk") {
        for (time, value) in [(15, 1), (16, 0), (17, 1), (17, 0), (18, 1), (19, 0)] {
            ops.push(Op::Schedule("clk", time, value));
        }
        ops.push(Op::Run(20));
        ops.push(Op::Clock("clk", 4, 21));
        ops.push(Op::Run(40));
    }
    ops.push(Op::Steps(12));
    ops
}

/// The executor call sequence of a timed simulation on each backend: a
/// golden for refactors of the scheduler that must not change behaviour.
#[test]
fn executor_call_traces_are_unchanged() {
    type Case = (
        &'static str,
        fn(bool) -> FrontendArtifact,
        &'static [&'static str],
        &'static [&'static str],
    );
    let cases: &[Case] = &[
        ("pulse", pulse, &[], &["waits", "ffs"]),
        ("repeated_edges", repeated_edges, &[], &["q", "wakes"]),
        ("host_clock", host_clock, &["clk"], &["ffs", "waits"]),
        ("level", level, &[], &["first", "second"]),
        (
            "zero_delays",
            zero_delays,
            &[],
            &["q", "inactive_seen", "post_nba_seen"],
        ),
        (
            "nonblocking",
            nonblocking,
            &[],
            &["q", "r", "q_changes", "r_changes"],
        ),
        ("finishing", finishing, &["clk"], &["a", "edges"]),
        ("waiting", waiting, &["go"], &["wakes"]),
        ("edge_order", edge_order, &["clk"], &["q", "seen"]),
        (
            "comb_pulse",
            comb_pulse,
            &["a", "b"],
            &["y", "z", "ffs", "waits"],
        ),
        ("rtl", |_| rtl(false), &["clk"], &["a", "b", "sum"]),
        (
            "rtl_testbench",
            |_| rtl(true),
            &["clk"],
            &["a", "b", "sum", "edges"],
        ),
        ("republished", republished, &[], &["q", "changes"]),
        ("pulse_around_nba", pulse_around_nba, &[], &["waits", "ffs"]),
        (
            "interleaved",
            interleaved,
            &[],
            &["qa", "qb", "falls", "rises"],
        ),
        ("low_then_high", low_then_high, &[], &["ffs"]),
        ("nba_pulse", nba_pulse, &[], &["ffs"]),
        ("comb_override", comb_override, &["a", "low"], &["y"]),
        ("wait_on_comb", wait_on_comb, &["a", "b"], &["hits"]),
        (
            "nba_pulse_after_one_domain",
            nba_pulse_after_one_domain,
            &["clk"],
            &["a_count", "b_count"],
        ),
        ("pulse_across_waves", pulse_across_waves, &[], &["rises"]),
        ("raw_go", raw_go, &["go"], &["done"]),
        (
            "comb_reads_unwritten_bits",
            comb_reads_unwritten_bits,
            &["a"],
            &["y"],
        ),
        (
            "gated_clock",
            |four_state| gated_clock_with_process(four_state, false),
            &["clk", "en"],
            &["count", "ticks", "wakes"],
        ),
        (
            "gated_clock_inactive",
            |four_state| gated_clock_with_process(four_state, true),
            &["clk", "en"],
            &["count", "ticks", "wakes"],
        ),
        (
            "host_steps",
            |four_state| host_writes_with_processes(four_state, false),
            &["clk", "d"],
            &["q", "ticks", "done"],
        ),
        (
            "host_steps_waiting",
            |four_state| host_writes_with_processes(four_state, true),
            &["clk", "d"],
            &["q", "ticks", "done", "rises"],
        ),
        (
            "counted_with_comb_writer",
            counted_with_comb_writer,
            &["a"],
            &["changes"],
        ),
        (
            "comb_pulse_between_waves",
            comb_pulse_between_waves,
            &["clk"],
            &["ffs", "wakes"],
        ),
        ("long_loop", long_loop, &[], &["i", "done"]),
    ];
    for &(name, design, inputs, observe) in cases {
        for four_state in [false, true] {
            let artifact = design(four_state);
            let ops = stimulus(inputs);
            let builder = || builder(&artifact, four_state);
            let suffix = if four_state { "_4state" } else { "" };
            insta::assert_snapshot!(
                format!("trace_{name}{suffix}_interpreter"),
                trace(
                    Simulation::new(builder().build_interpreter().unwrap()),
                    &ops,
                    observe
                )
            );
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            {
                insta::assert_snapshot!(
                    format!("trace_{name}{suffix}_native"),
                    trace(
                        Simulation::new(builder().build_native().unwrap()),
                        &ops,
                        observe
                    )
                );
                // Parallel flip-flop kernels; their calls are those of the
                // plain RTL design.
                if !name.starts_with("rtl") {
                    continue;
                }
                insta::assert_snapshot!(
                    format!("trace_{name}{suffix}_native_threads"),
                    trace(
                        Simulation::new(builder().threads(4).build_native().unwrap()),
                        &ops,
                        observe
                    )
                );
            }
        }
    }
}

/// Clock aliases, clocks derived combinationally and from registers,
/// asynchronous resets, two clocks and both edges of one clock, in Veryl.
const VERYL_TRACE_DESIGNS: &[(&str, &str, &[&str], &[&str])] = &[
    (
        "veryl_derived_register_clock",
        r#"
module Top (
    clk: input '_ clock,
    rst: input '_ reset_async_low,
    cnt1: output logic<8>,
    cnt2: output logic<8>,
) {
    var a: logic<8>;
    var b: logic<8>;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = a + 8'd3; }
    }
    let divided: '_ clock = a[1];
    always_ff (divided, rst) {
        if_reset { b = 0; } else { b = b + 8'd1; }
    }
    assign cnt1 = a;
    assign cnt2 = b;
}
"#,
        &["clk", "rst"],
        &["cnt1", "cnt2"],
    ),
    (
        "veryl_gated_clock",
        r#"
module Top (
    clk: input '_ clock,
    en: input '_ logic,
    rst: input '_ reset_async_low,
    a: output '_ logic<8>,
    b: output '_ logic<8>,
) {
    let gated: '_ clock = clk & en;
    always_ff (clk, rst) {
        if_reset { a = 0; } else { a = a + 8'd1; }
    }
    always_ff (gated, rst) {
        if_reset { b = 0; } else { b = b + a; }
    }
}
"#,
        &["clk", "en", "rst"],
        &["a", "b"],
    ),
    (
        "veryl_two_clocks",
        r#"
module Top (
    clk: input 'a clock,
    clk_slow: input 'b clock,
    rst: input 'a reset,
    count: output 'a logic<8>,
    sampled: output 'b logic<8>,
) {
    always_ff (clk, rst) {
        if_reset { count = 0; } else { count = count + 1; }
    }
    unsafe (cdc) {
        always_ff (clk_slow) {
            sampled = count;
        }
    }
}
"#,
        &["clk", "clk_slow", "rst"],
        &["count", "sampled"],
    ),
];

#[test]
fn executor_call_traces_of_veryl_clock_domains_are_unchanged() {
    for &(name, source, inputs, observe) in VERYL_TRACE_DESIGNS {
        let ops = stimulus(inputs);
        let builder = || Simulator::builder(source, "Top").emit_triggers();
        insta::assert_snapshot!(
            format!("trace_{name}_interpreter"),
            trace(
                Simulation::new(builder().build_interpreter().unwrap()),
                &ops,
                observe
            )
        );
        insta::assert_snapshot!(
            format!("trace_{name}_default"),
            trace(
                Simulation::builder(source, "Top").build().unwrap(),
                &ops,
                observe
            )
        );
    }
}

#[cfg(feature = "systemverilog")]
mod sv_traces {
    use super::*;

    /// Clock and reset aliases, a combinationally gated clock, a register
    /// divided clock, both edges of one clock, and the timing suite's cases
    /// that Celox runs.
    const DESIGNS: &[(&str, &str, &[&str], &[&str])] = &[
        (
            "sv_buffered_clock",
            "module Top(input logic clk, output logic [31:0] up, output logic [31:0] down);
               logic g1;
               assign g1 = clk;
               always_ff @(posedge g1) up <= up + 1;
               always_ff @(negedge g1) down <= down + 1;
             endmodule",
            &["clk"],
            &["up", "down"],
        ),
        (
            "sv_aliased_reset",
            "module Top(input logic clk, input logic rst_p, output logic [31:0] q);
               logic rn;
               assign rn = rst_p;
               logic [31:0] r;
               always_ff @(posedge clk or negedge rn) if (!rn) r <= 0; else r <= r + 1;
               assign q = r;
             endmodule",
            &["clk", "rst_p"],
            &["q"],
        ),
        (
            "sv_gated_clock",
            "module Top(input logic clk, input logic en, output logic [7:0] a, output logic [7:0] b);
               logic gclk;
               assign gclk = clk & en;
               always_ff @(posedge clk) a <= a + 8'd1;
               always_ff @(posedge gclk) b <= b + a;
             endmodule",
            &["clk", "en"],
            &["a", "b"],
        ),
        (
            "sv_divided_clock",
            "module Top(input logic clk, output logic [7:0] n, output logic div);
               always_ff @(posedge clk) div <= ~div;
               always_ff @(posedge div) n <= n + 8'd1;
             endmodule",
            &["clk"],
            &["n", "div"],
        ),
        (
            "sv_both_edges",
            "module Child(input logic clk, input logic rst, input logic [7:0] p,
                          output logic [7:0] c, output logic [7:0] f);
               always_ff @(negedge clk or posedge rst) if (rst) f <= 0; else f <= f + 1;
               always_ff @(posedge clk or posedge rst) if (rst) c <= 0; else c <= p;
             endmodule
             module Top(input logic clk, input logic rst,
                        output logic [7:0] p, output logic [7:0] c, output logic [7:0] f);
               always_ff @(posedge clk or posedge rst) if (rst) p <= 0; else p <= p + 1;
               Child u(.clk(clk), .rst(rst), .p(p), .c(c), .f(f));
             endmodule",
            &["clk", "rst"],
            &["p", "c", "f"],
        ),
        (
            "sv_initial_blocks_run_at_time_zero",
            "module Top(output bit [7:0] y);
               initial begin
                 y = 8'd7;
                 $display(\"y=%0d\", y);
               end
             endmodule",
            &[],
            &["y"],
        ),
        (
            "sv_host_edges_after_time_zero_clock_flip_flops",
            "module Top(input bit clk, output bit [7:0] q);
               always_ff @(posedge clk) q <= q + 8'd1;
             endmodule",
            &["clk"],
            &["q"],
        ),
    ];

    #[test]
    fn executor_call_traces_of_sv_designs_are_unchanged() {
        for &(name, source, inputs, observe) in DESIGNS {
            let ops = stimulus(inputs);
            let sources = || vec![(source, std::path::Path::new("trace.sv"))];
            insta::assert_snapshot!(
                format!("trace_{name}_interpreter"),
                trace(
                    Simulation::new(
                        Simulator::from_sv_sources(sources(), "Top")
                            .emit_triggers()
                            .build_interpreter()
                            .unwrap()
                    ),
                    &ops,
                    observe
                )
            );
            insta::assert_snapshot!(
                format!("trace_{name}_default"),
                trace(
                    Simulation::from_sv_sources(sources(), "Top")
                        .build()
                        .unwrap(),
                    &ops,
                    observe
                )
            );
        }
    }
}
