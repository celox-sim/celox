use super::*;

sv_backends! {

    fn simulates_veryl_generated_countones_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Countones.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Countones.sv"))], "Top");

    let i_data = sim.signal("i_data");
    let o_ones = sim.signal("o_ones");

    for value in [0u64, 1, 0xffff, 0xdead_beef, u64::MAX] {
        sim.modify(|io| io.set(i_data, value)).unwrap();
        assert_eq!(sim.get(o_ones), value.count_ones().into());
    }
    }

    fn simulates_veryl_generated_onehot_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Onehot.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Onehot.sv"))], "Top");

    let i_data = sim.signal("i_data");
    let o_onehot = sim.signal("o_onehot");
    let o_zero = sim.signal("o_zero");

    for value in [0u64, 1, 2, 3, 1u64 << 63, (1u64 << 63) | 1] {
        sim.modify(|io| io.set(i_data, value)).unwrap();
        assert_eq!(sim.get(o_onehot), u8::from(value.count_ones() == 1).into());
        assert_eq!(sim.get(o_zero), u8::from(value == 0).into());
    }
    }

    fn simulates_veryl_generated_edge_detector_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/EdgeDetector.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("EdgeDetector.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_data = sim.signal("i_data");
    let o_edge = sim.signal("o_edge");
    let o_posedge = sim.signal("o_posedge");
    let o_negedge = sim.signal("o_negedge");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_data, 0u32);
    }).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();

    sim.modify(|io| io.set(i_data, 1u32)).unwrap();
    assert_eq!(sim.get(o_edge), 1u8.into());
    assert_eq!(sim.get(o_posedge), 1u8.into());
    assert_eq!(sim.get(o_negedge), 0u8.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_edge), 0u8.into());

    sim.modify(|io| io.set(i_data, 0u32)).unwrap();
    assert_eq!(sim.get(o_edge), 1u8.into());
    assert_eq!(sim.get(o_posedge), 0u8.into());
    assert_eq!(sim.get(o_negedge), 1u8.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_edge), 0u8.into());
    }

    fn simulates_veryl_generated_std_counter_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/StdCounter.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("StdCounter.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 0u32.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 1u32.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 2u32.into());
    }

    fn simulates_veryl_generated_gray_counter_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/GrayCounter.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("GrayCounter.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_up = sim.signal("i_up");
    let o_count = sim.signal("o_count");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_up, 0u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 0u32.into());

    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(i_up, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 1u32.into());

    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_count), 3u32.into());
    }

    fn simulates_veryl_generated_lfsr_sv(sim) {
        @setup {
    let sv = include_str!("../../../testdata/verilator/Lfsr.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("Lfsr.sv"))], "Top");

    let clk = sim.event("clk");
    let i_en = sim.signal("i_en");
    let i_set = sim.signal("i_set");
    let i_setval = sim.signal("i_setval");
    let o_val = sim.signal("o_val");

    sim.modify(|io| {
        io.set(i_en, 1u8);
        io.set(i_set, 1u8);
        io.set(i_setval, 1u32);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_val), 1u32.into());

    sim.modify(|io| io.set(i_set, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(o_val), 0x8000_0057u32.into());
    }

}

#[test]
fn simulates_veryl_generated_gray_codec_sv_smoke() {
    let sv = include_str!("../../../testdata/verilator/GrayCodec.sv");
    let mut sim = Simulator::from_sv_sources(vec![(sv, Path::new("GrayCodec.sv"))], "Top")
        .build_native()
        .unwrap();
    let i_bin = sim.signal("i_bin");
    let o_gray = sim.signal("o_gray");
    let o_bin = sim.signal("o_bin");

    sim.modify(|io| io.set(i_bin, 0xdead_beefu32)).unwrap();

    assert_eq!(sim.get(o_gray), 0xb1fb_6198u32.into());
    assert_eq!(sim.get(o_bin), 0xdead_beefu32.into());
}

#[test]
fn builds_veryl_generated_verilator_sv_smoke() {
    for (name, sv) in [
        (
            "Countones.sv",
            include_str!("../../../testdata/verilator/Countones.sv"),
        ),
        (
            "Fifo.sv",
            include_str!("../../../testdata/verilator/Fifo.sv"),
        ),
        (
            "EdgeDetector.sv",
            include_str!("../../../testdata/verilator/EdgeDetector.sv"),
        ),
        (
            "GrayCodec.sv",
            include_str!("../../../testdata/verilator/GrayCodec.sv"),
        ),
        (
            "GrayCounter.sv",
            include_str!("../../../testdata/verilator/GrayCounter.sv"),
        ),
        (
            "Lfsr.sv",
            include_str!("../../../testdata/verilator/Lfsr.sv"),
        ),
        (
            "Onehot.sv",
            include_str!("../../../testdata/verilator/Onehot.sv"),
        ),
        (
            "StdCounter.sv",
            include_str!("../../../testdata/verilator/StdCounter.sv"),
        ),
    ] {
        Simulator::from_sv_sources(vec![(sv, Path::new(name))], "Top")
            .build_native()
            .unwrap_or_else(|err| panic!("failed to build {name}: {err:?}"));
    }
}

fn assert_combinational_loop(source: &str) {
    match Simulator::from_sv_sources(vec![(source, Path::new("loop.sv"))], "Top").build_cranelift()
    {
        Ok(_) => panic!("a genuine combinational loop must be rejected:\n{source}"),
        Err(error) => assert!(
            matches!(
                error.kind(),
                celox::SimulatorErrorKind::SIRParser(
                    celox::ParserError::Scheduler(celox::SchedulerError::CombinationalLoop { .. })
                        | celox::ParserError::SchedulerWithLocation {
                            error: celox::SchedulerError::CombinationalLoop { .. },
                            ..
                        }
                )
            ),
            "expected a combinational loop, got: {error:?}"
        ),
    }
}

// Bit-precise scheduling must still reject feedback through the same bit.
#[test]
fn rejects_feedback_through_the_same_bit_of_one_variable() {
    assert_combinational_loop(
        r#"
        module Top(input logic inp, output logic out);
            logic [1:0] v;
            logic lo;
            assign lo = v[0];
            assign v = {inp, lo};
            assign out = v[0];
        endmodule
    "#,
    );
}

#[test]
fn rejects_feedback_through_the_same_bit_of_child_input() {
    assert_combinational_loop(
        r#"
        module Child(input logic [1:0] a, output logic lo);
            assign lo = a[0];
        endmodule
        module Top(input logic inp, output logic out);
            logic [1:0] v;
            logic lo;
            Child c(.a(v), .lo(lo));
            assign v = {inp, lo};
            assign out = v[1];
        endmodule
    "#,
    );
}

sv_backends! {
    fn literal_parameter_api_preserves_width_sign_and_unknowns(sim) {
        @setup {
            let source = r#"
                module Top #(parameter TEXT="default", parameter NEG=0,
                    parameter logic [57:0] FILL=0, parameter logic [3:0] UNKNOWN=0,
                    parameter logic [64:0] WIDE=0, parameter FINAL=0,
                    parameter logic [57:0] X=0, parameter logic [57:0] Z=0,
                    parameter logic [57:0] COPY=FILL)(output logic matched);
                    assign matched=(TEXT=="XILINX") && ($bits(TEXT)==48)
                        && (NEG==-8) && (NEG<0) && ($bits(NEG)==32)
                        && (FILL==58'h3ffffffffffffff) && (UNKNOWN===4'b10xz)
                        && (WIDE==65'h10000000000000001) && (FINAL==7)
                        && (COPY==58'h3ffffffffffffff)
                        && (X===58'bx) && (Z===58'bz);
                endmodule
            "#;
        }
        @build Simulator::from_sv_sources(vec![(source, Path::new("parameter_api.sv"))], "Top")
            .param_literal("TEXT", "\"XILINX\"")
            .param("NEG", 3).param_literal("NEG", "-8")
            .param_literal("FILL", "'1").param_literal("UNKNOWN", "4'b10xz")
            .param_literal("WIDE", "65'h10000000000000001")
            .param_literal("FINAL", "-1").param("FINAL", 7)
            .param_literal("X", "'x").param_literal("Z", "'z")
            .four_state(true).opt_level(celox::OptLevel::O2);
        assert_eq!(sim.get(sim.signal("matched")), BigUint::from(1u8));
    }
}

#[test]
fn rejects_incompatible_named_array_parameter_overrides() {
    for declaration in [
        "logic [7:0] TABLE[3] = '{1, 2, 3}",
        "logic [3:0] TABLE[2] = '{1, 2}",
        "logic signed [7:0] TABLE[2] = '{1, 2}",
        "bit [7:0] TABLE[2] = '{1, 2}",
        "logic [7:0] TABLE[1][2] = '{'{1, 2}}",
    ] {
        let source = format!(
            r#"
            module Child #(parameter logic [7:0] Q[2] = '{{0, 0}})(output logic [7:0] y);
                assign y = Q[0];
            endmodule
            module Top(output logic [7:0] y);
                localparam {declaration};
                Child #(.Q(TABLE)) child(y);
            endmodule
        "#
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("array_override.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("incompatible array parameter must be rejected"),
        };
        assert!(
            error.to_string().contains("is not assignment compatible"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn rejects_incompatible_port_pattern_subarray_items() {
    for declaration in [
        "logic [7:0] row",
        "logic [7:0] row[3]",
        "logic [3:0] row[2]",
        "logic signed [7:0] row[2]",
        "bit [7:0] row[2]",
        "logic [7:0] row[1][2]",
    ] {
        let source = format!(
            r#"
            module Child(input logic [7:0] cfg[2][2], output wire [7:0] y);
                assign y=cfg[0][0];
            endmodule
            module Top(output wire [7:0] y);
                {declaration};
                Child c(.cfg('{{row,row}}), .y(y));
            endmodule
            "#
        );
        let error =
            match Simulator::from_sv_sources(vec![(&source, Path::new("subarray_item.sv"))], "Top")
                .build_cranelift()
            {
                Err(error) => error,
                Ok(_) => panic!("incompatible subarray item must be rejected: {declaration}"),
            };
        let error = error.to_string();
        assert!(
            error.contains("is not assignment compatible")
                || (error.contains("scalar")
                    && error.contains("assignment pattern item for unpacked array")),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn rejects_array_values_in_scalar_port_pattern_elements() {
    for pattern in ["'{row,row}", "'{default:row}"] {
        let source = format!(
            r#"
            module Child(input logic [7:0] cfg[2], output wire [7:0] y);
                assign y=cfg[0];
            endmodule
            module Top(output wire [7:0] y);
                logic [7:0] row[2];
                Child c(.cfg({pattern}), .y(y));
            endmodule
            "#
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("array_scalar_item.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("array value must not be truncated to a scalar: {pattern}"),
        };
        let error = error.to_string();
        assert!(
            error.contains("assignment pattern item for scalar"),
            "{pattern}: {error}"
        );
    }
}

/// A default of a port pattern fills each packed union element as a whole
/// (IEEE 1800-2023 10.9.2, 7.3.1), never by its overlapping fields one by
/// one, for module and interface ports and parameters.
#[test]
fn port_pattern_defaults_fill_packed_unions_whole() {
    for (connection, value) in [
        ("Child c(.cfg('{default:'x}), .o(o));", None),
        (
            "Child c(.cfg(P::items_t'{default:'1}), .o(o));",
            Some(0b11u8),
        ),
        ("Cfg c(.cfg('{default:'x})); assign o = '0;", Some(0)),
        ("WithParam #(.CFG('{default:'1})) c(.o(o));", Some(0b11)),
    ] {
        let source = format!(
            r#"
            package P;
                typedef union packed {{logic a; bit b;}} item_t;
                typedef item_t items_t[2];
            endpackage
            module Child(input P::item_t cfg[2], output logic [1:0] o);
                assign o = {{cfg[1], cfg[0]}};
            endmodule
            interface Cfg(input P::item_t cfg[2]); endinterface
            module WithParam #(parameter P::item_t CFG[2]='{{'0,'0}})(output logic [1:0] o);
                assign o = {{CFG[1], CFG[0]}};
            endmodule
            module Top(output logic [1:0] o);
                {connection}
            endmodule
            "#
        );
        let mut sim = Simulator::from_sv_sources(
            vec![(&source, Path::new("union_pattern_default.sv"))],
            "Top",
        )
        .four_state(true)
        .build_cranelift()
        .unwrap_or_else(|error| panic!("{connection}: {error}"));
        let (bits, mask) = sim.get_four_state(sim.signal("o"));
        match value {
            // Each union element is X as a whole.
            None => assert_eq!(mask, 0b11u8.into(), "{connection}"),
            Some(value) => {
                assert_eq!((bits, mask), (value.into(), 0u8.into()), "{connection}")
            }
        }
    }
}

#[test]
fn array_parameter_elements_reject_runtime_values() {
    for (declaration, expression) in [
        ("logic [4:0] A[2]='{5'd3,5'd4};", "A[0]"),
        ("localparam logic [4:0] A[2]='{5'd3,5'd4};", "A[index]"),
    ] {
        let source = format!(
            "module Child #(parameter P=0)(output wire[4:0] y);assign y=P;endmodule\n\
             module Top(input int index,output wire[4:0] y);{declaration}\n\
             Child #(.P({expression})) child(.y(y));endmodule"
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("runtime_array_parameter.sv"))],
            "Top",
        )
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("runtime parameter argument {expression} must be rejected"),
        };
        assert!(
            error.to_string().contains("parameter"),
            "{expression}: {error}"
        );
    }
}

#[test]
fn array_parameter_elements_check_unselected_conditional_branch_types() {
    for declaration in [
        "logic[4:0] B[2]='{1,2};",
        "logic[3:0] B[3]='{1,2,3};",
        "logic signed[3:0] B[2]='{1,2};",
        "bit[3:0] B[2]='{1,2};",
    ] {
        let source = format!(
            "module Child #(parameter logic[3:0] Q[2]='{{0,0}})(output wire[3:0] y);\n\
             assign y=Q[0];endmodule\n\
             module Top(output wire[3:0] y);localparam logic[3:0] A[2]='{{3,4}};\n\
             localparam {declaration}\n\
             Child #(.Q(1 ? A : B)) child(.y(y));endmodule"
        );
        assert!(
            Simulator::from_sv_sources(
                vec![(&source, Path::new("conditional_array_type.sv"))],
                "Top"
            )
            .build_cranelift()
            .is_err(),
            "unselected conditional branch with {declaration} must be rejected"
        );
    }
}

#[test]
fn rejects_malformed_non_ansi_port_declarations() {
    for (source, message) in [
        (
            "module Top(a, y); input a; endmodule",
            "port `y` has no port declaration",
        ),
        (
            "module Top(a); input a; input a; endmodule",
            "port `a` is declared more than once",
        ),
        (
            "module Top(a); input a; output b; endmodule",
            "port declaration `b` names no port",
        ),
        (
            "module Top(a); input [3:0] a; wire [7:0] a; endmodule",
            "its net or variable declaration of `a` differ",
        ),
        (
            "module Top(a); parameter N = 8; input [3:0] a; wire [N-1:0] a; endmodule",
            "its net or variable declaration of `a` differ",
        ),
        (
            "module Top(a); input a; wire [3:0] a; endmodule",
            "its net or variable declaration differ",
        ),
        (
            "module Top(q); output reg q; reg q; endmodule",
            "port `q` with a net or variable type is declared again",
        ),
        (
            "module Top(q); output q; reg q = 1'b1; endmodule",
            "variable declaration of non-ANSI port `q` with an initializer",
        ),
        (
            "module Top({y, z}); output y, z; assign y = 1'b1; assign z = 1'b0; endmodule",
            "non-ANSI port expressions",
        ),
    ] {
        let error = Simulator::from_sv_sources(vec![(source, Path::new("ports.sv"))], "Top")
            .build_cranelift()
            .expect_err(source)
            .to_string()
            .replace('│', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(error.contains(message), "{source}: {error}");
    }
}

/// A hierarchical name selects a member of a packed structure of another
/// instance, of a package type or of a type the instance's module declares,
/// to read it and to write it (IEEE 1800-2023 23.6, 7.2). Icarus Verilog
/// 13.0 and Verilator 5.054 print the same lines.
#[test]
fn hierarchical_names_select_members_of_structures() {
    let source = r#"`timescale 1ns/1ns
package p;
  typedef struct packed { logic [1:0] hi; logic [1:0] lo; } s_t;
endpackage
module child(input logic [3:0] a);
  typedef struct packed { logic en; logic [2:0] v; } l_t;
  p::s_t s;
  l_t l;
  always_comb s = a;
  initial begin
  end
endmodule
module top;
  logic [3:0] a = 4'd6;
  child c(.a(a));
  initial begin
    #1 $display("A %b %b", c.s.lo, c.s.hi);
    c.l.v = 3'd5; c.l.en = 1'b1;
    #1 $display("B %b %b %b", c.l, c.l.v, c.l.en);
    $finish;
  end
endmodule"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("members.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["A 10 01", "B 1101 101 1"],
            "four_state={four_state}"
        );
    }
}

/// A hierarchical name of another instance's structure variable selects
/// members of the elements of its packed array member by run-time indices,
/// as Caliptra's testbench reads
/// `key_vault1.kv_reg_hwif_out.KEY_ENTRY[kv_entry_id]` (IEEE 1800-2023
/// 23.6, 7.4.5); the name is resolved although its scan ends at the index.
/// Verilator 5.054 prints the same lines.
#[test]
fn hierarchical_names_select_member_elements_by_run_time_indices() {
    let source = r#"`timescale 1ns/1ns
package p;
  typedef struct packed { logic [3:0] data; logic v; } e_t;
  typedef struct packed { logic [7:0] pre; e_t [3:0] KEY_ENTRY; } o_t;
endpackage
module kv;
  p::o_t hwif_out;
  initial begin
    hwif_out = '0;
    for (int k = 0; k < 4; k++) hwif_out.KEY_ENTRY[k].data = 4'(k + 9);
  end
endmodule
module top;
  kv key_vault1();
  int id;
  initial begin
    #1;
    for (id = 0; id < 4; id++) $display("R %0d %h", id, key_vault1.hwif_out.KEY_ENTRY[id].data);
    id = 2;
    key_vault1.hwif_out.KEY_ENTRY[id].v = 1'b1;
    #1 $display("W %h", key_vault1.hwif_out);
  end
endmodule"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("indices.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["R 0 9", "R 1 a", "R 2 b", "R 3 c", "W 00c5e92"],
            "four_state={four_state}"
        );
    }
}

/// The hierarchical name may select an element of an instance array by a
/// constant index before the variable, as
/// `key_vault[1].hwif_out.KEY_ENTRY[id]` (IEEE 1800-2023 23.6, 7.4.5).
/// Verilator 5.054 prints the same lines.
#[test]
fn hierarchical_names_of_instance_array_elements_select_member_elements_by_run_time_indices() {
    let source = r#"`timescale 1ns/1ns
package p;
  typedef struct packed { logic [3:0] data; logic v; } e_t;
  typedef struct packed { logic [7:0] pre; e_t [3:0] KEY_ENTRY; } o_t;
endpackage
module kv #(parameter int B = 0);
  p::o_t hwif_out;
  initial begin
    hwif_out = '0;
    for (int k = 0; k < 4; k++) hwif_out.KEY_ENTRY[k].data = 4'(k + B);
  end
endmodule
module top;
  kv #(.B(3)) key_vault[2] ();
  int id;
  initial begin
    #1;
    for (id = 0; id < 4; id++) $display("R %0d %h", id, key_vault[1].hwif_out.KEY_ENTRY[id].data);
    id = 1;
    key_vault[1].hwif_out.KEY_ENTRY[id].v = 1'b1;
    #1 $display("W %h %h", key_vault[0].hwif_out, key_vault[1].hwif_out);
  end
endmodule"#;
    for four_state in [false, true] {
        let mut sim =
            celox::Simulation::from_sv_sources(vec![(source, Path::new("indices.sv"))], "top")
                .four_state(four_state)
                .build()
                .unwrap();
        sim.run_until(10).unwrap();
        let lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lines,
            ["R 0 3", "R 1 4", "R 2 5", "R 3 6", "W 0062906 0062926"],
            "four_state={four_state}"
        );
    }
}
