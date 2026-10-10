//! Procedural timing controls: `#delay`, `@(event)` and `wait (condition)`
//! in `initial` blocks and `always` processes, compiled into resumable
//! kernels and driven by the timed `Simulation`.

use std::path::Path;

use celox::{RuntimeEvent, Simulation, Simulator};

fn simulation(source: &str) -> Simulation {
    Simulation::from_sv_sources(vec![(source, Path::new("processes.sv"))], "Top")
        .build()
        .unwrap()
}

fn four_state_simulation(source: &str) -> Simulation {
    Simulation::from_sv_sources(vec![(source, Path::new("processes.sv"))], "Top")
        .four_state(true)
        .build()
        .unwrap()
}

fn build_error(source: &str) -> String {
    match Simulator::from_sv_sources(vec![(source, Path::new("processes.sv"))], "Top")
        .build_cranelift()
    {
        Ok(_) => panic!("design unexpectedly compiled:\n{source}"),
        Err(error) => error
            .to_string()
            .split_whitespace()
            .filter(|word| *word != "│")
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Run to the end and collect every display with the time it was printed at.
fn displays(sim: &mut Simulation) -> Vec<(u64, String)> {
    let mut lines = Vec::new();
    while let Some(time) = sim.step().unwrap() {
        for event in sim.drain_runtime_events() {
            match event {
                RuntimeEvent::Display { message } => lines.push((time, message)),
                RuntimeEvent::Write { message } => lines.push((time, message)),
                other => panic!("unexpected runtime event {other:?}"),
            }
        }
        if sim.is_finished() {
            break;
        }
    }
    lines
}

fn lines(items: &[(u64, &str)]) -> Vec<(u64, String)> {
    items
        .iter()
        .map(|(time, text)| (*time, text.to_string()))
        .collect()
}

#[test]
fn delays_resume_initial_blocks_at_later_times() {
    let source = r#"
        module Top(output logic [7:0] a);
            parameter int GAP = 4;
            logic [7:0] pause = 8'd3;
            initial begin
                a = 8'd1;
                #10 a = 8'd2;
                #GAP a = 8'd3;
                #(pause) a = 8'd4;
                #0 a = 8'd5;
                #pause;
                $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let a = sim.signal("a");
    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(a), 1u8.into());
    assert_eq!(sim.step().unwrap(), Some(10));
    assert_eq!(sim.get(a), 2u8.into());
    assert_eq!(sim.step().unwrap(), Some(14));
    assert_eq!(sim.get(a), 3u8.into());
    assert_eq!(sim.step().unwrap(), Some(17));
    // The zero delay resumes at the same time.
    assert_eq!(sim.get(a), 5u8.into());
    assert!(!sim.is_finished());
    assert_eq!(sim.step().unwrap(), Some(20));
    assert!(sim.is_finished());
    assert_eq!(sim.step().unwrap(), None);
}

#[test]
fn always_with_a_delay_generates_a_clock() {
    let source = r#"
        module Top(output logic [7:0] count, output logic clk);
            initial clk = 1'b0;
            always #5 clk = ~clk;
            always_ff @(posedge clk) count <= count + 8'd1;
            initial count = 8'd0;
        endmodule
    "#;
    let mut sim = simulation(source);
    let count = sim.signal("count");
    let clk = sim.signal("clk");
    sim.run_until(4).unwrap();
    assert_eq!(sim.get(clk), 0u8.into());
    assert_eq!(sim.get(count), 0u8.into());
    // Rising edges at 5, 15, ..., 95.
    sim.run_until(100).unwrap();
    assert_eq!(sim.get(count), 10u8.into());
    assert_eq!(sim.get(clk), 0u8.into());
}

#[test]
fn edge_waits_resume_before_the_registers_of_that_edge_update() {
    let source = r#"
        module Top(output logic [7:0] q);
            logic clk = 1'b0;
            logic [7:0] d = 8'd0;
            always #5 clk = ~clk;
            always_ff @(posedge clk) q <= d;
            initial begin
                d = 8'd1;
                @(posedge clk);
                $display("first q=%0d", q);
                #1 d = 8'd2;
                @(posedge clk);
                $display("second q=%0d", q);
                @(negedge clk);
                $display("negedge q=%0d", q);
                repeat (2) @(posedge clk);
                $display("repeat q=%0d", q);
                $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    // A process woken by an edge runs in the active region, before the
    // nonblocking updates of that edge (IEEE 1800-2023 4.4): it reads the
    // old register value.
    assert_eq!(
        displays(&mut sim),
        lines(&[
            (5, "first q=0"),
            (15, "second q=1"),
            (20, "negedge q=2"),
            (35, "repeat q=2"),
        ])
    );
    assert_eq!(sim.time(), 35);
}

#[test]
fn blocking_writes_of_a_woken_process_reach_the_registers_of_that_edge() {
    let source = r#"
        module Top(output logic [7:0] q);
            logic clk = 1'b0;
            logic [7:0] d = 8'd0;
            always #5 clk = ~clk;
            always_ff @(posedge clk) q <= d;
            initial begin
                @(posedge clk);
                d = 8'd2;
                @(posedge clk);
                $display("q=%0d", q);
                $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    // The order of a woken process and the registers of the same edge is a
    // race in IEEE 1800-2023; Celox runs the process first, as Icarus
    // Verilog does, so the registers sample its blocking writes.
    assert_eq!(displays(&mut sim), lines(&[(15, "q=2")]));
}

#[test]
fn edge_waits_see_scheduled_clocks() {
    let source = r#"
        module Top(input logic clk, output logic [7:0] edges, output logic [7:0] count);
            always_ff @(posedge clk) count <= count + 8'd1;
            initial count = 8'd0;
            initial begin
                edges = 8'd0;
                forever begin
                    @(posedge clk);
                    edges = edges + 8'd1;
                end
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let edges = sim.signal("edges");
    let count = sim.signal("count");
    sim.add_clock("clk", 10, 0);
    sim.run_until(45).unwrap();
    // Rising edges at 0, 10, 20, 30 and 40. The edge at time zero is
    // applied before the process starts, so it is not one the process
    // waited for.
    assert_eq!(sim.get(edges), 4u8.into());
    assert_eq!(sim.get(count), 5u8.into());
}

#[test]
fn any_change_waits_wake_on_every_value_change() {
    let source = r#"
        module Top(output logic [7:0] data, output logic flag);
            logic [7:0] other;
            initial begin
                data = 8'd0;
                flag = 1'b0;
                other = 8'd0;
                #3 data = 8'd7;
                #4 data = 8'd7;
                #1 data = 8'd9;
                #2 other = 8'd1;
                #2 flag = 1'b1;
                #1 $finish;
            end
            initial forever begin
                @(data);
                $display("data=%0d", data);
            end
            initial forever begin
                @(other or flag);
                $display("other=%0d flag=%0d", other, flag);
            end
            initial forever begin
                @(flag, data);
                $display("either");
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    assert_eq!(
        displays(&mut sim),
        lines(&[
            (3, "data=7"),
            (3, "either"),
            (8, "data=9"),
            (8, "either"),
            (10, "other=1 flag=0"),
            (12, "other=1 flag=1"),
            (12, "either"),
        ])
    );
}

#[test]
fn wait_statements_block_until_their_condition_holds() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic go = 1'b0;
            logic [7:0] level = 8'd0;
            initial begin
                y = 8'd0;
                #7 go = 1'b1;
                #5 level = 8'd3;
                #5 level = 8'd5;
            end
            initial begin
                wait (go) $display("go");
                wait (go);
                $display("still go");
                wait (level > 8'd4) y = level;
                $display("level=%0d", level);
                #1 $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let y = sim.signal("y");
    assert_eq!(
        displays(&mut sim),
        lines(&[(7, "go"), (7, "still go"), (17, "level=5")])
    );
    assert_eq!(sim.get(y), 5u8.into());
}

#[test]
fn level_sensitive_always_runs_as_a_process() {
    let source = r#"
        module Top(output logic [7:0] y, output logic [7:0] runs);
            logic [7:0] a = 8'd0;
            logic [7:0] b = 8'd0;
            initial runs = 8'd0;
            always @(a or b) begin
                y = a + b;
                runs = runs + 8'd1;
            end
            initial begin
                #2 a = 8'd3;
                #2 b = 8'd4;
                #2 a = 8'd3;
                #2 $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let y = sim.signal("y");
    let runs = sim.signal("runs");
    sim.step().unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.run_until(3).unwrap();
    assert_eq!(sim.get(y), 3u8.into());
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(y), 7u8.into());
    sim.run_until(10).unwrap();
    assert!(sim.is_finished());
    // An unchanged value is not an event.
    assert_eq!(sim.get(runs), 2u8.into());
}

