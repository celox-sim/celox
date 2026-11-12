//! A combinational net the host does not observe only carries a value from
//! its driver to its readers. With a dead-store policy that lets signals go
//! the frontend substitutes such a net into the paths that read it, or
//! drops it when nothing does (`SirPass::NetInlining`); the signals that
//! stay observable keep the values they had.
use celox::{DeadStorePolicy, OptLevel, SettlePartition, Simulator, SirPass};
use std::path::Path;

/// Two instances chained through wires, a register that feeds the first
/// one back, a wire nothing reads, and wires with one and with two readers.
const DESIGN: &str = r#"
module Stage(
    input  logic [7:0] a,
    input  logic [7:0] b,
    input  logic       sel,
    output logic [7:0] y,
    output logic [7:0] aux
);
    logic [7:0] sum, mixed, picked;
    assign sum = a + b;
    assign mixed = sum ^ {b[3:0], a[7:4]};
    always_comb begin
        if (sel) picked = mixed;
        else picked = sum & 8'h3c;
    end
    assign y = picked;
    assign aux = sum;
endmodule

module Top(
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    input  logic       sel,
    output logic [7:0] out,
    output logic [7:0] acc_out
);
    logic [7:0] acc, first, second, aux1, aux2, unread;
    wire [7:0] next = second + 8'd1;
    Stage u1(.a(din), .b(acc), .sel(sel), .y(first), .aux(aux1));
    Stage u2(.a(first), .b(din), .sel(~sel), .y(second), .aux(aux2));
    assign unread = aux1 & aux2;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) acc <= 8'd0;
        else acc <= next;
    end
    assign out = second ^ acc;
    assign acc_out = acc;
endmodule
"#;

fn design() -> celox::SimulatorBuilder<'static, Simulator> {
    Simulator::from_sv_sources(vec![(DESIGN, Path::new("net-inlining.sv"))], "Top")
}

/// Reset, then drive a fixed pseudo-random sequence and read both outputs
/// after every host write and every clock edge.
macro_rules! trace {
    ($sim:expr) => {{
        let sim = &mut $sim;
        let clk = sim.event("clk");
        let rst_n = sim.signal("rst_n");
        let din = sim.signal("din");
        let sel = sim.signal("sel");
        let out = sim.signal("out");
        let acc_out = sim.signal("acc_out");
        let mut trace = Vec::new();
        sim.modify(|io| {
            io.set(rst_n, 0u8);
            io.set(din, 0u8);
            io.set(sel, 0u8);
        })
        .unwrap();
        sim.modify(|io| io.set(rst_n, 1u8)).unwrap();
        let mut state = 0x2545_f491u32;
        for _ in 0..200 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let (value, select) = ((state >> 8) as u8, (state >> 3) as u8 & 1);
            sim.modify(|io| {
                io.set(din, value);
                io.set(sel, select);
            })
            .unwrap();
            trace.push((sim.get(out), sim.get(acc_out)));
            sim.tick(clk).unwrap();
            trace.push((sim.get(out), sim.get(acc_out)));
        }
        trace
    }};
}

/// The settle plan of `builder`, cut as finely as it can be so that it
/// names every net the settle still stores.
fn settle_plan(builder: celox::SimulatorBuilder<'static, Simulator>) -> String {
    builder
        .settle_partition(SettlePartition::Always)
        .trace_settle_plan()
        .build_with_trace()
        .trace
        .settle_plan
        .clone()
        .expect("a partitioned settle has a plan")
}

/// Whether the settle of `plan` stores the net `name`: `instance.net`, or
/// `.net` in the top module.
fn stores(plan: &str, name: &str) -> bool {
    plan.lines()
        .filter(|line| line.trim_start().starts_with("writes "))
        .any(|line| line.split_whitespace().any(|written| written == name))
}

/// The observed outputs are the same with the nets inlined, with them kept
/// by disabling the pass, and without any dead-store policy.
#[test]
fn outputs_are_unchanged_by_inlining() {
    let mut reference = design().build().unwrap();
    let mut kept = design()
        .opt_level(OptLevel::O2)
        .disable_pass(SirPass::NetInlining)
        .build()
        .unwrap();
    let mut inlined = design().opt_level(OptLevel::O2).build().unwrap();
    let mut partitioned = design()
        .opt_level(OptLevel::O2)
        .settle_partition(SettlePartition::Always)
        .settle_check(true)
        .build()
        .unwrap();
    let expected = trace!(reference);
    assert!(
        expected.iter().any(|(out, _)| *out != 0u8.into()),
        "the sequence must exercise the design"
    );
    assert_eq!(trace!(kept), expected);
    assert_eq!(trace!(inlined), expected);
    assert_eq!(trace!(partitioned), expected);
}

