use super::*;

sv_backends! {
    fn reads_signals_of_instances_below(sim) {
        @case "hierarchical_references::reads_signals_of_instances_below";
    }
    fn writes_memory_elements_of_instances_below(sim) {
        @case "hierarchical_references::writes_memory_elements_of_instances_below";
    }
    fn names_instance_array_elements_and_generated_instances(sim) {
        @case "hierarchical_references::names_instance_array_elements_and_generated_instances";
    }
    fn reads_signals_of_enclosing_instances_by_module_name(sim) {
        @case "hierarchical_references::reads_signals_of_enclosing_instances_by_module_name";
    }
    fn foreign_writes_interleave_with_nonblocking_element_writes(sim) {
        @case "hierarchical_references::foreign_writes_interleave_with_nonblocking_element_writes";
    }
    fn mutual_hierarchical_writes_next_to_nonblocking_writes(sim) {
        @case "hierarchical_references::mutual_hierarchical_writes_next_to_nonblocking_writes";
    }
    fn foreign_bit_writes_survive_nonblocking_writes_of_other_bits(sim) {
        @case "hierarchical_references::foreign_bit_writes_survive_nonblocking_writes_of_other_bits";
    }
    fn names_instance_array_elements_by_constant_expressions(sim) {
        @case "hierarchical_references::names_instance_array_elements_by_constant_expressions";
    }
    fn initial_writes_through_names_follow_declaration_initializers(sim) {
        @case "hierarchical_references::initial_writes_through_names_follow_declaration_initializers";
    }
    fn upward_names_resolve_in_each_parameterized_parent(sim) {
        @case "hierarchical_references::upward_names_resolve_in_each_parameterized_parent";
    }
    fn subroutines_write_through_hierarchical_names(sim) {
        @case "hierarchical_references::subroutines_write_through_hierarchical_names";
    }
    fn foreign_inactive_writes_precede_nonblocking_bit_writes(sim) {
        @case "hierarchical_references::foreign_inactive_writes_precede_nonblocking_bit_writes";
    }
    fn waits_through_names_see_intermediate_combinational_writes(sim) {
        @case "hierarchical_references::waits_through_names_see_intermediate_combinational_writes";
    }
    fn input_arguments_read_through_hierarchical_names(sim) {
        @case "hierarchical_references::input_arguments_read_through_hierarchical_names";
    }
    fn names_from_the_module_name_denote_its_own_signals(sim) {
        @case "hierarchical_references::names_from_the_module_name_denote_its_own_signals";
    }
    fn names_in_inactive_generate_branches_are_ignored(sim) {
        @case "hierarchical_references::names_in_inactive_generate_branches_are_ignored";
    }
    fn signed_elements_through_hierarchical_names(sim) {
        @case "hierarchical_references::signed_elements_through_hierarchical_names";
    }
}

fn displayed(source: &str) -> Vec<String> {
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    sim.run_until(1_000_000).unwrap();
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect()
}

