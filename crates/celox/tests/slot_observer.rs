//! A host that observes the regions inside a time slot, as a VPI driver
//! does (#1730): edges before the flip-flop domains commit, and settled
//! slots.
#![cfg(feature = "systemverilog")]

use std::path::Path;
use std::sync::mpsc;

use celox::{SignalRef, SimBackend, Simulation, SlotIo, SlotObserver};

fn simulation(source: &str) -> Simulation {
    Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .build()
        .unwrap()
}

const COUNTER: &str = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] q = 0;
    logic [7:0] d = 0;
    logic [7:0] ticks = 0;
    always #5 clk = ~clk;
    always #7 ticks = ticks + 1;
    always_ff @(posedge clk) q <= q + 1;
endmodule
"#;

/// Records `watched` at every rise of `clk` before the domains commit, and
/// writes `write` there if set.
struct Edges {
    clk: SignalRef,
    watched: SignalRef,
    write: Option<SignalRef>,
    level: u8,
    seen: mpsc::Sender<(u64, u64)>,
}

impl<B: SimBackend> SlotObserver<B> for Edges {
    fn before_domains(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        let level = u8::try_from(io.get(self.clk)).unwrap();
        if level == 1 && self.level == 0 {
            let value = u64::try_from(io.get(self.watched)).unwrap();
            // A test that does not check what is seen drops the receiver.
            let _ = self.seen.send((time, value));
            if let Some(write) = self.write {
                io.set(write, time as u8);
            }
        }
        self.level = level;
    }

    fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}
}

#[test]
fn an_observed_edge_is_seen_before_the_domains_commit() {
    let mut sim = simulation(COUNTER);
    let (seen, received) = mpsc::channel();
    let clk = sim.signal("clk");
    let observer = Edges {
        clk,
        watched: sim.signal("q"),
        write: None,
        level: 0,
        seen,
    };
    sim.observe_slots(Box::new(observer), &[clk]);
    sim.run_until(40).unwrap();
    // The rises at 5, 15, 25 and 35 see the count before their update.
    assert_eq!(
        received.try_iter().collect::<Vec<_>>(),
        vec![(5, 0), (15, 1), (25, 2), (35, 3)]
    );
    assert_eq!(sim.get(sim.signal("q")), 4u8.into());
}

#[test]
fn only_the_kernels_that_write_an_observed_event_leave_compiled_slots() {
    let mut plain = simulation(COUNTER);
    plain.run_until(200).unwrap();
    let unobserved = plain.scheduler_stats().compiled_slots;
    assert!(unobserved > 0);

    let mut sim = simulation(COUNTER);
    let clk = sim.signal("clk");
    let observer = Edges {
        clk,
        watched: sim.signal("q"),
        write: None,
        level: 0,
        seen: mpsc::channel().0,
    };
    sim.observe_slots(Box::new(observer), &[clk]);
    sim.run_until(200).unwrap();
    let observed = sim.scheduler_stats().compiled_slots;
    // The slots of `ticks` alone (7, 14, 21, ... off the clock's) stay
    // compiled; those of the clock run in the reference loop.
    assert!(
        observed > 0 && observed < unobserved,
        "{observed} of {unobserved}"
    );
    assert_eq!(sim.get(sim.signal("q")), plain.get(plain.signal("q")));
    assert_eq!(
        sim.get(sim.signal("ticks")),
        plain.get(plain.signal("ticks"))
    );
}

#[test]
fn a_write_before_the_domains_is_what_they_sample() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] d = 0;
    logic [7:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= d;
endmodule
"#;
    let mut sim = simulation(source);
    let clk = sim.signal("clk");
    let d = sim.signal("d");
    let observer = Edges {
        clk,
        watched: d,
        write: Some(d),
        level: 0,
        seen: mpsc::channel().0,
    };
    sim.observe_slots(Box::new(observer), &[clk]);
    sim.run_until(16).unwrap();
    // The rise at 15 samples the 15 the host wrote at that edge.
    assert_eq!(sim.get(sim.signal("q")), 15u8.into());
}

/// At the end of every slot, records changes of `watched` and sets `flag`
/// once `watched` reaches `at`.
struct Settled {
    watched: SignalRef,
    flag: SignalRef,
    at: u64,
    last: u64,
    seen: mpsc::Sender<(u64, u64)>,
}

impl<B: SimBackend> SlotObserver<B> for Settled {
    fn before_domains(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}