#[test]
fn processes_wake_each_other_at_the_same_time() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic start = 1'b0;
            logic done = 1'b0;
            initial begin
                y = 8'd0;
                #3 start = 1'b1;
                @(posedge done);
                $display("done at y=%0d", y);
                $finish;
            end
            initial begin
                @(posedge start);
                y = 8'd1;
                #0 y = 8'd2;
                done = 1'b1;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    assert_eq!(displays(&mut sim), lines(&[(3, "done at y=2")]));
}

#[test]
fn tasks_with_timing_controls_run_inline() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic clk = 1'b0;
            always #5 clk = ~clk;
            task automatic pulse(input logic [7:0] value, input int cycles);
                y = value;
                repeat (cycles) @(posedge clk);
                #1 y = 8'd0;
            endtask
            initial begin
                y = 8'd0;
                pulse(8'd7, 2);
                $display("y=%0d", y);
                pulse(8'd9, 1);
                $display("y=%0d", y);
                $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let y = sim.signal("y");
    sim.run_until(10).unwrap();
    assert_eq!(sim.get(y), 7u8.into());
    assert_eq!(displays(&mut sim), lines(&[(16, "y=0"), (26, "y=0")]));
    assert_eq!(sim.time(), 26);
}

#[test]
fn edge_waits_on_a_clock_the_testbench_drives() {
    let source = r#"
        module Top(input logic a, output logic [7:0] seen);
            initial begin
                seen = 8'd0;
                forever begin
                    @(posedge a);
                    seen = seen + 8'd1;
                end
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let a = sim.signal("a");
    let seen = sim.signal("seen");
    sim.step().unwrap();
    // A host write wakes a waiting process when the simulation next runs.
    for value in [1u8, 0, 1, 1, 0] {
        sim.modify(|io| io.set(a, value)).unwrap();
        sim.run_until(sim.time()).unwrap();
    }
    assert_eq!(sim.get(seen), 2u8.into());
    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(seen), 2u8.into());
    assert_eq!(sim.step().unwrap(), None);
    assert_eq!(sim.get(seen), 3u8.into());
}

