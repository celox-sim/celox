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
    /// Once `when` reads `value`, the host writes `set` to `target`.
    write: Option<HostWrite>,
}

struct HostWrite {
    when: SignalRef,
    value: u64,
    target: SignalRef,
    set: u64,
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
        if let Some(write) = &self.write
            && u64::try_from(io.get(write.when)).unwrap() == write.value
        {
            io.set(write.target, write.set);
            self.write = None;
        }
    }
}

/// Show `sim`'s updates of `watched` to a [`Changes`] observer that makes
/// `write` (`when`, `value`, `target`, `set`), and return what it sees.
fn observe<B: SimBackend + 'static>(
    sim: &mut Simulation<B>,
    watched: &[&str],
    write: Option<(&str, u64, &str, u64)>,
) -> mpsc::Receiver<(u64, Vec<u64>)> {
    let signals: Vec<_> = watched.iter().map(|name| sim.signal(name)).collect();
    let initial = signals
        .iter()
        .map(|&signal| u64::try_from(sim.get(signal)).unwrap())
        .collect();
    let write = write.map(|(when, value, target, set)| HostWrite {
        when: sim.signal(when),
        value,
        target: sim.signal(target),
        set,
    });
    let (sender, received) = mpsc::channel();
    sim.observe_slots(
        Box::new(Changes {
            signals: signals.clone(),
            last: Some(initial),
            seen: sender,
            write,
        }),
        &signals,
    );
    received
}

/// What `run` shows an observer of `watched` that makes `write`, in each
/// scheduler mode: the reference and compiled runs must agree, and the
/// compiled one must run compiled slots.
fn changes_of(
    source: &str,
    watched: &[&str],
    write: Option<(&str, u64, &str, u64)>,
    run: impl Fn(&mut Simulation),
) -> Vec<(u64, Vec<u64>)> {
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
        let received = observe(&mut sim, watched, write);
        run(&mut sim);
        if mode == celox::SchedulerMode::Compiled {
            assert!(sim.scheduler_stats().compiled_slots > 0, "no compiled slot");
        }
        seen.push(received.try_iter().collect::<Vec<_>>());
    }
    assert_eq!(seen[0], seen[1], "compiled and reference runs differ");
    seen.swap_remove(0)
}

/// [`changes_of`] a run to `end` without host writes.
fn changes(source: &str, watched: &[&str], end: u64) -> Vec<(u64, Vec<u64>)> {
    changes_of(source, watched, None, |sim| sim.run_until(end).unwrap())
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

#[test]
fn each_scheduled_value_of_a_pulse_is_seen() {
    // The queue applies the values of one time in value order: `x` falls and
    // rises again at 5, in untimed and timed simulations.
    for timed in ["", "    always #7 ;\n"] {
        let source = format!(
            "`timescale 1ns/1ns\nmodule Top(input logic x);\n    logic [7:0] edges = 0;\n    always @(posedge x) edges = edges + 1;\n{timed}endmodule\n"
        );
        let mut sim =
            Simulation::from_sv_sources(vec![(source.as_str(), Path::new("top.sv"))], "Top")
                .observable_updates(true)
                .build()
                .unwrap();
        let received = observe(&mut sim, &["x"], None);
        sim.schedule("x", 3, 1).unwrap();
        sim.schedule("x", 5, 1).unwrap();
        sim.schedule("x", 5, 0).unwrap();
        sim.run_until(6).unwrap();
        assert_eq!(
            received.try_iter().collect::<Vec<_>>(),
            vec![(3, vec![1]), (5, vec![0]), (5, vec![1])],
            "{timed:?}"
        );
    }
}

#[test]
fn a_host_write_is_seen_before_the_process_it_wakes() {
    let source = r#"`timescale 1ns/1ns
module Top(input logic [7:0] a);
    logic [7:0] b = 0;
    initial #20 ;
    always @(a) b = a + 1;
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .scheduler_mode(celox::SchedulerMode::Reference)
        .observable_updates(true)
        .build()
        .unwrap();
    let received = observe(&mut sim, &["a", "b"], None);
    sim.run_until(4).unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.run_until(30).unwrap();
    // The write applies with the next slot. The process it wakes resumes
    // after that slot's first settle, which shows the write.
    assert_eq!(
        received.try_iter().collect::<Vec<_>>(),
        vec![(0, vec![0, 1]), (20, vec![1, 1]), (20, vec![1, 2])]
    );
}

#[test]
fn a_callback_write_settles_before_the_next_kernel_runs() {
    // The host enables `en` when `x` rises. The kernel that runs next in
    // the same pass reads `d`, which `en` drives.
    let source = r#"`timescale 1ns/1ns
module Top;
    logic x = 0;
    logic en = 0;
    logic seen = 0;
    wire d = en;
    initial #5 x = 1;
    initial #5 seen = d;
endmodule
"#;
    let seen = changes_of(source, &["x", "seen"], Some(("x", 1, "en", 1)), |sim| {
        sim.run_until(10).unwrap()
    });
    assert_eq!(seen, vec![(5, vec![1, 0]), (5, vec![1, 1])]);
}

#[test]
fn an_update_is_seen_before_the_simulation_stops_or_finishes() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic [7:0] x = 0;
    initial begin
        #5 x = 1;
        $stop;
        x = 2;
        $finish;
    end
endmodule
"#;
    // Processes that stop run in the reference loop.
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .scheduler_mode(celox::SchedulerMode::Reference)
        .observable_updates(true)
        .build()
        .unwrap();
    let received = observe(&mut sim, &["x"], None);
    sim.run_until(10).unwrap();
    assert!(sim.is_stopped());
    sim.run_until(10).unwrap();
    assert_eq!(
        received.try_iter().collect::<Vec<_>>(),
        vec![(5, vec![1]), (5, vec![2])]
    );
}

#[test]
fn an_update_is_seen_before_a_compiled_process_finishes() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] x = 0;
    always #5 clk = ~clk;
    initial begin
        #5 x = 1;
        $finish;
    end
endmodule
"#;
    assert_eq!(changes(source, &["x"], 20), vec![(5, vec![1])]);
}

#[cfg(feature = "cranelift")]
#[test]
fn a_build_without_native_services_shows_every_update_from_the_reference_loop() {
    let source = r#"`timescale 1ns/1ns
module Top;
    logic clk = 0;
    logic [7:0] q = 0;
    always #5 clk = ~clk;
    always_ff @(posedge clk) q <= q + 1;
endmodule
"#;
    let mut sim = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
        .observable_updates(true)
        .build_cranelift()
        .unwrap();
    let received = observe(&mut sim, &["clk", "q"], None);
    sim.run_until(10).unwrap();
    assert_eq!(
        received.try_iter().collect::<Vec<_>>(),
        vec![(5, vec![1, 0]), (5, vec![1, 1]), (10, vec![0, 1])]
    );
}