    fn end_of_slot(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        let value = u64::try_from(io.get(self.watched)).unwrap();
        if value != self.last {
            // A test that does not check what is seen drops the receiver.
            let _ = self.seen.send((time, value));
            if value == self.at {
                io.set(self.flag, 1u8);
            }
        }
        self.last = value;
    }
}

#[test]
fn a_settled_slot_shows_every_change_and_its_writes_stay_in_the_slot() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] q = 0;
    logic flag = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 1;
    initial begin
        wait (flag);
        $display("flag at %0t q=%0d", $time, q);
    end
endmodule
"#;
    let mut sim = simulation(source);
    let (seen, received) = mpsc::channel();
    let watched = sim.signal("q");
    let observer = Settled {
        watched,
        flag: sim.signal("flag"),
        at: 2,
        last: 0,
        seen,
    };
    sim.observe_slots(Box::new(observer), &[watched]);
    sim.run_until(40).unwrap();
    // Every slot of the compiled clock is seen, one per run.
    assert_eq!(
        received.try_iter().collect::<Vec<_>>(),
        vec![(5, 1), (15, 2), (25, 3), (35, 4)]
    );
    assert!(sim.scheduler_stats().compiled_slots > 0);
    // The flag written at the end of slot 15 wakes the process at 15.
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: b"flag at 15 q=2".to_vec()
        }]
    );
}

#[test]
fn a_differential_simulation_stops_checking_an_observed_simulation() {
    let mut sim = Simulation::from_sv_sources(vec![(COUNTER, Path::new("top.sv"))], "Top")
        .scheduler_mode(celox::SchedulerMode::Differential)
        .build()
        .unwrap();
    let watched = sim.signal("q");
    let observer = Settled {
        watched,
        flag: sim.signal("d"),
        at: u64::MAX,
        last: 0,
        seen: mpsc::channel().0,
    };
    sim.observe_slots(Box::new(observer), &[watched]);
    sim.run_until(40).unwrap();
    let stats = sim.scheduler_stats();
    assert_eq!(stats.checked_steps, 0);
    assert!(stats.unchecked.is_some());
}

struct FinishAtFive {
    before_domains: bool,
    write: SignalRef,
}

impl<B: SimBackend> SlotObserver<B> for FinishAtFive {
    fn before_domains(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        if self.before_domains && time == 5 {
            io.set(self.write, 55u8);
            io.request_finish();
        }
    }

    fn end_of_slot(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        if !self.before_domains && time == 5 {
            io.set(self.write, 55u8);
            io.request_finish();
        }
    }
}

fn check_observer_finish<B: SimBackend + 'static>(mut sim: Simulation<B>, before_domains: bool) {
    let clk = sim.signal("clk");
    let written = sim.signal("d");
    sim.observe_slots(
        Box::new(FinishAtFive {
            before_domains,
            write: written,
        }),
        &[clk],
    );
    sim.run_until(40).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.time(), 5);
    assert_eq!(sim.get(sim.signal("q")), u8::from(!before_domains).into());
    assert_eq!(sim.get(sim.signal("ticks")), 0u8.into());
    assert_eq!(sim.get(written), 55u8.into());
    sim.run_until(100).unwrap();
    assert_eq!(sim.time(), 5);
    assert_eq!(sim.get(sim.signal("q")), u8::from(!before_domains).into());
}

fn check_observer_finish_backends(before_domains: bool) {
    let build = || Simulation::from_sv_sources(vec![(COUNTER, Path::new("top.sv"))], "Top");
    check_observer_finish(build().build_native().unwrap(), before_domains);
    check_observer_finish(build().build_interpreter().unwrap(), before_domains);
}

#[test]
fn an_observer_can_finish_before_domains_commit() {
    check_observer_finish_backends(true);
}

#[test]
fn an_observer_can_finish_at_the_end_of_a_slot() {
    check_observer_finish_backends(false);
}