fn build_error(source: &str) -> String {
    Simulator::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .build_cranelift()
        .expect_err("the design must be rejected")
        .to_string()
        // The report wraps long messages.
        .replace('│', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// `$root` names the top instance from anywhere in the hierarchy (IEEE
// 1800-2023 23.3.1), including as an edge event of a flip-flop.
#[test]
fn root_names_start_at_the_top_instance() {
    let source = r#"
        `timescale 1ns/1ns
        module Leaf;
            logic [7:0] ticks = 8'd0;
            always @(posedge $root.Top.clk) ticks <= ticks + 8'd1;
        endmodule
        module Top;
            logic clk = 0;
            Leaf l0();
            Leaf l1();
            always #5 clk = ~clk;
            initial begin
                #22 $display("%0d %0d", $root.Top.l0.ticks, l1.ticks);
                $finish;
            end
        endmodule
    "#;
    assert_eq!(displayed(source), ["2 2"]);
}

#[test]
fn rejects_unsupported_hierarchical_drivers_by_name() {
    let child = "module Child; logic [7:0] v; wire [7:0] n = 8'd3; endmodule";
    let error = build_error(&format!(
        "{child} module Top(input logic [7:0] a); Child c(); assign c.v = a; endmodule"
    ));
    assert!(
        error.contains("continuous assignment to a variable through hierarchical name `c.v`"),
        "{error}"
    );
    let error = build_error(&format!(
        "{child} module Top; Child c(); initial c.n = 8'd1; endmodule"
    ));
    assert!(
        error.contains("driver of a net through hierarchical name `c.n`"),
        "{error}"
    );
}

#[test]
fn rejects_names_that_denote_no_signal() {
    let error = build_error(
        "module Child; logic v; endmodule \
         module Top; logic y; Child c(); initial y = c.missing; endmodule",
    );
    assert!(
        error.contains("unresolved hierarchical reference `c.missing`"),
        "{error}"
    );
}

#[test]
fn rejects_hierarchical_writes_next_to_exclusive_drivers() {
    let error = build_error(
        "module C(input logic [7:0] a); logic [7:0] v; assign v = a; endmodule \
         module Top; logic [7:0] a = 1; C c(.a(a)); initial #1 c.v = 8'd5; endmodule",
    );
    assert!(
        error.contains("which another module writes through a hierarchical name: its module drives it continuously"),
        "{error}"
    );
    let error = build_error(
        "module C(input logic clk); logic [7:0] v = 0; always @(posedge clk) v <= v + 1; endmodule \
         module Top; logic clk = 0; C c(.clk(clk)); always_ff @(posedge clk) c.v <= 8'd9; endmodule",
    );
    assert!(error.contains("an always_ff process writes it"), "{error}");
    // Two repeating writers of one packed variable would lose bits.
    let error = build_error(
        "module C(input logic clk, input logic [2:0] idx); logic [7:0] q = 0; \
         always @(posedge clk) q[idx] <= 1'b1; endmodule \
         module Top; logic clk = 0; logic [2:0] idx = 7; C c(.clk(clk), .idx(idx)); \
         always @(posedge clk) c.q[0] = 1'b1; endmodule",
    );
    assert!(error.contains("another process writes it too"), "{error}");
}

// Names in a module with an interface port resolve in the module as
// written, not in the specialization made for the interface, including
// names that start at its own name.
#[test]
fn names_resolve_below_instances_with_interface_ports() {
    let source = r#"
        `timescale 1ns/1ns
        interface Bus; logic [7:0] d; endinterface
        module C; logic [7:0] v = 8'd4; endmodule
        module Leaf(interface bus);
            C c();
            bit q = 1;
            initial #1 $display("%0d %0d", c.v, Leaf.q);
        endmodule
        module Top; Bus b(); Leaf l(.bus(b)); endmodule
    "#;
    assert_eq!(displayed(source), ["4 1"]);
}

// A forced signal that is the target of a hierarchical name stays forceable
// in the specialization the name resolution makes.
#[test]
fn forced_targets_of_hierarchical_names_stay_forceable() {
    let source = r#"
        `timescale 1ns/1ns
        module C; logic [7:0] v = 8'd4; logic [7:0] q = 8'd1; endmodule
        module Top;
            C c();
            initial #1 $display("%0d %0d", c.v, c.q);
        endmodule
    "#;
    let mut sim = celox::Simulation::from_sv_sources(vec![(source, Path::new("hier.sv"))], "Top")
        .native_force_support(true)
        .signal_access(celox::SignalAccessOptions {
            force_signals: vec!["Top.c.q".into()],
            ..Default::default()
        })
        .build_cranelift()
        .unwrap();
    sim.run_until(10).unwrap();
    let displayed: Vec<_> = sim
        .drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => Some(String::from_utf8(message).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(displayed, ["4 1"]);
}
