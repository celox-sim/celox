//! Host clocks and scheduled values (`Simulation::add_clock`,
//! `Simulation::schedule`) on inputs that only waiting processes sense: no
//! flip-flop makes such an input an event, the wait does.

use super::*;

/// The counts of three processes waiting for the rising edges, the falling
/// edges and the changes of `clk`, whose type is `clk_type`.
fn edge_counters(clk_type: &str) -> String {
    format!(
        r#"`timescale 1ns/1ns
        module Top(input {clk_type} clk, output int rises, falls, changes);
            initial forever begin @(posedge clk); rises++; end
            initial forever begin @(negedge clk); falls++; end
            initial forever begin @(clk); changes++; end
        endmodule"#
    )
}

/// `[rises, falls, changes]` of [`edge_counters`].
macro_rules! edge_counts {
    ($sim:expr) => {
        ["rises", "falls", "changes"].map(|name| $sim.get($sim.signal(name)))
    };
}

sv_backends! {
    fn host_clock_wakes_processes_waiting_on_an_input_without_flip_flops(sim) {
        @setup { let source = edge_counters("bit"); }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("waited_clock.sv"))], "Top");
        // Rising edges at 5, 15 and 25, falling edges at 10, 20 and 30.
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(edge_counts!(sim), [3u32, 3, 6].map(BigUint::from));
        // The input is an event, and the only one.
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
    }

    fn scheduled_values_wake_processes_waiting_on_an_input_without_flip_flops(sim) {
        @setup { let source = edge_counters("bit"); }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("waited_schedule.sv"))], "Top");
        sim.schedule("clk", 3, 1).unwrap();
        sim.schedule("clk", 6, 0).unwrap();
        sim.schedule("clk", 9, 1).unwrap();
        // A value the signal already has is no change.
        sim.schedule("clk", 12, 1).unwrap();
        sim.run_until(15).unwrap();
        assert_eq!(edge_counts!(sim), [2u32, 1, 3].map(BigUint::from));
    }

    fn host_writes_of_a_waited_input_wake_its_processes(sim) {
        @setup { let source = edge_counters("bit"); }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("waited_writes.sv"))], "Top");
        let clk = sim.signal("clk");
        // The processes begin to wait at time 0.
        sim.run_until(1).unwrap();
        for (time, level) in [(2, 1u8), (3, 0), (4, 1)] {
            sim.modify(|io| io.set(clk, level)).unwrap();
            sim.run_until(time).unwrap();
        }
        assert_eq!(edge_counts!(sim), [2u32, 1, 3].map(BigUint::from));
    }

    fn host_clock_edges_of_a_four_state_input_follow_table_9_2(sim) {
        @setup { let source = edge_counters("logic"); }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("waited_logic_clock.sv"))], "Top").four_state(true);
        // The first rising edge leaves x (IEEE 1800-2023 Table 9-2).
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(edge_counts!(sim), [3u32, 3, 6].map(BigUint::from));
    }

    fn waited_host_clock_runs_in_the_compiled_scheduler(sim) {
        @setup { let source = edge_counters("bit"); }
        @build celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("waited_compiled.sv"))], "Top")
            .scheduler_mode(celox::SchedulerMode::DifferentialRuns);
        sim.add_clock("clk", 4, 1);
        for stop in [2, 9, 40] {
            sim.run_until(stop).unwrap();
        }
        // Rising edges at 1, 5, ..., 37, falling edges at 3, 7, ..., 39.
        assert_eq!(edge_counts!(sim), [10u32, 10, 20].map(BigUint::from));
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert!(stats.checked_steps > 0, "{stats:?}");
        assert!(stats.unchecked.is_none(), "{stats:?}");
        assert!(
            stats.kernels.iter().all(|kernel| *kernel == celox::KernelSchedule::Compiled),
            "{stats:?}"
        );
    }

    fn host_clock_reaches_a_wait_through_port_connections(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Leaf(input logic c, output int n);
                initial forever begin @(posedge c); n++; end
            endmodule
            module Mid(input logic clk, output int n);
                Leaf leaf(.c(clk), .n(n));
            endmodule
            module Top(input logic clk, output int n);
                Mid mid(.clk(clk), .n(n));
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_below.sv"))], "Top");
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(sim.get(sim.signal("n")), BigUint::from(3u32));
    }

    fn host_clock_reaches_a_wait_through_a_continuous_assignment(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(input bit clk, output int n);
                wire copy;
                assign copy = clk;
                initial forever begin @(negedge copy); n++; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_copy.sv"))], "Top");
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(sim.get(sim.signal("n")), BigUint::from(3u32));
    }

    fn host_clock_reaches_a_wait_through_a_hierarchical_name(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Leaf(output int n);
                initial forever begin @(Top.clk); n++; end
            endmodule
            module Top(input bit clk, output int n);
                Leaf leaf(.n(n));
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_name.sv"))], "Top");
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(sim.get(sim.signal("n")), BigUint::from(6u32));
    }

    fn scheduled_values_satisfy_level_waits_on_inputs(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(input bit go, input bit a, input bit b, output int done, output int both);
                initial begin wait (go); done = 1; end
                initial begin wait (a && b); both = 1; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_level.sv"))], "Top");
        sim.schedule("a", 3, 1).unwrap();
        sim.schedule("go", 5, 1).unwrap();
        sim.schedule("b", 7, 1).unwrap();
        for (time, done, both) in [(4, 0u32, 0u32), (6, 1, 0), (8, 1, 1)] {
            sim.run_until(time).unwrap();
            assert_eq!(sim.get(sim.signal("done")), BigUint::from(done), "time {time}");
            assert_eq!(sim.get(sim.signal("both")), BigUint::from(both), "time {time}");
        }
    }

    fn host_clock_wakes_waits_in_tasks_and_always_procedures(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(input bit clk, input bit strobe, output int ticks, output int late, output int strobes);
                task automatic tick(); @(posedge clk); endtask
                initial forever begin tick(); ticks++; end
                // A procedure with a delay is a waiting process, not a flip-flop.
                always @(posedge strobe) begin #1 late++; strobes++; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_task.sv"))], "Top");
        sim.add_clock("clk", 10, 5);
        sim.add_clock("strobe", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(sim.get(sim.signal("ticks")), BigUint::from(3u32));
        assert_eq!(sim.get(sim.signal("late")), BigUint::from(3u32));
        assert_eq!(sim.get(sim.signal("strobes")), BigUint::from(3u32));
    }

    fn waited_host_clock_drives_the_clock_gated_from_it(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(input bit clk, input bit en, output int q, output int n);
                wire gated = clk & en;
                always_ff @(posedge gated) q <= q + 1;
                initial forever begin @(posedge clk); n++; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_gated.sv"))], "Top");
        let en = sim.signal("en");
        sim.add_clock("clk", 10, 5);
        sim.run_until(12).unwrap();
        sim.modify(|io| io.set(en, 1u8)).unwrap();
        sim.run_until(32).unwrap();
        // `en` is 1 for the rising edges at 15 and 25.
        assert_eq!(sim.get(sim.signal("n")), BigUint::from(3u32));
        assert_eq!(sim.get(sim.signal("q")), BigUint::from(2u32));
    }

    fn waited_input_drives_the_reset_computed_from_it(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(input bit clk, input bit rst_n, output bit [7:0] q, output int released);
                wire rst = ~rst_n;
                always_ff @(posedge clk or posedge rst) if (rst) q <= '0; else q <= q + 8'd1;
                initial forever begin @(posedge rst_n); released++; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_reset.sv"))], "Top")
            .scheduler_mode(celox::SchedulerMode::DifferentialRuns);
        // The wait makes `rst_n` an event beside the clock and the reset.
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk", "rst_n", "rst"]);
        // Rising edges of `clk` at 5, 15, ...; `rst` holds until 22, and
        // from 47 to 58.
        sim.add_clock("clk", 10, 5);
        sim.schedule("rst_n", 22, 1).unwrap();
        sim.schedule("rst_n", 47, 0).unwrap();
        sim.schedule("rst_n", 58, 1).unwrap();
        for (time, q, released) in [(20, 0u32, 0u32), (46, 3, 1), (50, 0, 1), (56, 0, 1), (80, 2, 2)] {
            sim.run_until(time).unwrap();
            assert_eq!(sim.get(sim.signal("q")), BigUint::from(q), "time {time}");
            assert_eq!(sim.get(sim.signal("released")), BigUint::from(released), "time {time}");
        }
        let stats = sim.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert!(stats.checked_steps > 0, "{stats:?}");
        assert!(stats.unchecked.is_none(), "{stats:?}");
        assert!(
            stats.kernels.iter().all(|kernel| *kernel == celox::KernelSchedule::Compiled),
            "{stats:?}"
        );
    }

    fn waited_input_clocking_flip_flops_below_keeps_their_edge(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Leaf(input logic c, output int q);
                always_ff @(negedge c) q <= q + 1;
            endmodule
            module Top(input logic clk, output int q, output int n);
                Leaf leaf(.c(clk), .q(q));
                initial forever begin @(posedge clk); n++; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_negedge.sv"))], "Top");
        // The flip-flops make `clk` a falling clock; the wait leaves it one.
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
        // Rising edges at 5, 15 and 25, falling edges at 10 and 20.
        sim.add_clock("clk", 10, 5);
        sim.run_until(27).unwrap();
        assert_eq!(sim.get(sim.signal("n")), BigUint::from(3u32));
        assert_eq!(sim.get(sim.signal("q")), BigUint::from(2u32));
    }

    fn waited_host_clock_beside_flip_flops_keeps_its_domain(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(input bit clk, output bit [7:0] q, output int n, output bit [7:0] seen);
                always_ff @(posedge clk) q <= q + 8'd1;
                // The wait resumes before the nonblocking update of the edge.
                initial forever begin @(posedge clk); n++; seen = q; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_ff_clock.sv"))], "Top");
        sim.add_clock("clk", 10, 5);
        sim.schedule("clk", 31, 1).unwrap();
        sim.run_until(32).unwrap();
        // Rising edges at 5, 15, 25 and 31.
        assert_eq!(sim.get(sim.signal("q")), BigUint::from(4u32));
        assert_eq!(sim.get(sim.signal("n")), BigUint::from(4u32));
        assert_eq!(sim.get(sim.signal("seen")), BigUint::from(3u32));
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
    }

    fn inputs_no_process_waits_on_are_no_events(sim) {
        @setup {
            let source = r#"`timescale 1ns/1ns
            module Top(
                input bit clk, input bit en, input bit d, input bit [1:0] sel,
                input bit a, input bit b,
                output bit y, output int gated, output int selected, output int both
            );
                assign y = d;
                // `en` gates the wait; it is not what the process waits for.
                initial forever begin @(posedge clk iff en); gated++; end
                // Only a one-bit signal is an event.
                initial forever begin @(sel); selected++; end
                // The process waits for the value of the expression.
                initial forever begin @(posedge (a & b)); both++; end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_inputs.sv"))], "Top");
        let events: Vec<String> = sim.named_events().into_iter().map(|event| event.name).collect();
        assert_eq!(events, ["clk"]);
        for port in ["en", "d", "sel", "a", "b"] {
            assert_eq!(
                sim.schedule(port, 1, 1),
                Err(celox::RuntimeErrorCode::NotAnEvent(port.to_string())),
            );
        }
        // The host still writes them, and the waits see its writes.
        let [en, sel, a, b] = ["en", "sel", "a", "b"].map(|name| sim.signal(name));
        sim.add_clock("clk", 10, 5);
        sim.run_until(12).unwrap();
        sim.modify(|io| {
            io.set(en, 1u8);
            io.set(sel, 2u8);
            io.set(a, 1u8);
            io.set(b, 1u8);
        }).unwrap();
        sim.run_until(32).unwrap();
        // `en` is 1 for the rising edges at 15 and 25.
        assert_eq!(sim.get(sim.signal("gated")), BigUint::from(2u32));
        assert_eq!(sim.get(sim.signal("selected")), BigUint::from(1u32));
        assert_eq!(sim.get(sim.signal("both")), BigUint::from(1u32));
    }
}

/// The waited input stays an event, and its waits keep seeing it, at every
/// optimization level.
#[test]
fn waited_host_clock_wakes_processes_at_each_optimization_level() {
    let source = edge_counters("bit");
    for level in [
        celox::OptLevel::O0,
        celox::OptLevel::O1,
        celox::OptLevel::O2,
    ] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source.as_str(), Path::new("waited_levels.sv"))],
            "Top",
        )
        .opt_level(level)
        .build()
        .unwrap();
        sim.add_clock("clk", 10, 5);
        sim.run_until(32).unwrap();
        assert_eq!(
            edge_counts!(sim),
            [3u32, 3, 6].map(BigUint::from),
            "{level:?}"
        );
    }
}

/// A design of several top-level modules (IEEE 1800-2023 23.3.1) names its
/// events by the top they belong to, and clocks them by their ids: the
/// waited input of one top as the flip-flop clock of another.
#[test]
fn waited_inputs_of_several_top_level_modules_are_events() {
    let source = r#"`timescale 1ns/1ns
        module First(input bit clk, output int n);
            always_ff @(posedge clk) n <= n + 1;
        endmodule
        module Second(input bit strobe, input bit d, output int n);
            initial forever begin @(strobe); n++; end
        endmodule"#;
    let mut sim =
        celox::Simulation::from_sv_sources(vec![(source, Path::new("waited_tops.sv"))], "First")
            .tops(["First", "Second"])
            .build()
            .unwrap();
    let events = sim.named_events();
    let names: Vec<&str> = events.iter().map(|event| event.name.as_str()).collect();
    assert_eq!(names, ["First.clk", "Second.strobe"]);
    for event in &events {
        sim.try_add_clock_by_id(event.id as u32, 10, 5).unwrap();
    }
    sim.run_until(32).unwrap();
    assert_eq!(sim.get(sim.signal("First.n")), BigUint::from(3u32));
    assert_eq!(sim.get(sim.signal("Second.n")), BigUint::from(6u32));
}
