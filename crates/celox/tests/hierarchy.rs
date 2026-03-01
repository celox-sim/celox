use celox::Simulator;

#[test]
fn test_flattened_instance_port_connection() {
    let code = r#"
        module Sub (
            i_data: input  logic<8>,
            o_data: output logic<8>
        ) {
            assign o_data = i_data;
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
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let top_in = sim.signal("top_in");
    let top_out = sim.signal("top_out");

    sim.modify(|io| io.set(top_in, 0x55u8)).unwrap();
    assert_eq!(sim.get(top_out), 0x55u8.into());
}

#[test]
fn test_multiple_instances_isolation() {
    let code = r#"
        module Worker (
            clk: input clock,
            i_val: input logic<8>,
            o_val: output logic<8>
        ) {
            var internal_reg: logic<8>;
            always_ff {
                internal_reg = i_val + 1;
            }
            assign o_val = internal_reg;
        }

        module Top (
            clk: input clock,
            in0: input logic<8>,
            in1: input logic<8>,
            out0: output logic<8>,
            out1: output logic<8>
        ) {
            inst u0: Worker ( clk: clk, i_val: in0, o_val: out0 );
            inst u1: Worker ( clk: clk, i_val: in1, o_val: out1 );
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let clk = sim.event("clk");
    let in0 = sim.signal("in0");
    let in1 = sim.signal("in1");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");

    sim.modify(|io| {
        io.set(in0, 10u8);
        io.set(in1, 20u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    assert_eq!(sim.get(out0), 11u8.into());
    assert_eq!(sim.get(out1), 21u8.into());
}

#[test]
fn test_deep_hierarchical_path_resolution() {
    let code = r#"
        module Leaf ( i: input logic, o: output logic ) {
            assign o = ~i;
        }
        module Mid ( i: input logic, o: output logic ) {
            inst u_leaf: Leaf ( i: i, o: o );
        }
        module Top ( top_i: input logic, top_o: output logic ) {
            inst u_mid: Mid ( i: top_i, o: top_o );
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let top_i = sim.signal("top_i");
    let top_o = sim.signal("top_o");

    sim.modify(|io| io.set(top_i, 1u8)).unwrap();
    assert_eq!(sim.get(top_o), 0u8.into());
}

#[test]
fn test_constant_propagation_across_hierarchy() {
    let code = r#"
        module Sub ( i: input logic<8>, o: output logic<8> ) {
            assign o = i + 8'h01;
        }
        module Top ( o: output logic<8> ) {
            inst u_sub: Sub ( i: 8'h0F, o: o );
        }
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let o = sim.signal("o");

    sim.modify(|_| {}).unwrap();
    assert_eq!(sim.get(o), 0x10u8.into());
}

#[test]
fn test_hierarchical_concat_feedback_runtime() {
    let code = r#"
        module Child (
            a: input logic<2>,
            lo: output logic,
        ) {
            assign lo = a[1];
        }

        module Top (
            inp: input logic,
            out: output logic,
        ) {
            var v: logic<2>;
            var lo: logic;

            inst c: Child (
                a: v,
                lo: lo,
            );

            assign v = {inp, lo};
            assign out = v[0];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let inp = sim.signal("inp");
    let out = sim.signal("out");

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());

    sim.modify(|io| io.set(inp, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u8.into());

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());
}

#[test]
fn test_hierarchical_concat_feedback_runtime_multi_observe() {
    let code = r#"
        module Child (
            a: input logic<2>,
            lo: output logic,
        ) {
            assign lo = a[1];
        }

        module Top (
            inp: input logic,
            out0: output logic,
            out1: output logic,
        ) {
            var v: logic<2>;
            var lo: logic;

            inst c: Child (
                a: v,
                lo: lo,
            );

            assign v = {inp, lo};
            assign out0 = v[0];
            assign out1 = v[1];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let inp = sim.signal("inp");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");

    for bit in [0u8, 1u8, 0u8, 1u8] {
        sim.modify(|io| io.set(inp, bit)).unwrap();
        assert_eq!(sim.get(out0), bit.into());
        assert_eq!(sim.get(out1), bit.into());
    }
}

#[test]
fn test_hierarchical_concat_feedback_with_constant_middle_bit() {
    let code = r#"
        module Child (
            a: input logic<3>,
            lo: output logic,
        ) {
            assign lo = a[2];
        }

        module Top (
            inp: input logic,
            out: output logic,
            mid: output logic,
        ) {
            var v: logic<3>;
            var lo: logic;

            inst c: Child (
                a: v,
                lo: lo,
            );

            assign v = {inp, 1'b0, lo};
            assign out = v[0];
            assign mid = v[1];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let inp = sim.signal("inp");
    let out = sim.signal("out");
    let mid = sim.signal("mid");

    sim.modify(|io| io.set(inp, 0u8)).unwrap();
    assert_eq!(sim.get(out), 0u8.into());
    assert_eq!(sim.get(mid), 0u8.into());

    sim.modify(|io| io.set(inp, 1u8)).unwrap();
    assert_eq!(sim.get(out), 1u8.into());
    assert_eq!(sim.get(mid), 0u8.into());
}

#[test]
fn test_hierarchical_dynamic_index_feedback_runtime() {
    let code = r#"
        module ChildFb (
            a: input logic<3>,
            lo: output logic,
        ) {
            assign lo = a[2];
        }

        module ChildDyn (
            a: input logic<3>,
            idx: input logic,
            o: output logic,
        ) {
            assign o = a[idx];
        }

        module Top (
            i1: input logic,
            i2: input logic,
            sel: input logic,
            out_fb: output logic,
            out_dyn: output logic,
        ) {
            var v: logic<3>;
            var lo: logic;
            var d: logic;

            inst fb: ChildFb (
                a: v,
                lo: lo,
            );

            inst dyn: ChildDyn (
                a: v,
                idx: sel,
                o: d,
            );

            // Instance-crossing feedback (fb) and dynamic index access (dyn).
            // lo = v[2], while v is built by split assignments so bit-dependencies are precise.
            assign v[2:1] = {i2, i1};
            assign v[0] = lo;
            assign out_fb = lo;
            assign out_dyn = d;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel = sim.signal("sel");
    let out_fb = sim.signal("out_fb");
    let out_dyn = sim.signal("out_dyn");

    sim.modify(|io| {
        io.set(i1, 1u8);
        io.set(i2, 0u8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    sim.modify(|io| {
        io.set(i1, 0u8);
        io.set(i2, 1u8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());
}

#[test]
fn test_hierarchical_dual_dynamic_readers_feedback_runtime() {
    let code = r#"
        module ChildFb (
            a: input logic<3>,
            lo: output logic,
        ) {
            assign lo = a[2];
        }

        module ChildDyn (
            a: input logic<3>,
            idx: input logic,
            o: output logic,
        ) {
            assign o = a[idx];
        }

        module Top (
            i1: input logic,
            i2: input logic,
            sel0: input logic,
            sel1: input logic,
            out_fb: output logic,
            out0: output logic,
            out1: output logic,
        ) {
            var v: logic<3>;
            var lo: logic;
            var d0: logic;
            var d1: logic;

            inst fb: ChildFb (
                a: v,
                lo: lo,
            );

            inst dyn0: ChildDyn (
                a: v,
                idx: sel0,
                o: d0,
            );

            inst dyn1: ChildDyn (
                a: v,
                idx: sel1,
                o: d1,
            );

            assign v[2:1] = {i2, i1};
            assign v[0] = lo;

            assign out_fb = lo;
            assign out0 = d0;
            assign out1 = d1;
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel0 = sim.signal("sel0");
    let sel1 = sim.signal("sel1");
    let out_fb = sim.signal("out_fb");
    let out0 = sim.signal("out0");
    let out1 = sim.signal("out1");

    sim.modify(|io| {
        io.set(i1, 1u8);
        io.set(i2, 0u8);
        io.set(sel0, 0u8);
        io.set(sel1, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out0), 0u8.into());
    assert_eq!(sim.get(out1), 1u8.into());

    sim.modify(|io| io.set(sel0, 1u8)).unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out0), 1u8.into());
    assert_eq!(sim.get(out1), 1u8.into());

    sim.modify(|io| {
        io.set(i2, 1u8);
        io.set(sel1, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out0), 1u8.into());
    assert_eq!(sim.get(out1), 1u8.into());

    sim.modify(|io| {
        io.set(i1, 0u8);
        io.set(sel0, 1u8);
        io.set(sel1, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out0), 0u8.into());
    assert_eq!(sim.get(out1), 0u8.into());
}

#[test]
fn test_hierarchical_overlapping_partial_write_dynamic_index_runtime() {
    let code = r#"
        module ChildFb (
            a: input logic<3>,
            lo: output logic,
        ) {
            assign lo = a[2];
        }

        module ChildDyn (
            a: input logic<3>,
            idx: input logic,
            o: output logic,
        ) {
            assign o = a[idx];
        }

        module Top (
            i0: input logic,
            i1: input logic,
            i2: input logic,
            sel: input logic,
            out_fb: output logic,
            out_dyn: output logic,
            out_v0: output logic,
            out_v1: output logic,
        ) {
            var v: logic<3>;
            var lo: logic;
            var d: logic;

            inst fb: ChildFb (
                a: v,
                lo: lo,
            );

            inst dyn: ChildDyn (
                a: v,
                idx: sel,
                o: d,
            );

            // Non-overlapping source for feedback input.
            assign v[2] = i2;

            // Overlapping writes to the same bit: final v[1] must be lo (not i1).
            always_comb {
                v[1] = i1;
                v[1] = lo;
                v[0] = i0;
            }

            assign out_fb = lo;
            assign out_dyn = d;
            assign out_v0 = v[0];
            assign out_v1 = v[1];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let i0 = sim.signal("i0");
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel = sim.signal("sel");
    let out_fb = sim.signal("out_fb");
    let out_dyn = sim.signal("out_dyn");
    let out_v0 = sim.signal("out_v0");
    let out_v1 = sim.signal("out_v1");

    // lo = v[2] = i2 = 0. v[1] must be overridden to lo (0), v[0] = i0 (1).
    sim.modify(|io| {
        io.set(i0, 1u8);
        io.set(i1, 1u8);
        io.set(i2, 0u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_v0), 1u8.into());
    assert_eq!(sim.get(out_v1), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());

    // sel=0 reads untouched bit v[0] path.
    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // Change i2 only: should update lo/v[1], while v[0] keeps i0.
    sim.modify(|io| io.set(i2, 1u8)).unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_v0), 1u8.into());

    sim.modify(|io| io.set(sel, 1u8)).unwrap();
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // Change i1 only: v[1] must still follow lo (i2), not i1.
    sim.modify(|io| io.set(i1, 0u8)).unwrap();
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // Change i0 only: dynamic read on sel=0 should follow v[0] immediately.
    sim.modify(|io| {
        io.set(i0, 0u8);
        io.set(sel, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_v0), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());
}

#[test]
fn test_hierarchical_concat_then_overlap_dynamic_index_runtime() {
    let code = r#"
        module ChildFb (
            a: input logic<3>,
            lo: output logic,
        ) {
            assign lo = a[2];
        }

        module ChildDyn (
            a: input logic<3>,
            idx: input logic,
            o: output logic,
        ) {
            assign o = a[idx];
        }

        module Top (
            i0: input logic,
            i1: input logic,
            i2: input logic,
            sel: input logic,
            out_fb: output logic,
            out_dyn: output logic,
            out_v1: output logic,
        ) {
            var v: logic<3>;
            var lo: logic;
            var d: logic;

            inst fb: ChildFb (
                a: v,
                lo: lo,
            );

            inst dyn: ChildDyn (
                a: v,
                idx: sel,
                o: d,
            );

            // Full concat assignment then overlapping bit override.
            // Final v[1] should be lo (== v[2] == i2), not i1.
            always_comb {
                v = {i2, i1, i0};
                v[1] = lo;
            }

            assign out_fb = lo;
            assign out_dyn = d;
            assign out_v1 = v[1];
        }
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();

    let i0 = sim.signal("i0");
    let i1 = sim.signal("i1");
    let i2 = sim.signal("i2");
    let sel = sim.signal("sel");
    let out_fb = sim.signal("out_fb");
    let out_dyn = sim.signal("out_dyn");
    let out_v1 = sim.signal("out_v1");

    sim.modify(|io| {
        io.set(i0, 0u8);
        io.set(i1, 0u8);
        io.set(i2, 1u8);
        io.set(sel, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get(out_fb), 1u8.into());
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // i1 toggles but must not affect v[1] because v[1] is overridden by lo.
    sim.modify(|io| io.set(i1, 1u8)).unwrap();
    assert_eq!(sim.get(out_v1), 1u8.into());
    assert_eq!(sim.get(out_dyn), 1u8.into());

    // i2 toggles and must propagate to lo/v[1]/dynamic sel=1.
    sim.modify(|io| io.set(i2, 0u8)).unwrap();
    assert_eq!(sim.get(out_fb), 0u8.into());
    assert_eq!(sim.get(out_v1), 0u8.into());
    assert_eq!(sim.get(out_dyn), 0u8.into());
}

#[test]
fn test_child_signal_access() {
    let code = r#"
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
    "#;
    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let top_in = sim.signal("top_in");

    // Access child instance signal via child_signal()
    let child_i_data = sim.child_signal(&[("u_sub", 0)], "i_data");
    let child_o_data = sim.child_signal(&[("u_sub", 0)], "o_data");

    sim.modify(|io| io.set(top_in, 0x10u8)).unwrap();
    assert_eq!(sim.get(child_i_data), 0x10u8.into());
    assert_eq!(sim.get(child_o_data), 0x11u8.into());
}

#[test]
fn test_named_hierarchy_structure() {
    let code = r#"
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
    "#;
    let sim = Simulator::builder(code, "Top").build().unwrap();
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

#[test]
fn test_named_hierarchy_multiple_instances() {
    let code = r#"
        module Worker (
            clk: input clock,
            i_val: input  logic<8>,
            o_val: output logic<8>
        ) {
            var reg: logic<8>;
            always_ff { reg = i_val; }
            assign o_val = reg;
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
    "#;
    let sim = Simulator::builder(code, "Top").build().unwrap();
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