#[test]
fn compiled_clocks_accept_reset_gates_driven_only_between_runs() {
    let source = r#"`timescale 1ns/1ns
module Top(input logic clk, rst_n, enable, output logic [7:0] q, memory);
    wire cpu_rst_n = rst_n && enable;
    always_ff @(posedge clk or negedge rst_n)
        if (!rst_n) memory <= 0;
        else memory <= memory + 1;
    always_ff @(posedge clk or negedge cpu_rst_n)
        if (!cpu_rst_n) q <= 0;
        else q <= q + 1;
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .four_state(false)
        .scheduler_mode(celox::SchedulerMode::DifferentialRuns)
        .build()
        .unwrap();
    let rst = sim.signal("rst_n");
    let enable = sim.signal("enable");
    let q = sim.signal("q");
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.modify(|io| io.set(enable, 1u8)).unwrap();
    sim.add_clock("clk", 2, 1);
    sim.run_until(20).unwrap();
    assert_eq!(sim.get(q), 10u8.into());
    assert!(
        sim.scheduler_stats().compiled_slots > 0,
        "{:?}",
        sim.scheduler_stats()
    );
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.run_until(22).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.run_until(30).unwrap();
    assert_eq!(sim.get(q), 4u8.into());
    assert!(sim.scheduler_stats().checked_steps > 0);
}

struct DataChanges {
    count: SignalRef,
    mirror: SignalRef,
    last: u64,
    changes: mpsc::Sender<(u64, u64)>,
    settled: mpsc::Sender<(u64, u64)>,
}

impl<B: SimBackend> SlotObserver<B> for DataChanges {
    fn before_domains(&mut self, io: &mut SlotIo<'_, B>, _: u64) {
        let count = u64::try_from(io.get(self.count)).unwrap();
        if count != self.last {
            self.changes
                .send((count, u64::try_from(io.get(self.mirror)).unwrap()))
                .unwrap();
        }
        self.last = count;
    }

    fn end_of_slot(&mut self, io: &mut SlotIo<'_, B>, _: u64) {
        self.settled
            .send((
                u64::try_from(io.get(self.count)).unwrap(),
                u64::try_from(io.get(self.mirror)).unwrap(),
            ))
            .unwrap();
    }
}

#[test]
fn observed_data_changes_precede_native_waiters_and_unwatching_restores_native_slots() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] count = 0;
    logic [7:0] mirror = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) count <= count + 1;
    initial forever begin
        @(count);
        mirror = count;
    end
endmodule
"#;
    for four_state in [false, true] {
        for optimize in [false, true] {
            let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .four_state(four_state)
                .optimize(optimize)
                .build_native()
                .unwrap();
            sim.run_until(0).unwrap();
            assert!(
                sim.scheduler_stats()
                    .kernels
                    .iter()
                    .all(|kernel| { matches!(kernel, celox::KernelSchedule::Compiled) })
            );
            let count = sim.signal("count");
            let (changes, received) = mpsc::channel();
            let (settled, final_values) = mpsc::channel();
            sim.observe_slots(
                Box::new(DataChanges {
                    count,
                    mirror: sim.signal("mirror"),
                    last: 0,
                    changes,
                    settled,
                }),
                &[count],
            );
            sim.run_until(30).unwrap();
            assert_eq!(
                received.try_iter().collect::<Vec<_>>(),
                vec![(1, 0), (2, 1), (3, 2)]
            );
            assert!(
                final_values
                    .try_iter()
                    .all(|(count, mirror)| count == mirror)
            );
            let compiled = sim.scheduler_stats().compiled_slots;
            sim.set_observed_signals(&[]);
            sim.run_until(60).unwrap();
            assert!(sim.scheduler_stats().compiled_slots > compiled);
        }
    }
}

/// Kernels that wait for an edge, delay, and stage nonblocking updates, a
/// host clock, and an asynchronous reset the host schedules: one-slot runs
/// of the compiled scheduler with work of the reference loop between them.
const RESIDENT: &str = r#"`timescale 1ns/1ns
module Top(input logic hclk, input logic rst);
    logic clk = 0;
    logic [7:0] stim = 0;
    logic [7:0] ticks = 0;
    logic [7:0] late = 0;
    logic [7:0] q = 0;
    logic [7:0] h = 0;
    always #5 clk = ~clk;
    always #7 ticks = ticks + 1;
    initial forever @(posedge hclk) stim = stim + 3;
    initial forever begin
        #3 late <= late + ticks;
    end
    always_ff @(posedge clk or posedge rst)
        if (rst) q <= 0; else q <= q + stim + late;
    always_ff @(posedge hclk) h <= h + q;
endmodule
"#;

const RESIDENT_SIGNALS: [&str; 5] = ["q", "h", "stim", "ticks", "late"];

