use super::*;

sv_backends! {
    fn simulates_systemverilog_named_port_hierarchy(sim) {
        @setup {
    let sv = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            assign y = a ^ b;
        endmodule

        module Top(input logic [7:0] lhs, input logic [7:0] rhs, output logic [7:0] out);
            Xor8 u_xor(
                .a(lhs),
                .b(rhs),
                .y(out)
            );
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("hierarchy.sv"))], "Top");

    let lhs = sim.signal("lhs");
    let rhs = sim.signal("rhs");
    let out = sim.signal("out");

    sim.modify(|io| {
        io.set(lhs, 0xa5u8);
        io.set(rhs, 0x3cu8);
    })
    .unwrap();

    assert_eq!(sim.get(out), 0x99u8.into());
    }

    fn simulates_systemverilog_hierarchy_through_internal_signal(sim) {
        @setup {
    let sv = r#"
        module Xor8(input logic [7:0] a, input logic [7:0] b, output logic [7:0] y);
            assign y = a ^ b;
        endmodule

        module Top(input logic [7:0] lhs, input logic [7:0] rhs, output logic [7:0] out);
            logic [7:0] rhs_tmp;
            assign rhs_tmp = rhs;
            Xor8 u_xor(
                .a(lhs),
                .b(rhs_tmp),
                .y(out)
            );
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("internal_hierarchy.sv"))], "Top");

    let lhs = sim.signal("lhs");
    let rhs = sim.signal("rhs");
    let out = sim.signal("out");

    sim.modify(|io| {
        io.set(lhs, 0xf0u8);
        io.set(rhs, 0x0fu8);
    })
    .unwrap();

    assert_eq!(sim.get(out), 0xffu8.into());
    }

    fn simulates_veryl_generated_style_gray_encoder_hierarchy(sim) {
        @setup {
    let sv = r#"
        module gray_encoder #(
            parameter int unsigned WIDTH = 32
        ) (
            input var logic [WIDTH-1:0] i_bin,
            output var logic [WIDTH-1:0] o_gray
        );
            always_comb o_gray = i_bin ^ (i_bin >> 1);
        endmodule

        module Top (
            input  var logic [32-1:0] i_bin,
            output var logic [32-1:0] o_gray
        );
            gray_encoder #(
                .WIDTH (32)
            ) u_enc (
                .i_bin  (i_bin),
                .o_gray (o_gray)
            );
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("generated_gray_encoder.sv"))], "Top");

    let i_bin = sim.signal("i_bin");
    let o_gray = sim.signal("o_gray");

    sim.modify(|io| io.set(i_bin, 0b1011_0000u32)).unwrap();
    assert_eq!(sim.get(o_gray), 0b1110_1000u32.into());
    }

    fn simulates_parameter_specialized_systemverilog_hierarchy(sim) {
        @setup {
    let sv = r#"
        module Pass #(
            parameter int unsigned WIDTH = 1
        ) (
            input  logic [WIDTH-1:0] i,
            output logic [WIDTH-1:0] o
        );
            assign o = i;
        endmodule

        module Top(
            input  logic [7:0] i8,
            input  logic [15:0] i16,
            output logic [7:0] o8,
            output logic [15:0] o16
        );
            Pass #(.WIDTH(8)) u8 (
                .i(i8),
                .o(o8)
            );
            Pass #(.WIDTH(16)) u16 (
                .i(i16),
                .o(o16)
            );
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("parameterized_hierarchy.sv"))], "Top");

    let i8 = sim.signal("i8");
    let i16 = sim.signal("i16");
    let o8 = sim.signal("o8");
    let o16 = sim.signal("o16");

    sim.modify(|io| {
        io.set(i8, 0xa5u8);
        io.set(i16, 0x5aa5u16);
    })
    .unwrap();

    assert_eq!(sim.get(o8), 0xa5u8.into());
    assert_eq!(sim.get(o16), 0x5aa5u16.into());
    }

    fn simulates_systemverilog_hierarchical_always_ff(sim) {
        @setup {
    let sv = r#"
        module Child(input logic clk, input logic rst, input logic d, output logic q);
            always_ff @(posedge clk, negedge rst) begin
                if (!rst) begin
                    q <= 1'b0;
                end else begin
                    q <= d;
                end
            end
        endmodule

        module Top(input logic clk, input logic rst, input logic d, output logic q);
            Child u(.clk(clk), .rst(rst), .d(d), .q(q));
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("hierarchical_ff.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    }

    fn simulates_systemverilog_hierarchical_always_ff_with_constant_clear(sim) {
        @setup {
    let sv = r#"
        module Child(
            input logic clk,
            input logic rst,
            input logic clear,
            input logic d,
            output logic q
        );
            always_ff @(posedge clk, negedge rst) begin
                if (!rst) begin
                    q <= 1'b0;
                end else if (clear) begin
                    q <= 1'b0;
                end else begin
                    q <= d;
                end
            end
        endmodule

        module Top(input logic clk, input logic rst, input logic d, output logic q);
            Child u(.clk(clk), .rst(rst), .clear(1'b0), .d(d), .q(q));
        endmodule
    "#;
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("hierarchical_ff_constant_clear.sv"))], "Top");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d = sim.signal("d");
    let q = sim.signal("q");

    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d, 1u8);
    }).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());

    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 1u8.into());
    }

    fn simulates_veryl_generated_countones_sv(sim) {
        @setup {
    let sv = include_str!("../../../../../benches/verilator/Countones.sv");
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
    let sv = include_str!("../../../../../benches/verilator/Onehot.sv");
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

    fn simulates_veryl_generated_linear_sec_sv(sim) {
        @setup {
    let sv = include_str!("../../../../../benches/verilator/LinearSec.sv");
        }
        @build Simulator::from_sv_sources(vec![(sv, Path::new("LinearSec.sv"))], "Top");

    let i_word = sim.signal("i_word");
    let o_word = sim.signal("o_word");
    let o_corrected = sim.signal("o_corrected");

    for value in [0u64, 1, 0x678, 0x1234_5678, (1u64 << 57) - 1] {
        sim.modify(|io| io.set(i_word, value)).unwrap();
        assert_eq!(sim.get(o_word), value.into());
        assert_eq!(sim.get(o_corrected), 0u8.into());
    }
    }

    fn simulates_veryl_generated_edge_detector_sv(sim) {
        @setup {
    let sv = include_str!("../../../../../benches/verilator/EdgeDetector.sv");
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
    let sv = include_str!("../../../../../benches/verilator/StdCounter.sv");
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
    let sv = include_str!("../../../../../benches/verilator/GrayCounter.sv");
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
    let sv = include_str!("../../../../../benches/verilator/Lfsr.sv");
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
    let sv = include_str!("../../../../../benches/verilator/GrayCodec.sv");
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
            include_str!("../../../../../benches/verilator/Countones.sv"),
        ),
        (
            "EdgeDetector.sv",
            include_str!("../../../../../benches/verilator/EdgeDetector.sv"),
        ),
        (
            "Fifo.sv",
            include_str!("../../../../../benches/verilator/Fifo.sv"),
        ),
        (
            "GrayCodec.sv",
            include_str!("../../../../../benches/verilator/GrayCodec.sv"),
        ),
        (
            "GrayCounter.sv",
            include_str!("../../../../../benches/verilator/GrayCounter.sv"),
        ),
        (
            "Lfsr.sv",
            include_str!("../../../../../benches/verilator/Lfsr.sv"),
        ),
        (
            "LinearSec.sv",
            include_str!("../../../../../benches/verilator/LinearSec.sv"),
        ),
        (
            "Onehot.sv",
            include_str!("../../../../../benches/verilator/Onehot.sv"),
        ),
        (
            "StdCounter.sv",
            include_str!("../../../../../benches/verilator/StdCounter.sv"),
        ),
    ] {
        Simulator::from_sv_sources(vec![(sv, Path::new(name))], "Top")
            .build_native()
            .unwrap_or_else(|err| panic!("failed to build {name}: {err:?}"));
    }
}
