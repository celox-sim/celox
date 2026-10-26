//! A host that observes the regions inside a time slot, as a VPI driver
//! does (#1730): edges before the flip-flop domains commit, and settled
//! slots.
#![cfg(feature = "systemverilog")]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

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
    seen: Rc<RefCell<Vec<(u64, u64)>>>,
}

impl<B: SimBackend> SlotObserver<B> for Edges {
    fn before_domains(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        let level = u8::try_from(io.get(self.clk)).unwrap();
        if level == 1 && self.level == 0 {
            let value = u64::try_from(io.get(self.watched)).unwrap();
            self.seen.borrow_mut().push((time, value));
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
    let seen = Rc::new(RefCell::new(Vec::new()));
    let clk = sim.signal("clk");
    let observer = Edges {
        clk,
        watched: sim.signal("q"),
        write: None,
        level: 0,
        seen: Rc::clone(&seen),
    };
    sim.observe_slots(Box::new(observer), &[clk]);
    sim.run_until(40).unwrap();
    // The rises at 5, 15, 25 and 35 see the count before their update.
    assert_eq!(*seen.borrow(), vec![(5, 0), (15, 1), (25, 2), (35, 3)]);
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
        seen: Rc::default(),
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
        seen: Rc::default(),
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
    seen: Rc<RefCell<Vec<(u64, u64)>>>,
}

impl<B: SimBackend> SlotObserver<B> for Settled {
    fn before_domains(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}

    fn end_of_slot(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        let value = u64::try_from(io.get(self.watched)).unwrap();
        if value != self.last {
            self.seen.borrow_mut().push((time, value));
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
    let seen = Rc::new(RefCell::new(Vec::new()));
    let watched = sim.signal("q");
    let observer = Settled {
        watched,
        flag: sim.signal("flag"),
        at: 2,
        last: 0,
        seen: Rc::clone(&seen),
    };
    sim.observe_slots(Box::new(observer), &[watched]);
    sim.run_until(40).unwrap();
    // Every slot of the compiled clock is seen, one per run.
    assert_eq!(*seen.borrow(), vec![(5, 1), (15, 2), (25, 3), (35, 4)]);
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
        seen: Rc::default(),
    };
    sim.observe_slots(Box::new(observer), &[watched]);
    sim.run_until(40).unwrap();
    let stats = sim.scheduler_stats();
    assert_eq!(stats.checked_steps, 0);
    assert!(stats.unchecked.is_some());
}