#[test]
fn four_state_edges_count_transitions_through_unknown() {
    let source = r#"
        module Top(output logic [7:0] pos, output logic [7:0] neg, output logic [7:0] any);
            logic s;
            initial begin
                pos = 8'd0;
                neg = 8'd0;
                any = 8'd0;
                s = 1'b0;
                #1 s = 1'bx; // 0 -> x: posedge
                #1 s = 1'b1; // x -> 1: posedge
                #1 s = 1'bz; // 1 -> z: negedge
                #1 s = 1'bx; // z -> x: no edge, a change
                #1 s = 1'b0; // x -> 0: negedge
                #1 s = 1'b1; // 0 -> 1: posedge
                #1 s = 1'b0; // 1 -> 0: negedge
                #1 $finish;
            end
            initial forever begin
                @(posedge s);
                pos = pos + 8'd1;
            end
            initial forever begin
                @(negedge s);
                neg = neg + 8'd1;
            end
            initial forever begin
                @(s);
                any = any + 8'd1;
            end
        endmodule
    "#;
    let mut sim = four_state_simulation(source);
    let pos = sim.signal("pos");
    let neg = sim.signal("neg");
    let any = sim.signal("any");
    sim.run_until(100).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.get(pos), 3u8.into());
    assert_eq!(sim.get(neg), 3u8.into());
    assert_eq!(sim.get(any), 7u8.into());
}