/// Runs `RESIDENT` in `mode`, with the changes of `q` watched at the end of
/// every slot if `observe`, through runs that end between slots or at their
/// budget, a reset the reference loop applies, and a checkpoint restored
/// mid-run. Returns what the observer saw, the final values and the compiled
/// slots.
fn resident_run(mode: celox::SchedulerMode, observe: bool) -> (Vec<(u64, u64)>, Vec<u64>, u64) {
    let mut sim = Simulation::from_sv_sources(vec![(RESIDENT, Path::new("top.sv"))], "Top")
        .scheduler_mode(mode)
        .build()
        .unwrap();
    let values = |sim: &mut Simulation| -> Vec<u64> {
        RESIDENT_SIGNALS
            .iter()
            .map(|name| u64::try_from(sim.get(sim.signal(name))).unwrap())
            .collect()
    };
    sim.add_clock("hclk", 4, 1);
    let (seen, received) = mpsc::channel();
    if observe {
        let q = sim.signal("q");
        let observer = Settled {
            watched: q,
            flag: sim.signal("stim"),
            at: u64::MAX,
            last: 0,
            seen,
        };
        sim.observe_slots(Box::new(observer), &[q, sim.signal("h")]);
    }
    sim.schedule("rst", 63, 1).unwrap();
    sim.schedule("rst", 64, 0).unwrap();
    for end in [11, 12, 40, 62, 63, 90] {
        sim.run_until(end).unwrap();
    }
    let checkpoint = sim.checkpoint().unwrap();
    sim.run_until(150).unwrap();
    let ahead = values(&mut sim);
    sim.restore(&checkpoint).unwrap();
    while sim.time() < 150 {
        sim.step().unwrap();
    }
    assert_eq!(
        values(&mut sim),
        ahead,
        "{mode:?}: stepping after the restore"
    );
    // Runs that end at their budget, between slots of one time or not.
    while sim.time() < 200 {
        sim.run_slots(3, || false).unwrap();
    }
    sim.run_until(300).unwrap();
    let compiled = sim.scheduler_stats().compiled_slots;
    (received.try_iter().collect(), values(&mut sim), compiled)
}

#[test]
fn consecutive_compiled_runs_match_the_reference_loop() {
    for observe in [true, false] {
        let (seen, values, compiled) = resident_run(celox::SchedulerMode::Compiled, observe);
        let (reference_seen, reference_values, _) =
            resident_run(celox::SchedulerMode::Reference, observe);
        assert!(compiled > 0, "observe {observe}");
        assert_eq!(values, reference_values, "observe {observe}");
        assert_eq!(seen, reference_seen, "observe {observe}");
    }
}

/// Runs `source` to 3000 in `mode` with a host clock on `hclk`, draining
/// its runtime events every 400 time units. Returns the events, `q` and the
/// compiled slots.
fn displayed_run(source: &str, mode: celox::SchedulerMode) -> (Vec<celox::RuntimeEvent>, u64, u64) {
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .scheduler_mode(mode)
        .build()
        .unwrap();
    sim.add_clock("hclk", 4, 1);
    let mut events = Vec::new();
    for end in (400..=3000).step_by(400).chain([3000]) {
        sim.run_until(end).unwrap();
        events.extend(sim.drain_runtime_events());
    }
    let q = u64::try_from(sim.get(sim.signal("q"))).unwrap();
    let compiled = sim.scheduler_stats().compiled_slots;
    (events, q, compiled)
}

#[test]
fn compiled_runs_that_return_to_drain_events_continue_resident() {
    // A run returns whenever its events fill an eighth of the ring buffer,
    // 128 of them, several times in each run to the next drain; the loop
    // goes on from the kernels and clock it left in the slots.
    let source = r#"`timescale 1ns/1ns
module Top(input logic hclk);
    logic [15:0] ticks = 0;
    logic [15:0] q = 0;
    always #2 begin
        ticks = ticks + 1;
        $display("tick %0d", ticks);
    end
    always_ff @(posedge hclk) q <= q + ticks;
endmodule
"#;
    let (events, q, compiled) = displayed_run(source, celox::SchedulerMode::Compiled);
    let (reference_events, reference_q, _) = displayed_run(source, celox::SchedulerMode::Reference);
    assert!(compiled > 0);
    assert_eq!(events.len(), 1500);
    assert_eq!(events, reference_events);
    assert_eq!(q, reference_q);
}