/// With the top ports alone observed, the nets inside the instances and the
/// connecting wires leave the settle; the top outputs stay. Disabling the
/// pass, or building without a dead-store policy, keeps them.
#[test]
fn unobserved_nets_leave_the_settle() {
    let inlined = settle_plan(design().opt_level(OptLevel::O2));
    for gone in ["u1.sum", "u1.mixed", "u1.y", ".first", ".unread", ".aux2"] {
        assert!(!stores(&inlined, gone), "`{gone}` stays in:\n{inlined}");
    }
    for stays in [".out", ".acc_out"] {
        assert!(stores(&inlined, stays), "`{stays}` left:\n{inlined}");
    }
    let disabled = settle_plan(
        design()
            .opt_level(OptLevel::O2)
            .disable_pass(SirPass::NetInlining),
    );
    let unoptimized = settle_plan(design());
    for plan in [&disabled, &unoptimized] {
        for kept in ["u1.sum", "u1.y", ".first", ".unread"] {
            assert!(stores(plan, kept), "`{kept}` left:\n{plan}");
        }
    }
}

/// A net that left the program is no state of its own: looking it up
/// reports the dead-store policy, as it does for an eliminated store, and
/// a listing of the instance leaves it out. The ports stay.
#[test]
fn inlined_nets_are_not_handed_out() {
    let sim = design().opt_level(OptLevel::O2).build().unwrap();
    for (instance, net) in [("u1", "sum"), ("u1", "mixed"), ("u2", "y")] {
        assert!(
            matches!(
                sim.try_child_signal(&[(instance, 0)], net),
                Err(celox::AddrLookupError::DeadStoreEliminated { .. })
            ),
            "{instance}.{net}"
        );
    }
    for net in ["first", "unread", "aux2"] {
        assert!(
            matches!(
                sim.try_signal(net),
                Err(celox::AddrLookupError::DeadStoreEliminated { .. })
            ),
            "{net}"
        );
    }
    assert!(
        sim.instance_signals(&[("u1", 0)])
            .iter()
            .all(|signal| signal.name != "sum" && signal.name != "mixed")
    );
    assert!(sim.try_signal("out").is_ok());
    assert!(sim.try_signal("acc_out").is_ok());
}

/// What the host can still name stays in state: every instance's ports
/// under `PreserveAllPorts`, and every net once a signal is declared live.
#[test]
fn observable_nets_stay() {
    let all_ports = settle_plan(
        design()
            .opt_level(OptLevel::O2)
            .dead_store_policy(DeadStorePolicy::PreserveAllPorts),
    );
    for kept in ["u1.aux", "u2.y"] {
        assert!(stores(&all_ports, kept), "`{kept}` left:\n{all_ports}");
    }
    for gone in ["u1.sum", "u1.mixed", ".unread"] {
        assert!(!stores(&all_ports, gone), "`{gone}` stays in:\n{all_ports}");
    }
    let live = settle_plan(
        design()
            .opt_level(OptLevel::O2)
            .live_signal(vec![("u1".into(), 0)], vec!["mixed".into()]),
    );
    for kept in ["u1.mixed", "u1.sum", ".first"] {
        assert!(stores(&live, kept), "`{kept}` left:\n{live}");
    }
}