#[test]
fn unknown_delays_and_conditions_count_as_zero_and_false() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic [7:0] amount = 8'bx;
            logic flag = 1'bx;
            initial begin
                y = 8'd1;
                #amount y = 8'd2;
                wait (flag) y = 8'd3;
                $finish;
            end
            initial #4 flag = 1'b1;
        endmodule
    "#;
    let mut sim = four_state_simulation(source);
    let y = sim.signal("y");
    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(y), 2u8.into());
    assert_eq!(sim.step().unwrap(), Some(4));
    assert_eq!(sim.get(y), 3u8.into());
    assert!(sim.is_finished());
}

#[test]
fn checkpoints_restore_processes_waiting_for_events() {
    let source = r#"
        module Top(output logic [7:0] edges, output logic clk);
            initial clk = 1'b0;
            always #5 clk = ~clk;
            initial begin
                edges = 8'd0;
                forever begin
                    @(posedge clk);
                    edges = edges + 8'd1;
                end
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let edges = sim.signal("edges");
    sim.run_until(22).unwrap();
    assert_eq!(sim.get(edges), 2u8.into());
    let checkpoint = sim.checkpoint().unwrap();
    sim.run_until(52).unwrap();
    assert_eq!(sim.get(edges), 5u8.into());
    sim.restore(&checkpoint).unwrap();
    assert_eq!(sim.time(), 22);
    assert_eq!(sim.get(edges), 2u8.into());
    sim.run_until(52).unwrap();
    assert_eq!(sim.get(edges), 5u8.into());
}

#[test]
fn finish_in_an_always_process_ends_the_simulation() {
    let source = r#"
        module Top(output logic [7:0] ticks);
            initial ticks = 8'd0;
            always begin
                #3 ticks = ticks + 8'd1;
                if (ticks == 8'd4) $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let ticks = sim.signal("ticks");
    sim.run_until(100).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.time(), 12);
    assert_eq!(sim.get(ticks), 4u8.into());
}

#[test]
fn waiting_processes_do_not_keep_an_idle_simulation_running() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic never = 1'b0;
            initial begin
                y = 8'd1;
                @(posedge never);
                y = 8'd2;
            end
            initial #5 y = 8'd3;
        endmodule
    "#;
    let mut sim = simulation(source);
    let y = sim.signal("y");
    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.step().unwrap(), Some(5));
    assert_eq!(sim.get(y), 3u8.into());
    assert_eq!(sim.next_event_time(), None);
    assert_eq!(sim.step().unwrap(), None);
    assert!(!sim.is_finished());
}

#[test]
fn rejects_timing_controls_outside_processes() {
    let error = build_error(
        r#"module Top(input logic clk, input logic d, output logic q);
            always_ff @(posedge clk) begin
                q <= d;
                #1 q <= ~d;
            end
        endmodule"#,
    );
    assert!(error.contains("delay outside a process"), "{error}");

    let error = build_error(
        r#"module Top(input logic a, output logic y);
            always_comb begin
                wait (a) y = a;
            end
        endmodule"#,
    );
    assert!(
        error.contains("procedural timing control outside a process"),
        "{error}"
    );

    let error = build_error(
        r#"module Top(input logic a, output logic y);
            always y = a;
        endmodule"#,
    );
    assert!(
        error.contains("always and always_latch processes"),
        "{error}"
    );

    let error = build_error(
        r#"module Top(input logic a, output logic y);
            initial begin
                @* y = a;
            end
        endmodule"#,
    );
    assert!(error.contains("`@*` in a process"), "{error}");

    let error = build_error(
        r#"module Top(input logic a, output logic y);
            initial begin
                @(posedge a iff y) y = a;
            end
        endmodule"#,
    );
    assert!(error.contains("iff-qualified event"), "{error}");

    let error = build_error(
        r#"module Top(input logic a, output logic y);
            initial begin
                #1.5 y = a;
            end
        endmodule"#,
    );
    assert!(error.contains("delay value"), "{error}");

    let error = build_error(
        r#"module Top(input logic clk, input logic a, output logic y);
            always @(posedge clk) begin
                y <= a;
                #1;
            end
        endmodule"#,
    );
    assert!(
        error.contains("nonblocking assignment in a process that runs with timing"),
        "{error}"
    );
}

