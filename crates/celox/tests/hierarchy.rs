use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // For-loop instances: verify named_hierarchy groups them correctly
    // and child_signal access works.
fn test_for_loop_instance_hierarchy(sim) {
    @omit_veryl;
    @ignore_on(sv);
        @setup { let code = r#"
module Sub (
clk: input '_ clock,
i_data: input  logic<8>,
o_data: output logic<8>
) {
assign o_data = i_data + 8'h01;
}
module Top (
clk: input '_ clock,
rst: input reset,
top_in: input  logic<8>,
top_out: output logic<8>[2]
) {
for i in 0..2: g {
inst u_sub: Sub (
clk,
i_data: top_in,
o_data: top_out[i],
);
}
}
"#; }
        @build Simulator::builder(code, "Top");
    let hierarchy = sim.named_hierarchy();

    // Verify hierarchy structure
    assert_eq!(hierarchy.children.len(), 1, "should have 1 child group");
    let (child_name, instances) = &hierarchy.children[0];
    assert_eq!(child_name, "u_sub");
    assert_eq!(instances.len(), 2, "for-loop should produce 2 instances");
    assert_eq!(instances[0].module_name, "Sub");
    assert_eq!(instances[1].module_name, "Sub");

    // Verify child_signal access works for each for-loop instance
    let top_in = sim.signal("top_in");
    sim.modify(|io| io.set(top_in, 0x10u8)).unwrap();

    let child0_o = sim.child_signal(&[("u_sub", 0)], "o_data");
    let child1_o = sim.child_signal(&[("u_sub", 1)], "o_data");
    assert_eq!(sim.get(child0_o), 0x11u8.into());
    assert_eq!(sim.get(child1_o), 0x11u8.into());

    }

    fn test_flattened_instance_port_connection(sim) {
        @case "hierarchy::test_flattened_instance_port_connection";
    }

    fn test_instance_unpacked_array_slice_input(sim) {
        @ignore_on(sv);
        @case "hierarchy::test_instance_unpacked_array_slice_input";
    }

    fn test_instance_unpacked_array_slice_output(sim) {
        @ignore_on(sv);
        @case "hierarchy::test_instance_unpacked_array_slice_output";
    }

    fn test_instance_input_function_output_writeback(sim) {
        @ignore_on(sv);
        @case "hierarchy::test_instance_input_function_output_writeback";
    }

    fn test_instance_input_function_output_concat_dynamic_writeback(sim) {
        @ignore_on(veryl, sv);
        @case "hierarchy::test_instance_input_function_output_concat_dynamic_writeback";
    }

fn test_inactive_instance_input_output_call_adds_no_parent_driver(sim) {
    @ignore_on(sv);
    @case "hierarchy::test_inactive_instance_input_output_call_adds_no_parent_driver";
}

    fn test_instance_input_function_output_preserves_runtime_display(sim) {
        // veryl-simulator does not write the connection's function output
        // actual back.
        @omit_veryl;
        @ignore_on(sv);
        @setup { let code = r#"
module Child (
i: input logic,
o: output logic
) {
assign o = i;
}
module Top (
a: input logic,
child_o: output logic,
seen_o: output logic
) {
function write_seen (
x: input logic,
seen: output logic
) -> logic {
seen = x;
$display("seen=%0d", seen);
return x;
}
var seen: logic;
inst child: Child (
i: write_seen(a, seen),
o: child_o
);
assign seen_o = seen;
}
"#; }
        @build Simulator::builder(code, "Top");
    let a = sim.signal("a");
    let child_o = sim.signal("child_o");
    let seen_o = sim.signal("seen_o");
    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get(child_o), 1u8.into());
    assert_eq!(sim.get(seen_o), 1u8.into());
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "seen=1".to_string(),
        }],
    );

    }

    fn test_instance_output_dynamic_index_function_output_writeback(sim) {
        @ignore_on(veryl, sv);
        @case "hierarchy::test_instance_output_dynamic_index_function_output_writeback";
    }





    fn test_instance_output_dynamic_index_composes_aliasing_writeback(sim) {
        @ignore_on(veryl, sv);
        @case "hierarchy::test_instance_output_dynamic_index_composes_aliasing_writeback";
    }

    fn test_instance_output_concat_advances_each_destination(sim) {
        @ignore_on(veryl, sv);
        @case "hierarchy::test_instance_output_concat_advances_each_destination";
    }







    fn test_unconnected_child_output_needs_no_parent_glue(sim) {
        @case "hierarchy::test_unconnected_child_output_needs_no_parent_glue";
    }

    fn test_instance_input_port_assignment_width_context(sim) {
        @case "hierarchy::test_instance_input_port_assignment_width_context";
    }

