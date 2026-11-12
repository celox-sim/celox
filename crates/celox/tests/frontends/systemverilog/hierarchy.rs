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

#[test]
fn rejects_recursive_port_pattern_defaults_for_packed_unions() {
    for connection in [
        "Child c(.cfg('{default:'x}));",
        "Child c(.cfg(P::items_t'{default:'x}));",
        "Cfg c(.cfg('{default:'x}));",
        "WithParam #(.CFG('{default:'x})) c();",
    ] {
        let source = format!(
            r#"
            package P;
                typedef union packed {{logic a; bit b;}} item_t;
                typedef item_t items_t[2];
            endpackage
            module Child(input P::item_t cfg[2]); endmodule
            interface Cfg(input P::item_t cfg[2]); endinterface
            module WithParam #(parameter P::item_t CFG[2]='{{'0,'0}})(); endmodule
            module Top;
                {connection}
            endmodule
            "#
        );
        let error = match Simulator::from_sv_sources(
            vec![(&source, Path::new("union_pattern_default.sv"))],
            "Top",
        )
        .four_state(true)
        .build_cranelift()
        {
            Err(error) => error,
            Ok(_) => panic!("unsupported union default must be rejected: {connection}"),
        };
        assert!(
            error.to_string().contains("packed union"),
            "{connection}: {error}"
        );
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

/// A port range may name a member of an enumeration of a package that the
/// module header imports (IEEE 1800-2023 26.4), here in modules of two
/// specializations. Verilator 5.054 prints the same lines.
#[test]
fn port_ranges_name_enumeration_members_of_header_imports() {
    let source = r#"`timescale 1ns/1ns
package noc_pkg;
  typedef enum logic [2:0] {P = 3'd0, W, E, N, S} Dirs;
endpackage
module child
  import noc_pkg::*;
  #(parameter width_p = 4)
  (input [S:W][width_p-1:0] outs_i,
   input logic [E:W][width_p-1:0] hor_i,
   output [E:W][width_p-1:0] hor_o);
  assign hor_o = {hor_i[W], hor_i[E]};
  initial #1 $display("%m %0d %0d %h %h %h", $bits(outs_i), $bits(hor_o), outs_i[N], hor_i[E], hor_o[E]);
endmodule
module top;
  logic [3:0][3:0] outs;
  logic [1:0][3:0] hi, ho;
  logic [3:0][7:0] wouts;
  logic [1:0][7:0] whi, who;
  child c(.outs_i(outs), .hor_i(hi), .hor_o(ho));
  child #(.width_p(8)) w(.outs_i(wouts), .hor_i(whi), .hor_o(who));
  initial begin
    outs = 16'h4321; hi = 8'hA5;
    wouts = 32'h44332211; whi = 16'hBBAA;
    #2 $display("%h %h", ho, who);
  end
endmodule"#;
    for four_state in [false, true] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("port_enum_ranges.sv"))],
            "top",
        )
        .four_state(four_state)
        .build()
        .unwrap();
        sim.run_until(10).unwrap();
        let mut lines: Vec<_> = sim
            .drain_runtime_events()
            .into_iter()
            .filter_map(|event| match event {
                celox::RuntimeEvent::Display { message } => {
                    Some(String::from_utf8_lossy(&message).into_owned())
                }
                _ => None,
            })
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            ["5a aabb", "top.c 16 8 3 a 5", "top.w 32 16 33 bb aa"],
            "four_state={four_state}"
        );
    }
}
