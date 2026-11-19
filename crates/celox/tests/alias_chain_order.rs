//! A copy of a copy of a reset, through ports, shares its home with the
//! reset it copies; every access to it names that home, so that a settle
//! partition stores the reset before it loads the copy. RTLMeter VeeR-EL2
//! with store-load forwarding and block optimization off scheduled the
//! load of a DCCM flop's reset port (an alias of an alias of the core
//! reset) above the store of the core reset, and the flop stayed in reset.
//! The `pad` instances only make the settle large enough to be partitioned.
use celox::{OptLevel, RuntimeEvent, Simulation, SirPass};
use std::path::Path;

const HEAD: &str = r#"`timescale 1ns/1ns
module rvdff #( parameter WIDTH=1 )
   (input logic [WIDTH-1:0] din, input logic clk, input logic rst_l, output logic [WIDTH-1:0] dout);
   always_ff @(posedge clk or negedge rst_l) begin
      if (rst_l == 0) dout[WIDTH-1:0] <= 0;
      else dout[WIDTH-1:0] <= din[WIDTH-1:0];
   end
endmodule

module dccm_mem(input logic active_clk, input logic rst_l, input logic [15:0] dccm_rd_addr_lo, output logic [1:0] q);
   logic [3:2] dccm_rd_addr_lo_q, dccm_rd_addr_hi_q;
   rvdff #(2) rd_addr_lo_ff (.*, .din(dccm_rd_addr_lo[2+:2]), .dout(dccm_rd_addr_lo_q[2+:2]), .clk(active_clk));
   rvdff #(2) rd_addr_hi_ff (.*, .din(dccm_rd_addr_lo[4+:2]), .dout(dccm_rd_addr_hi_q[2+:2]), .clk(active_clk));
   assign q = dccm_rd_addr_lo_q ^ dccm_rd_addr_hi_q;
endmodule

module mem #(parameter EN = 1) (input logic clk, input logic rst_l, input logic [15:0] dccm_rd_addr_lo, output logic [1:0] q);
   logic active_clk;
   assign active_clk = clk;
   if (EN == 1) begin : Gen_dccm_enable
      dccm_mem dccm(.*);
   end else begin : Gen_dccm_disable
      assign q = '0;
   end
endmodule

