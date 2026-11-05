//! Value changes a slot observer sees after each update of a time slot, as
//! VPI value change callbacks see them (IEEE 1800-2023 38.36.1, 4.10).

use std::path::Path;
use std::sync::mpsc;

use celox::{SignalRef, SimBackend, Simulation, SlotIo, SlotObserver};

/// Records the values of `signals` after every update that changed one.
struct Changes {
    signals: Vec<SignalRef>,
    last: Option<Vec<u64>>,
    seen: mpsc::Sender<(u64, Vec<u64>)>,
}

impl<B: SimBackend> SlotObserver<B> for Changes {
    fn before_domains(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}

    fn end_of_slot(&mut self, _: &mut SlotIo<'_, B>, _: u64) {}

    fn observes_updates(&self) -> bool {
        true
    }

    fn after_update(&mut self, io: &mut SlotIo<'_, B>, time: u64) {
        let values: Vec<u64> = self
            .signals
            .iter()
            .map(|&signal| u64::try_from(io.get(signal)).unwrap())
            .collect();
        if self.last.as_ref() != Some(&values) {
            self.seen.send((time, values.clone())).unwrap();
            self.last = Some(values);
        }
    }
}

/// The changes of `watched` in `source` to `end` that an observer of every
/// update sees, in each scheduler mode.
fn changes(source: &str, watched: &[&str], end: u64) -> Vec<(u64, Vec<u64>)> {
    let mut seen = Vec::new();
    for mode in [
        celox::SchedulerMode::Reference,
        celox::SchedulerMode::Compiled,
    ] {
        let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .scheduler_mode(mode)
            .observable_updates(true)
            .build()
            .unwrap();
        let signals: Vec<_> = watched.iter().map(|name| sim.signal(name)).collect();
        let initial = signals
            .iter()
            .map(|&signal| u64::try_from(sim.get(signal)).unwrap())
            .collect();
        let (sender, received) = mpsc::channel();
        sim.observe_slots(
            Box::new(Changes {
                signals: signals.clone(),
                last: Some(initial),
                seen: sender,
            }),
            &signals,
        );
        sim.run_until(end).unwrap();
        if mode == celox::SchedulerMode::Compiled {
            assert!(sim.scheduler_stats().compiled_slots > 0, "no compiled slot");
        }
        seen.push(received.try_iter().collect::<Vec<_>>());
    }
    assert_eq!(seen[0], seen[1], "compiled and reference runs differ");
    seen.swap_remove(0)
}

#[test]
fn a_pulse_across_an_inactive_region_is_seen() {
    // `p` rises in the active region and falls in the inactive one, before
    // any domain: both are value changes.
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic p = 0;
    always #5 clk = ~clk;
    always @(posedge clk) begin
        p = 1;
        #0 p = 0;
    end
endmodule
"#;
    assert_eq!(
        changes(source, &["p"], 20),
        vec![(5, vec![1]), (5, vec![0]), (15, vec![1]), (15, vec![0])]
    );
}

#[test]
fn a_commit_is_seen_before_the_processes_it_wakes_run() {
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
    assert_eq!(
        changes(source, &["count", "mirror"], 20),
        vec![
            (5, vec![1, 0]),
            (5, vec![1, 1]),
            (15, vec![2, 1]),
            (15, vec![2, 2]),
        ]
    );
}

#[test]
fn a_net_changes_after_the_variable_it_reads() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic [7:0] x = 0;
    wire [7:0] y = x + 1;
    initial begin
        #5 x = 1;
        #5 x = 2;
    end
endmodule
"#;
    assert_eq!(
        changes(source, &["x", "y"], 20),
        vec![
            (5, vec![1, 1]),
            (5, vec![1, 2]),
            (10, vec![2, 2]),
            (10, vec![2, 3]),
        ]
    );
}

#[test]
fn nonblocking_updates_are_seen_after_the_process_that_schedules_them() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic [7:0] a = 0;
    logic [7:0] b = 0;
    initial begin
        #5 a = 1;
        b <= 1;
    end
endmodule
"#;
    assert_eq!(
        changes(source, &["a", "b"], 10),
        vec![(5, vec![1, 0]), (5, vec![1, 1])]
    );
}