/// A process of the design reads a net through the hierarchy: that net
/// stays, the net feeding it is substituted into it, and the process sees
/// the same values.
#[test]
fn a_net_a_process_reads_stays() {
    let source = r#"`timescale 1ns/1ns
module Dut(input logic clk, input logic [7:0] d, output logic [7:0] q);
    logic [7:0] bumped, masked;
    assign bumped = d + 8'd3;
    assign masked = bumped ^ 8'h55;
    always_ff @(posedge clk) q <= masked;
endmodule

module Top(output int mismatches, samples);
    bit clk;
    logic [7:0] d = 8'd0;
    logic [7:0] q;
    Dut dut(.clk(clk), .d(d), .q(q));
    always #1 clk = ~clk;
    always @(negedge clk) begin
        if (dut.masked !== ((d + 8'd3) ^ 8'h55)) mismatches++;
        samples++;
        d <= d * 8'd5 + 8'd7;
    end
endmodule"#;
    for level in [OptLevel::O1, OptLevel::O2] {
        let mut sim = celox::Simulation::from_sv_sources(
            vec![(source, Path::new("net-inlining-probe.sv"))],
            "Top",
        )
        .opt_level(level)
        .build()
        .unwrap();
        sim.run_until(40).unwrap();
        let mismatches = sim.signal("mismatches");
        let samples = sim.signal("samples");
        assert_eq!(sim.get(samples), 20u32.into(), "{level:?}");
        assert_eq!(sim.get(mismatches), 0u32.into(), "{level:?}");
    }
}

/// A net that takes part of a wider one, such as the element `req_t[0]` that
/// an instance port gives to a narrower net, is substituted into its readers
/// as a copy of the wider value into a narrower register, which keeps only
/// the low bits: the reduction `|conc_req` and the concatenation that reads
/// the narrow net see five bits, not the whole 25-bit vector. Found with the
/// RTLMeter design HammerBlade (bsg_mesh_router and bsg_arb_round_robin).
#[test]
fn a_substituted_part_of_a_wider_net_keeps_its_width() {
    const SOURCE: &str = r#"
module conc #(parameter bit [4:0] pattern = 5'b11111, parameter int w = 5) (input [4:0] i, output [w-1:0] o);
  if (pattern[0]) begin : first
    assign o[0] = i[0];
  end
  for (genvar j = 1; j < 5; j++) begin : rof
    if (pattern[j]) begin : on
      assign o[$countones(pattern[j-1:0])] = i[j];
    end
  end
endmodule
module arb #(parameter width_p = 2) (input clk_i, input [width_p-1:0] reqs_i);
  logic [width_p-2:0] thermocode_r, thermocode_n;
  always_ff @(posedge clk_i) thermocode_r <= thermocode_n;
  wire [width_p*2-1:0] scan_li = {1'b0, thermocode_r & reqs_i[width_p-2:0], reqs_i};
  wire [width_p*2-1:0] scan_lo;
  assign scan_lo = scan_li;
  assign thermocode_n = (|scan_li[width_p*2-1-:width_p]) ? scan_lo[width_p*2-1-:width_p-1]
                                                         : scan_lo[width_p-1:1];
endmodule
module r (input clk_i, input [4:0][4:0] req, output logic [1:0] v_o);
  localparam bit [1:0][4:0] pattern = {5'b00101, 5'b11111};
  logic [4:0][4:0] req_t;
  assign req_t = req;
  for (genvar i = 0; i < 2; i++) begin : xbar
    localparam int w = $countones(pattern[i]);
    logic [w-1:0] conc_req;
    conc #(.pattern(pattern[i]), .w(w)) c (.i(req_t[i]), .o(conc_req));
    if (i == 0) begin : vv
      assign v_o[0] = |conc_req;
    end
    arb #(.width_p(w)) rr (.clk_i(clk_i), .reqs_i(conc_req));
  end
endmodule
module Top (input clk_i, input [4:0][4:0] req, output logic v);
  logic [1:0] v_o;
  r dut (.clk_i(clk_i), .req(req), .v_o(v_o));
  assign v = v_o[0];
endmodule
"#;
    let builder = || {
        Simulator::from_sv_sources(vec![(SOURCE, Path::new("narrow-net.sv"))], "Top")
            .opt_level(OptLevel::O2)
    };
    macro_rules! check {
        ($sim:expr, $backend:literal) => {{
            let mut sim = $sim;
            let clk = sim.event("clk_i");
            let (req, v) = (sim.signal("req"), sim.signal("v"));
            sim.tick(clk).unwrap();
            // req[1] = 5'b11001: element 0, the one `v` reduces, stays zero.
            sim.modify(|io| io.set(req, 0x19u32 << 5)).unwrap();
            assert_eq!(sim.get(v), 0u8.into(), $backend);
            sim.modify(|io| io.set(req, 0x19u32 << 5 | 0x4)).unwrap();
            assert_eq!(sim.get(v), 1u8.into(), $backend);
        }};
    }
    check!(builder().build_interpreter().unwrap(), "interpreter");
    check!(builder().build().unwrap(), "default backend");
    check!(builder().build_cranelift().unwrap(), "cranelift");
    #[cfg(target_arch = "x86_64")]
    check!(builder().build_native().unwrap(), "native");
}