fn test_dynamic_output_port_rmw_preserves_unselected_bits(sim) {
    // Upstream veryl-simulator still accepts this invalid destination.
    // The SV analyzer stops at unsupported indexed part-select, before checking
    // the output destination. That limitation is not a successful rejection.
    @ignore_on(veryl, sv);
    @case "hierarchy::test_dynamic_output_port_rmw_preserves_unselected_bits";
}

fn test_dynamic_output_port_converts_four_state_child_to_two_state_parent(sim) {
    // Upstream veryl-simulator still accepts this invalid destination.
    @ignore_on(veryl);
    @case "hierarchy::test_dynamic_output_port_converts_four_state_child_to_two_state_parent";
}

fn test_dynamic_minus_colon_output_port_rmw(sim) {
    // Upstream veryl-simulator still accepts this invalid destination.
    // The SV analyzer cannot analyze indexed part-selects yet.
    @ignore_on(veryl, sv);
    @case "hierarchy::test_dynamic_minus_colon_output_port_rmw";
}

fn test_dynamic_step_output_port_rmw(sim) {
    // Upstream veryl-simulator still accepts this invalid destination.
    // The SV analyzer cannot analyze indexed part-selects yet.
    @ignore_on(veryl, sv);
    @case "hierarchy::test_dynamic_step_output_port_rmw";
}