/// IEEE 1800-2023 9.4.2: a waiter is woken by the first change, even when
/// the writing process restores the value before it suspends.
#[test]
fn events_hidden_by_a_later_store_of_the_same_run_still_wake_waiters() {
    let source = r#"
        module Top(output logic [7:0] woken);
            logic a = 1'b0;
            logic b = 1'b0;
            initial begin
                woken = 8'd0;
                #1 a = 1'b1;
                a = 1'b0;
                #1 b = 1'b1;
                b = 1'b0;
                b = 1'b1;
                #1 $finish;
            end
            initial begin
                @(a);
                $display("a changed a=%0d", a);
                woken = woken + 8'd1;
            end
            initial begin
                @(posedge a);
                $display("a rose");
                woken = woken + 8'd1;
            end
            initial begin
                @(negedge b);
                $display("b fell b=%0d", b);
                woken = woken + 8'd1;
            end
            initial begin
                @(a or b);
                $display("a or b");
                woken = woken + 8'd1;
            end
            initial begin
                @(a ^ 1'b0);
                $display("a xor");
                woken = woken + 8'd1;
            end
            initial begin
                @(negedge (b & 1'b1));
                $display("b and fell");
                woken = woken + 8'd1;
            end
        endmodule
    "#;
    for (four_state, mut sim) in [
        (false, simulation(source)),
        (true, four_state_simulation(source)),
    ] {
        let woken = sim.signal("woken");
        assert_eq!(
            displays(&mut sim),
            lines(&[
                (1, "a changed a=0"),
                (1, "a rose"),
                (1, "a or b"),
                (1, "a xor"),
                (2, "b fell b=1"),
                (2, "b and fell"),
            ]),
            "four_state={four_state}"
        );
        assert_eq!(sim.get(woken), 6u8.into(), "four_state={four_state}");
    }
}

/// Each process has its own activation of an `automatic` task that
/// suspends, declared on the task or as the module's default lifetime.
#[test]
fn concurrent_activations_of_a_timed_task_keep_their_arguments() {
    let source = r#"
        module Top(output logic [7:0] o0, output logic [7:0] o1, output logic [7:0] sum);
            task automatic put(input logic [7:0] value, input int index);
                logic [7:0] doubled;
                doubled = value + value;
                #1;
                if (index == 0) o0 = doubled;
                else o1 = doubled;
                sum = sum + value;
            endtask
            initial begin
                sum = 8'd0;
                o0 = 8'd0;
                o1 = 8'd0;
            end
            initial put(8'd1, 0);
            initial put(8'd2, 1);
            initial begin
                #2 $finish;
            end
        endmodule
    "#;
    let module_default = source
        .replace("module Top(", "module automatic Top(")
        .replace("task automatic put(", "task put(");
    for source in [source.to_string(), module_default] {
        let mut sim = simulation(&source);
        let (o0, o1, sum) = (sim.signal("o0"), sim.signal("o1"), sim.signal("sum"));
        sim.run_until(5).unwrap();
        assert!(sim.is_finished());
        assert_eq!(sim.get(o0), 2u8.into());
        assert_eq!(sim.get(o1), 4u8.into());
        assert_eq!(sim.get(sum), 3u8.into());
    }

    // A static task (the default) shares its formals and locals: both
    // activations resume with the later caller's values (IEEE 1800-2023
    // 13.3.1).
    let static_task = source.replace("task automatic put(", "task put(");
    let mut sim = simulation(&static_task);
    let (o0, o1, sum) = (sim.signal("o0"), sim.signal("o1"), sim.signal("sum"));
    sim.run_until(5).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.get(o0), 0u8.into());
    assert_eq!(sim.get(o1), 4u8.into());
    assert_eq!(sim.get(sum), 4u8.into());
}

/// A wait on a formal of an `automatic` task counts the events of this
/// activation's formal only: another activation's argument does not wake it.
#[test]
fn waits_on_private_formals_see_only_their_activation() {
    let source = r#"
        module Top(output logic [7:0] woken);
            task automatic watch(input logic value);
                @(value);
                woken = woken + 8'd1;
            endtask
            initial begin
                woken = 8'd0;
                watch(1'b0);
            end
            initial begin
                #1 watch(1'b1);
            end
            initial begin
                #3 $finish;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let woken = sim.signal("woken");
    sim.run_until(5).unwrap();
    assert!(sim.is_finished());
    assert_eq!(sim.get(woken), 0u8.into());
}

/// A task a package exports is classified like a local one: an
/// edge-sensitive `always` calling an imported task with timing controls is
/// a process.
#[test]
fn edge_sensitive_always_calling_an_imported_timed_task_runs_as_a_process() {
    let source = r#"
        package p;
            task automatic bump(inout logic [7:0] count);
                #1 count = count + 8'd1;
            endtask
        endpackage
        module Top(output logic [7:0] y, output logic [7:0] z);
            import p::*;
            logic clk = 1'b0;
            always #5 clk = ~clk;
            initial begin
                y = 8'd0;
                z = 8'd0;
            end
            always @(posedge clk) bump(y);
            always @(posedge clk) p::bump(z);
        endmodule
    "#;
    let mut sim = simulation(source);
    let (y, z) = (sim.signal("y"), sim.signal("z"));
    sim.run_until(20).unwrap();
    assert_eq!(sim.get(y), 2u8.into());
    assert_eq!(sim.get(z), 2u8.into());
}

/// An edge-sensitive `always` whose body reaches a timing control through a
/// task is a process.
#[test]
fn edge_sensitive_always_calling_a_timed_task_runs_as_a_process() {
    let source = r#"
        module Top(output logic [7:0] y);
            logic clk = 1'b0;
            always #5 clk = ~clk;
            task automatic bump;
                #1 y = y + 8'd1;
            endtask
            task automatic via;
                bump();
            endtask
            initial y = 8'd0;
            always @(posedge clk) via();
        endmodule
    "#;
    let mut sim = simulation(source);
    let y = sim.signal("y");
    // Edges at 5 and 15 increment at 6 and 16.
    sim.run_until(5).unwrap();
    assert_eq!(sim.get(y), 0u8.into());
    sim.run_until(6).unwrap();
    assert_eq!(sim.get(y), 1u8.into());
    sim.run_until(20).unwrap();
    assert_eq!(sim.get(y), 2u8.into());
}

#[test]
fn rejects_nonblocking_assignments_in_tasks_a_process_calls() {
    let source = r#"
        module Top(output logic [7:0] q);
            task automatic later;
                #1 q <= 8'd1;
            endtask
            initial later();
        endmodule
    "#;
    assert!(
        build_error(source).contains("nonblocking assignment in a process that runs with timing"),
        "{}",
        build_error(source)
    );
}

/// `Simulation::settle` resumes the processes a host write woke at the
/// current time, as a simulator's settle step does, without advancing time.
#[test]
fn settle_resumes_the_processes_a_host_write_wakes() {
    let source = r#"
        module Top(input logic go, output logic [7:0] seen);
            initial begin
                seen = 8'd0;
                wait (go);
                seen = 8'd1;
                #0 seen = 8'd2;
            end
        endmodule
    "#;
    let mut sim = simulation(source);
    let (go, seen) = (sim.signal("go"), sim.signal("seen"));
    assert_eq!(sim.step().unwrap(), Some(0));
    assert_eq!(sim.get(seen), 0u8.into());
    sim.set_wide(go, 1u8.into());
    sim.settle().unwrap();
    assert_eq!(sim.get(seen), 2u8.into());
    assert_eq!(sim.time(), 0);
}