module core(input logic clk, input logic rst_l, input logic dbg, output logic core_rst_l);
   logic dbg_core_rst_l;
   always_ff @(posedge clk or negedge rst_l) if (!rst_l) dbg_core_rst_l <= 1'b1; else dbg_core_rst_l <= ~dbg;
   assign core_rst_l = rst_l & (dbg_core_rst_l | 1'b0);
endmodule

module wrapper(input logic clk, input logic rst_l, input logic [15:0] addr, output logic [1:0] q);
   logic core_rst_l;
   core c(.*, .dbg(1'b0));
   mem m(.clk(clk), .rst_l(core_rst_l), .dccm_rd_addr_lo(addr), .q(q));
endmodule"#;

const TAIL: &str = r#"module tb;
  bit clk, c2, c3, c4, c5, c6;
  logic rst_l;
  logic [15:0] a;
  logic [1:0] q;
  logic [63:0] s1, s2, s3, s4, s5;
  wrapper w(.clk(clk), .rst_l(rst_l), .addr(a), .q(q));
  pad p1(.clk(c2), .rst_l(rst_l), .seed(64'h1), .sum(s1));
  pad p2(.clk(c3), .rst_l(rst_l), .seed(64'h2), .sum(s2));
  pad p3(.clk(c4), .rst_l(rst_l), .seed(64'h3), .sum(s3));
  pad p4(.clk(c5), .rst_l(rst_l), .seed(64'h4), .sum(s4));
  pad p5(.clk(c6), .rst_l(rst_l), .seed(64'h5), .sum(s5));
  always #5 clk = ~clk;
  always #6 c2 = ~c2;
  always #7 c3 = ~c3;
  always #8 c4 = ~c4;
  always #9 c5 = ~c5;
  always #11 c6 = ~c6;
  initial begin
    rst_l = 1'b1;
    rst_l = #5 1'b0;
    rst_l = #25 1'b1;
  end
  initial begin
    #31 $display("t=%0t leaf_rst_l=%b core_rst_l=%b", $time, w.m.Gen_dccm_enable.dccm.rd_addr_lo_ff.rst_l, w.c.core_rst_l);
    #2 $display("t=%0t leaf_rst_l=%b core_rst_l=%b", $time, w.m.Gen_dccm_enable.dccm.rd_addr_lo_ff.rst_l, w.c.core_rst_l);
    #3 $display("t=%0t leaf_rst_l=%b core_rst_l=%b", $time, w.m.Gen_dccm_enable.dccm.rd_addr_lo_ff.rst_l, w.c.core_rst_l);
  end
  initial begin
    a = 16'h05a4;
    for (int k = 0; k < 8; k++) begin
      @(negedge clk) $display("t=%0t rst_l=%b a=%h q=%0d", $time, rst_l, a, q);
      a = 16'(16'h05a4 + 4 * k);
    end
    $display("sums %h %h %h %h %h", s1, s2, s3, s4, s5);
    $finish;
  end
endmodule"#;

fn source() -> String {
    const ASSIGNS: u64 = 400;
    let mut pad = String::from(
        "module pad(input logic clk, input logic rst_l, input logic [63:0] seed, output logic [63:0] sum);\n",
    );
    pad += &format!("  logic [63:0] w [{ASSIGNS}];\n  logic [63:0] r;\n");
    pad += "  always_ff @(posedge clk or negedge rst_l) if (!rst_l) r <= '0; else r <= r ^ seed ^ {r[62:0], r[63]};\n";
    pad += "  assign w[0] = r;\n";
    for i in 1..ASSIGNS {
        let (a, b) = ((i * 7) % i, (i * 13) % i);
        let constant = i.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let rotate = i % 64;
        pad += &if rotate != 63 {
            format!(
                "  assign w[{i}] = (w[{}] ^ {{w[{a}][{rotate}:0], w[{a}][63:{}]}}) + (w[{b}] & 64'h{constant:016x});\n",
                i - 1,
                rotate + 1
            )
        } else {
            format!(
                "  assign w[{i}] = (w[{}] ^ w[{a}]) + (w[{b}] & 64'h{constant:016x});\n",
                i - 1
            )
        };
    }
    pad += &format!(
        "  always_ff @(posedge clk) sum <= w[{}];\nendmodule\n",
        ASSIGNS - 1
    );
    format!("{HEAD}\n{pad}{TAIL}\n")
}

#[test]
fn a_reset_copy_of_a_copy_is_loaded_after_its_store() {
    let source = source();
    for four_state in [false, true] {
        // O2's dead-store policy lets the copy nets become aliases; with
        // these two passes on, the chain is forwarded to a register before
        // the aliases form, which hides the order of its accesses.
        {
            let mut sim = Simulation::from_sv_sources(
                vec![(source.as_str(), Path::new("alias_chain.sv"))],
                "tb",
            )
            .four_state(four_state)
            .opt_level(OptLevel::O2)
            .disable_pass(SirPass::StoreLoadForwarding)
            .disable_pass(SirPass::OptimizeBlocks)
            .build_native()
            .unwrap();
            sim.run_until(200).unwrap();
            let leaves: Vec<String> = sim
                .drain_runtime_events()
                .into_iter()
                .filter_map(|event| match event {
                    RuntimeEvent::Display { message } => {
                        Some(String::from_utf8_lossy(&message).into_owned())
                    }
                    _ => None,
                })
                .filter(|line| line.contains("leaf_rst_l"))
                .collect();
            assert_eq!(
                leaves,
                [
                    "t=31 leaf_rst_l=1 core_rst_l=1",
                    "t=33 leaf_rst_l=1 core_rst_l=1",
                    "t=36 leaf_rst_l=1 core_rst_l=1"
                ],
                "four_state={four_state}"
            );
        }
    }
}