fn test_dynamic_prefix_colon_output_port_allows_zero_lsb(sim) {
    @omit_veryl;
    @case "hierarchy::test_dynamic_prefix_colon_output_port_allows_zero_lsb";
}

    fn test_multiple_instances_isolation(sim) {
        @case "hierarchy::test_multiple_instances_isolation";
    }

    fn test_deep_hierarchical_path_resolution(sim) {
        @case "hierarchy::test_deep_hierarchical_path_resolution";
    }

    fn test_constant_propagation_across_hierarchy(sim) {
        @case "hierarchy::test_constant_propagation_across_hierarchy";
    }

    fn test_hierarchical_concat_feedback_runtime(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_concat_feedback_runtime";
    }

    fn test_hierarchical_concat_feedback_runtime_multi_observe(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_concat_feedback_runtime_multi_observe";
    }

    fn test_hierarchical_concat_feedback_with_constant_middle_bit(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_concat_feedback_with_constant_middle_bit";
    }

    fn test_hierarchical_dynamic_index_feedback_runtime(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_dynamic_index_feedback_runtime";
    }

    fn test_hierarchical_dual_dynamic_readers_feedback_runtime(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_dual_dynamic_readers_feedback_runtime";
    }

    fn test_hierarchical_overlapping_partial_write_dynamic_index_runtime(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_overlapping_partial_write_dynamic_index_runtime";
    }

    fn test_hierarchical_concat_then_overlap_dynamic_index_runtime(sim) {
        @ignore_on(native, cranelift, wasm, interp, sv);
        @case "hierarchy::test_hierarchical_concat_then_overlap_dynamic_index_runtime";
    }

    fn test_child_signal_access(sim) {
        @case "hierarchy::test_child_signal_access";
    }

    fn test_named_hierarchy_structure(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Leaf (
i: input  logic,
o: output logic
) {
assign o = ~i;
}
module Mid (
i: input  logic,
o: output logic
) {
inst u_leaf: Leaf ( i: i, o: o );
}
module Top (
top_i: input  logic,
top_o: output logic
) {
inst u_mid: Mid ( i: top_i, o: top_o );
}
"#; }
        @build Simulator::builder(code, "Top");
    let hierarchy = sim.named_hierarchy();

    // Top-level module
    assert_eq!(hierarchy.module_name, "Top");
    assert!(hierarchy.signals.iter().any(|s| s.name == "top_i"));
    assert!(hierarchy.signals.iter().any(|s| s.name == "top_o"));

    // u_mid child
    assert_eq!(hierarchy.children.len(), 1);
    let (mid_name, mid_instances) = &hierarchy.children[0];
    assert_eq!(mid_name, "u_mid");
    assert_eq!(mid_instances.len(), 1);
    assert_eq!(mid_instances[0].module_name, "Mid");
    assert!(mid_instances[0].signals.iter().any(|s| s.name == "i"));
    assert!(mid_instances[0].signals.iter().any(|s| s.name == "o"));

    // u_leaf grandchild
    assert_eq!(mid_instances[0].children.len(), 1);
    let (leaf_name, leaf_instances) = &mid_instances[0].children[0];
    assert_eq!(leaf_name, "u_leaf");
    assert_eq!(leaf_instances.len(), 1);
    assert_eq!(leaf_instances[0].module_name, "Leaf");
    assert!(leaf_instances[0].signals.iter().any(|s| s.name == "i"));
    assert!(leaf_instances[0].signals.iter().any(|s| s.name == "o"));
    assert!(leaf_instances[0].children.is_empty());

    }

    fn test_named_hierarchy_multiple_instances(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Worker (
clk: input clock,
i_val: input  logic<8>,
o_val: output logic<8>
) {
var r_val: logic<8>;
always_ff { r_val = i_val; }
assign o_val = r_val;
}
module Top (
clk:  input clock,
in0:  input  logic<8>,
in1:  input  logic<8>,
out0: output logic<8>,
out1: output logic<8>
) {
inst u0: Worker ( clk: clk, i_val: in0, o_val: out0 );
inst u1: Worker ( clk: clk, i_val: in1, o_val: out1 );
}
"#; }
        @build Simulator::builder(code, "Top");
    let hierarchy = sim.named_hierarchy();

    assert_eq!(hierarchy.module_name, "Top");
    // Two separate children (u0 and u1), each with 1 instance
    assert_eq!(hierarchy.children.len(), 2);

    for (name, instances) in &hierarchy.children {
        assert!(name == "u0" || name == "u1");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].module_name, "Worker");
    }

    }

    fn test_instance_signals_child(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Sub (
i_data: input  logic<8>,
o_data: output logic<8>
) {
assign o_data = i_data + 8'h01;
}
module Top (
top_in:  input  logic<8>,
top_out: output logic<8>
) {
inst u_sub: Sub (
i_data: top_in,
o_data: top_out
);
}
"#; }
        @build Simulator::builder(code, "Top");

    let child_signals = sim.instance_signals(&[("u_sub", 0)]);
    assert!(!child_signals.is_empty());

    let names: Vec<&str> = child_signals.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"i_data"), "expected i_data in {:?}", names);
    assert!(names.contains(&"o_data"), "expected o_data in {:?}", names);

    }

    fn test_instance_signals_deep_hierarchy(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Leaf (
i: input  logic<8>,
o: output logic<8>
) {
assign o = i + 8'h01;
}
module Mid (
i: input  logic<8>,
o: output logic<8>
) {
inst u_leaf: Leaf ( i: i, o: o );
}
module Top (
top_i: input  logic<8>,
top_o: output logic<8>
) {
inst u_mid: Mid ( i: top_i, o: top_o );
}
"#; }
        @build Simulator::builder(code, "Top");

    // Get signals of the deeply nested leaf instance
    let leaf_signals = sim.instance_signals(&[("u_mid", 0), ("u_leaf", 0)]);
    assert!(!leaf_signals.is_empty());

    let names: Vec<&str> = leaf_signals.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"i"), "expected i in {:?}", names);
    assert!(names.contains(&"o"), "expected o in {:?}", names);

    // Verify we can read values through the resolved SignalRefs
    let top_i = sim.signal("top_i");
    sim.modify(|io| io.set(top_i, 0x42u8)).unwrap();

    let leaf_o = leaf_signals.iter().find(|s| s.name == "o").unwrap();
    assert_eq!(sim.get(leaf_o.signal), 0x43u8.into());

    }

    fn test_instance_signals_multiple_instances(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Worker (
i_val: input  logic<8>,
o_val: output logic<8>
) {
assign o_val = i_val + 8'h01;
}
module Top (
in0:  input  logic<8>,
in1:  input  logic<8>,
out0: output logic<8>,
out1: output logic<8>
) {
inst u0: Worker ( i_val: in0, o_val: out0 );
inst u1: Worker ( i_val: in1, o_val: out1 );
}
"#; }
        @build Simulator::builder(code, "Top");

    let signals_u0 = sim.instance_signals(&[("u0", 0)]);
    let signals_u1 = sim.instance_signals(&[("u1", 0)]);

    assert!(!signals_u0.is_empty());
    assert!(!signals_u1.is_empty());

    // Both instances should have the same signal names
    let names_u0: Vec<&str> = signals_u0.iter().map(|s| s.name.as_str()).collect();
    let names_u1: Vec<&str> = signals_u1.iter().map(|s| s.name.as_str()).collect();
    assert!(names_u0.contains(&"o_val"));
    assert!(names_u1.contains(&"o_val"));

    // But they should have different SignalRefs (different memory locations)
    let o_val_u0 = signals_u0.iter().find(|s| s.name == "o_val").unwrap();
    let o_val_u1 = signals_u1.iter().find(|s| s.name == "o_val").unwrap();
    assert_ne!(o_val_u0.signal, o_val_u1.signal);

    // Verify they read independently
    let in0 = sim.signal("in0");
    let in1 = sim.signal("in1");
    sim.modify(|io| {
        io.set(in0, 10u8);
        io.set(in1, 20u8);
    })
    .unwrap();
    assert_eq!(sim.get(o_val_u0.signal), 11u8.into());
    assert_eq!(sim.get(o_val_u1.signal), 21u8.into());

    }

    fn test_instance_signals_nonexistent_path(sim) {
        @omit_veryl;
        @setup { let code = r#"
module Top (
i: input  logic,
o: output logic
) {
assign o = i;
}
"#; }
        @build Simulator::builder(code, "Top");

    // Non-existent instance path should return empty Vec
    let signals = sim.instance_signals(&[("nonexistent", 0)]);
    assert!(signals.is_empty());

    // Deep non-existent path
    let signals = sim.instance_signals(&[("a", 0), ("b", 0), ("c", 0)]);
    assert!(signals.is_empty());

    }
}

fn assert_nonconstant_output_rejected(code: &str) {
    let error = match celox::Simulator::builder(code, "Top").build_interpreter() {
        Ok(_) => panic!("nonconstant output connection was accepted"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("output port destination must use constant indices and selects"),
        "{error}"
    );
}

#[test]
fn test_instance_output_dynamic_index_preserves_runtime_display() {
    let code = r#"
module Child (i: input logic, o: output logic) {
assign o = i;
}
module Top (
sel: input logic,
mem_o: output logic<2>,
tmp_o: output logic
) {
function choose_index (
x: input logic,
tmp: output logic
) -> logic {
tmp = x;
$display("index=%0d", x);
return x;
}
var mem: logic<2>;
var tmp: logic;
inst child: Child (
i: 1'b1,
o: mem[choose_index(sel, tmp)]
);
assign mem_o = mem;
assign tmp_o = tmp;
}
"#;
    assert_nonconstant_output_rejected(code);
}

#[test]
fn test_instance_output_index_runtime_effect_tracks_plain_sibling_source() {
    let code = r#"
module Child (i: input logic, o: output logic) {
assign o = i;
}
module Top (
d: input logic,
offset: input logic,
mem_o: output logic<2>
) {
function emit (x: input logic) -> logic {
$display("d=%0d", x);
return 1'b0;
}
var mem: logic<2>;
inst child: Child (
i: 1'b1,
o: mem[emit(d) + offset]
);
assign mem_o = mem;
}
"#;
    assert_nonconstant_output_rejected(code);
}

#[test]
fn test_instance_output_concat_runtime_effect_observes_prior_slice() {
    let code = r#"
module Child (i: input logic<2>, o: output logic<2>) {
assign o = i;
}
module Top (
value: input logic<2>,
mem_o: output logic<2>,
tmp_o: output logic
) {
function observe_index (x: input logic) -> logic {
$display("index=%0d", x);
return x;
}
var mem: logic<2>;
var tmp: logic;
inst child: Child (
i: value,
o: {mem[observe_index(tmp)], tmp}
);
assign mem_o = mem;
assign tmp_o = tmp;
}
"#;
    assert_nonconstant_output_rejected(code);
}

#[test]
fn test_instance_output_index_runtime_effect_triggers_on_child_change() {
    let code = r#"
module Child (i: input logic, o: output logic) {
assign o = i;
}
module Top (
data: input logic,
sel: input logic,
mem_o: output logic<2>
) {
function observe_index (x: input logic) -> logic {
$display("index=%0d", x);
return x;
}
var mem: logic<2>;
inst child: Child (
i: data,
o: mem[observe_index(sel)]
);
assign mem_o = mem;
}
"#;
    assert_nonconstant_output_rejected(code);
}

#[test]
fn test_instance_output_index_effect_triggers_when_two_state_parent_is_unchanged() {
    let code = r#"
module Child (mode: input logic<2>, o: output logic) {
always_comb {
case mode {
2'd0: o = 1'b0;
2'd1: o = 1'bx;
default: o = 1'bz;
}
}
}
module Top (
mode: input logic<2>,
index: input logic,
mem_o: output bit<2>
) {
function observe_index (x: input logic) -> logic {
$display("index=%0d", x);
return x;
}
var mem: bit<2>;
inst child: Child (
mode,
o: mem[observe_index(index)]
);
assign mem_o = mem;
}
"#;
    assert_nonconstant_output_rejected(code);
}
