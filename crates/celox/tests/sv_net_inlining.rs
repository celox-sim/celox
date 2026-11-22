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

/// Exercise the shared next-state evaluator through the normal tick and
/// settle entry points, including a register whose D cone crosses instances.
#[test]
fn next_state_graph_preserves_feedback_across_edges() {
    let mut reference = design()
        .four_state(false)
        .opt_level(OptLevel::O2)
        .disable_pass(SirPass::NextStateDag)
        .build()
        .unwrap();
    let expected = trace!(reference);
    let graph = || {
        design()
            .four_state(false)
            .opt_level(OptLevel::O2)
            .enable_pass(SirPass::NextStateDag)
    };
    assert_eq!(trace!(graph().build().unwrap()), expected);
    assert_eq!(trace!(graph().build_interpreter().unwrap()), expected);
    assert_eq!(trace!(graph().build_cranelift().unwrap()), expected);
    #[cfg(feature = "wasm-runtime")]
    assert_eq!(trace!(graph().build_wasm().unwrap()), expected);
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

/// The read address, write address, enable and data all sample the old state.
/// The second clock forces split evaluation and publication when both fire.
#[test]
fn graph_memory_samples_before_publication() {
    let source = r#"
        module Top(input bit clk, other, en, input bit [3:0] addr,
                   input bit [7:0] data, output bit [7:0] bypass, old, other_q);
            bit [7:0] mem[0:8];
            bit [3:0] sampled_addr;
            always @(posedge clk) begin
                sampled_addr <= addr;
                if (en) mem[sampled_addr] <= data;
                bypass <= en ? data : mem[addr];
                old <= mem[addr];
            end
            always @(posedge other) other_q <= mem[addr];
        endmodule
    "#;
    let builder = || {
        Simulator::from_sv_sources(vec![(source, Path::new("ram.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
            .enable_pass(SirPass::NextStateDag)
    };
    fn run<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let clk = sim.event("clk");
        let other = sim.event("other");
        let addr = sim.signal("addr");
        let en = sim.signal("en");
        let data = sim.signal("data");
        let old = sim.signal("old");
        let bypass = sim.signal("bypass");
        let other_q = sim.signal("other_q");
        let mut mem = [0u8; 9];
        let mut sampled_addr = 0;
        for (i, (a, w, d)) in [
            (1, 1, 42),
            (8, 1, 19),
            (1, 0, 5),
            (1, 1, 77),
            (8, 0, 0),
            (9, 1, 12),
            (0, 1, 31),
            (1, 0, 0),
            (1, 1, 0),
        ]
        .into_iter()
        .enumerate()
        {
            sim.set(addr, a as u8);
            sim.set(en, w as u8);
            sim.set(data, d);
            let before = mem.get(a).copied().unwrap_or_default();
            if i % 2 == 0 {
                sim.tick_events(&[clk, other]).unwrap();
                assert_eq!(sim.get(other_q), before.into());
            } else {
                sim.tick(clk).unwrap();
            }
            assert_eq!(sim.get(old), before.into(), "address {a}");
            assert_eq!(sim.get(bypass), if w == 1 { d } else { before }.into());
            if w == 1
                && let Some(word) = mem.get_mut(sampled_addr)
            {
                *word = d;
            }
            sampled_addr = a;
        }
    }
    run(builder().build().unwrap());
    run(builder().build_interpreter().unwrap());
    #[cfg(feature = "cranelift")]
    run(builder().build_cranelift().unwrap());
    #[cfg(feature = "wasm-runtime")]
    run(builder().build_wasm().unwrap());
}

#[test]
fn mixed_clock_samples_graph_and_procedural_writes_before_publication() {
    let source = r#"
        module Stage(input wire clk, input wire [7:0] d, output bit [7:0] q);
            always @(posedge clk) q <= d;
        endmodule
        module Top(input wire clk, other, input wire [7:0] d,
                   output wire [7:0] first, last, output bit [7:0] tmp, middle, seen, old);
            bit [7:0] mem[0:2];
            always @(posedge clk) begin
                if (d[7]) mem[d[1:0]] <= middle;
                old <= mem[d[1:0]];
            end
            Stage a(clk, d, first);
            always @(posedge clk) begin
                tmp = first * 8'd3;
                middle <= tmp + 8'd1;
            end
            Stage b(clk, middle, last);
            always @(posedge other) seen <= last;
        endmodule
    "#;
    let builder = || {
        Simulator::from_sv_sources(vec![(source, Path::new("stages.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
    };
    fn run<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let clk = sim.event("clk");
        let other = sim.event("other");
        let d = sim.signal("d");
        let outputs = ["first", "tmp", "middle", "last", "seen"].map(|s| sim.signal(s));
        let mut expected = [0u8; 5];
        let old = sim.signal("old");
        let mut memory = [0u8; 3];
        for (index, value) in [13u8, 97, 0, 168, 255, 4, 33, 136, 8]
            .into_iter()
            .enumerate()
        {
            sim.set(d, value);
            let [first, _, middle, last, seen] = expected;
            let sampled = first.wrapping_mul(3);
            let together = index % 2 == 1;
            if together {
                sim.tick_events(&[other, clk]).unwrap();
            } else {
                sim.tick(clk).unwrap();
            }
            let addr = usize::from(value & 3);
            assert_eq!(
                sim.get(old),
                memory.get(addr).copied().unwrap_or(0).into(),
                "RAM on edge {index}"
            );
            if value & 128 != 0
                && let Some(word) = memory.get_mut(addr)
            {
                *word = middle;
            }
            expected = [
                value,
                sampled,
                sampled.wrapping_add(1),
                middle,
                if together { last } else { seen },
            ];
            for (signal, value) in outputs.iter().zip(expected) {
                assert_eq!(sim.get(*signal), value.into(), "edge {index}");
            }
        }
    }
    run(builder()
        .disable_pass(SirPass::NextStateDag)
        .build()
        .unwrap());
    let graph = || builder().enable_pass(SirPass::NextStateDag);
    run(graph().build().unwrap());
    run(graph().build_interpreter().unwrap());
    #[cfg(feature = "cranelift")]
    run(graph().build_cranelift().unwrap());
    #[cfg(feature = "wasm-runtime")]
    run(graph().build_wasm().unwrap());
}

#[test]
fn a_timed_wait_observes_mixed_clock_publication() {
    let source = r#"
        `timescale 1ns/1ns
        module Top(output bit [7:0] q, sampled, wakes);
            bit clk;
            bit [7:0] d, temp;
            wire [7:0] value = d + 8'd1;
            always @(posedge clk) q <= value;
            always @(posedge clk) begin
                temp = q * 8'd3;
                sampled <= temp;
            end
            initial begin
                #1; d = 2;
                #1; clk = 1;
                #1; clk = 0; d = 3;
                #1; clk = 1;
                #1; clk = 0; d = 4;
                #1; clk = 1;
            end
            initial forever begin
                @(posedge q[0]);
                wakes = wakes + 8'd1;
            end
        endmodule
    "#;
    let builder = || {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("wait.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
    };
    macro_rules! check {
        ($sim:expr) => {{
            let mut sim = $sim.unwrap();
            let signals = ["q", "sampled", "wakes"].map(|name| sim.signal(name));
            for (time, values) in [(3, [3u8, 0, 1]), (5, [4, 9, 1]), (7, [5, 12, 2])] {
                sim.run_until(time).unwrap();
                for (&signal, value) in signals.iter().zip(values) {
                    assert_eq!(sim.get(signal), value.into(), "time {time}");
                }
            }
        }};
    }
    check!(
        builder()
            .disable_pass(SirPass::NextStateDag)
            .build_interpreter()
    );
    let graph = || builder().enable_pass(SirPass::NextStateDag);
    check!(graph().build());
    check!(graph().build_interpreter());
    #[cfg(feature = "cranelift")]
    check!(graph().build_cranelift());
    #[cfg(feature = "wasm-runtime")]
    check!(graph().build_wasm());
}

#[test]
fn transposed_column_reductions_use_words_and_preserve_observed_columns() {
    const SOURCE: &str = r#"
module Top(input wire [11:0] data, input wire [2:0] mask, output wire [3:0] result);
    wire [3:0] rows [0:2];
    wire [2:0] columns [3:0];
    for (genvar r=0; r<3; r++) begin
        assign rows[r] = data[4*r+:4] & {4{mask[r]}};
        for (genvar c=0; c<4; c++) assign columns[c][r] = rows[r][c];
    end
    for (genvar c=0; c<4; c++) assign result[c] = |columns[c];
endmodule"#;
    let builder = || {
        Simulator::from_sv_sources(vec![(SOURCE, Path::new("columns.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
            .dead_store_policy(DeadStorePolicy::PreserveTopPorts)
    };
    let before = settle_plan(builder().disable_pass(SirPass::NetInlining));
    assert!(stores(&before, ".columns"));
    let after = settle_plan(builder());
    assert!(!stores(&after, ".columns"), "{after}");
    let observed = settle_plan(builder().live_signal(vec![], vec!["columns".into()]));
    assert!(stores(&observed, ".columns"), "{observed}");
    let mut sim = builder().build_native().unwrap();
    let data = sim.signal("data");
    let mask = sim.signal("mask");
    let result = sim.signal("result");
    for value in [0x124u16, 0x738, 0xa5c, 0xf91] {
        for enables in 0..8u8 {
            sim.modify(|io| {
                io.set(data, value);
                io.set(mask, enables);
            })
            .unwrap();
            let expected = (0..3)
                .filter(|i| enables & (1 << i) != 0)
                .fold(0u16, |sum, i| sum | ((value >> (4 * i)) & 15));
            assert_eq!(sim.get(result), expected.into());
        }
    }
}

/// A producer register, the wire connecting two instances, and a RAM port
/// participate in the same event graph, even with a procedure on that edge.
#[test]
fn clocked_consumers_follow_wires_across_instances() {
    let source = r#"
        module Producer(input clk, input [3:0] d, output wire [3:0] o);
            reg [3:0] q;
            always @(posedge clk) q <= d;
            assign o = q;
        endmodule
        module Consumer(input clk, reset, input [3:0] i, input [1:0] a,
                        output reg [3:0] scalar, stored, output reg [3:0] count);
            reg [3:0] ram [0:3];
            always @(posedge clk or posedge reset)
                if (reset) scalar <= 0; else scalar <= i;
            always @(posedge clk) begin
                ram[a] <= i;
                stored <= ram[a];
            end
            always @(posedge clk) count = count + 1;
        endmodule
        module Top(input clk, reset, input [3:0] d, input [1:0] a,
                   output [3:0] scalar, stored, count);
            wire [3:0] link;
            Producer p(clk, d, link);
            Consumer c(clk, reset, link, a, scalar, stored, count);
        endmodule"#;
    let builder = || {
        Simulator::from_sv_sources(vec![(source, Path::new("graph.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
    };
    let plan = settle_plan(builder());
    for net in [".link", "p.o", "c.i"] {
        assert!(!stores(&plan, net), "{net} is still materialized:\n{plan}");
    }
    let mut sim = builder().build_interpreter().unwrap();
    let clock = sim.event("clk");
    let [d, a, scalar, stored, count] =
        ["d", "a", "scalar", "stored", "count"].map(|name| sim.signal(name));
    for (cycle, input, address, sample, old) in [
        (1, 5, 2, 0, 0),
        (2, 7, 2, 5, 0),
        (3, 1, 1, 7, 0),
        (4, 3, 2, 1, 5),
    ] {
        sim.modify(|io| {
            io.set(d, input as u8);
            io.set(a, address as u8);
        })
        .unwrap();
        sim.tick(clock).unwrap();
        for (signal, value) in [(scalar, sample), (stored, old), (count, cycle)] {
            assert_eq!(sim.get(signal), (value as u8).into(), "cycle {cycle}");
        }
    }
    let reset = sim.event("reset");
    sim.tick(reset).unwrap();
    assert_eq!(sim.get(scalar), 0u8.into());
}

#[test]
fn clock_samples_a_procedural_case_without_materializing_its_result() {
    let source = r#"
        module Gate(input en, hold, output gate);
            assign gate = en & ~hold;
        endmodule
        module Top(input clk, advance, output [1:0] value, output inverted, parity, toggle, pending);
            reg [1:0] state, next_state;
            reg enable, bit_state, next_bit, held, next_held;
            reg hold;
            wire gate;
            Gate g(enable, hold, gate);
            always @(posedge clk) begin
                enable <= advance;
                hold <= state == 2'd1;
                state <= next_state;
                bit_state <= next_bit;
                held <= next_held;
            end
            always_comb begin
                case (state)
                    2'd0: next_state = gate ? 2'd3 : 2'd0;
                    2'd3: next_state = 2'd1;
                    2'd1: next_state = 2'd2;
                    default: next_state = 2'd0;
                endcase
                next_bit = bit_state == 1'b0 ? 1'b1 : 1'b0;
                if (enable) next_held = 1'b1;
                else if (state == 2'd2) next_held = 1'b0;
                else next_held = held;
            end
            assign pending = held;
            assign toggle = bit_state;
            assign value = state;
            assign parity = ^state;
            assign inverted = ~advance;
        endmodule"#;
    let builder = || {
        Simulator::from_sv_sources(vec![(source, Path::new("sample-case.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
    };
    let plan = settle_plan(builder());
    for net in [".next_state", ".gate", "g.gate"] {
        assert!(!stores(&plan, net), "{net} remains in {plan}");
    }
    // next_bit powers up nonzero as an expression. The held bit only
    // supplies a fallback value to its driver. Both keep their settle stores.
    assert!(stores(&plan, ".next_bit"), "{plan}");
    assert!(stores(&plan, ".next_held"), "{plan}");
    let kept = settle_plan(builder().dead_store_policy(DeadStorePolicy::Off));
    assert!(stores(&kept, ".next_state"), "{kept}");

    let mut sim = builder().build_interpreter().unwrap();
    let clk = sim.event("clk");
    let [advance, value, toggle] = ["advance", "value", "toggle"].map(|name| sim.signal(name));
    let mut expected_bit = 0u8;
    for (input, expected) in [(1u8, 0u8), (0, 3), (0, 1), (0, 2), (0, 0), (0, 0)] {
        sim.modify(|io| io.set(advance, input)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(value), expected.into());
        expected_bit ^= 1;
        assert_eq!(sim.get(toggle), expected_bit.into());
    }
}

#[test]
fn a_procedure_samples_the_settled_copy_before_its_blocking_write_propagates() {
    let source = r#"
        module Top(input clk, input [7:0] d, output reg [7:0] sample);
            reg [7:0] active;
            wire [7:0] copy = active;
            always @(posedge clk) begin
                active = d;
                sample <= copy;
            end
        endmodule"#;
    let build = || {
        Simulator::from_sv_sources(vec![(source, Path::new("blocking.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
    };
    let mut sim = build().build_interpreter().unwrap();
    let clk = sim.event("clk");
    let [d, sample] = ["d", "sample"].map(|name| sim.signal(name));
    for (input, previous) in [(7u8, 0u8), (13, 7), (1, 13)] {
        sim.modify(|io| io.set(d, input)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(sample), previous.into());
    }
}

#[test]
fn per_destination_samples_keep_the_statement_order_and_case_priority() {
    let source = r#"
        module Top(input clk, input [1:0] sel, input [7:0] d,
                   output reg [7:0] t,q,old,alt,copy,selected);
            always @(posedge clk) begin
                old <= t;
                t = d*d;
                casez (sel)
                    2'b0?: q <= t;
                    2'b?0: alt <= t;
                    default: q <= t + 1;
                endcase
            end
            always @(posedge clk) begin
                copy = d;
                casez (sel)
                    2'b0?: copy = d + 1;
                    2'b?0: selected <= d + 2;
                    default: selected <= d + 3;
                endcase
            end
        endmodule"#;
    let build = || {
        Simulator::from_sv_sources(vec![(source, Path::new("destinations.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
    };
    fn run<B: celox::SimBackend>(mut sim: Simulator<B>) {
        let clk = sim.event("clk");
        let [d, sel] = ["d", "sel"].map(|name| sim.signal(name));
        let out = ["old", "t", "q", "alt", "copy", "selected"].map(|name| sim.signal(name));
        for (input, selector, expected) in [
            (2u8, 0u8, [0u8, 4, 4, 0, 3, 0]),
            (3, 2, [4, 9, 4, 9, 3, 5]),
            (5, 3, [9, 25, 26, 9, 5, 8]),
            (7, 1, [25, 49, 49, 9, 8, 8]),
        ] {
            sim.modify(|io| {
                io.set(d, input);
                io.set(sel, selector);
            })
            .unwrap();
            sim.tick(clk).unwrap();
            for (signal, value) in out.into_iter().zip(expected) {
                assert_eq!(sim.get(signal), value.into(), "d={input}, sel={selector}");
            }
        }
    }
    run(build().build_interpreter().unwrap());
    run(build().build().unwrap());
}

/// The settle computes the product once. Both clocks sample that value even
/// through a wire alias, and the observed output follows later input changes.
#[test]
fn two_clock_domains_sample_one_observed_calculation() {
    use celox_design::BinaryOp;
    use celox_sir::SIRInstruction;

    let source = r#"
module Top(input bit src, a, b, input bit [7:0] i, j,
           output bit [7:0] seen, qa, qb);
    bit [7:0] x, y;
    always_ff @(posedge src) begin x <= i; y <= j; end
    wire [7:0] product = x * y;
    wire [7:0] copy = product;
    assign seen = product;
    always_ff @(posedge a) qa <= product;
    always_ff @(posedge b) qb <= copy;
endmodule"#;
    let build = Simulator::from_sv_sources(vec![(source, Path::new("shared-product.sv"))], "Top")
        .opt_level(OptLevel::O2)
        .four_state(false)
        .enable_pass(SirPass::NextStateDag)
        .disable_pass(SirPass::PartitionedSettle)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let mut sim = build.res.unwrap();
    let (src, a, b) = (sim.event("src"), sim.event("a"), sim.event("b"));
    let program = build.trace.pre_optimized_sir.unwrap().sir;
    let count = |units: &[_]| {
        units
            .iter()
            .flat_map(|u: &celox_sir::ExecutionUnit<_>| u.blocks.values())
            .flat_map(|block| &block.instructions)
            .filter(|inst| matches!(inst, SIRInstruction::Binary(_, _, BinaryOp::Mul, _)))
            .count()
    };
    assert_eq!(count(&program.updates.observe), 1);
    for clock in [a, b] {
        let update = &program.updates.events[&clock.addr];
        assert!(update.entry(celox_sir::UpdateEntry::FromInputs).is_none());
        for at in [
            celox_sir::UpdateEntry::Transition,
            celox_sir::UpdateEntry::Sample,
        ] {
            assert_eq!(count(update.entry(at).unwrap()), 0);
        }
    }
    let (i, j) = (sim.signal("i"), sim.signal("j"));
    let (seen, qa, qb) = (sim.signal("seen"), sim.signal("qa"), sim.signal("qb"));
    sim.modify(|io| {
        io.set(i, 9u8);
        io.set(j, 7u8);
    })
    .unwrap();
    sim.tick(src).unwrap();
    sim.tick(a).unwrap();
    assert_eq!(sim.get(qa), 63u8.into());
    sim.modify(|io| io.set(i, 11u8)).unwrap();
    sim.tick(src).unwrap();
    assert_eq!(sim.get(seen), 77u8.into());
    sim.tick(b).unwrap();
    assert_eq!(sim.get(qb), 77u8.into());
    assert_eq!(sim.get(qa), 63u8.into());
}

/// A wait is an observation root even when its body does not read the
/// observed value. Check occurrences separated from the sampling edge.
#[test]
fn a_wait_and_a_clock_share_the_settled_product() {
    let source = r#"`timescale 1ns/1ns
module Top(output int changes, output bit [7:0] sampled);
    bit clk;
    bit [7:0] x;
    always #1 clk = ~clk;
    wire [7:0] product = x * x;
    initial forever begin @(product); changes++; end
    always_ff @(posedge clk) sampled <= product;
    initial begin
        #2 x = 5;
        #2 x = 7;
    end
endmodule"#;
    let build = || {
        celox::Simulation::from_sv_sources(vec![(source, Path::new("observed-product.sv"))], "Top")
            .four_state(false)
            .opt_level(OptLevel::O2)
            .enable_pass(SirPass::NextStateDag)
    };
    for partition in [false, true] {
        let builder = if partition {
            build().settle_partition(SettlePartition::Always)
        } else {
            build().disable_pass(SirPass::PartitionedSettle)
        };
        let mut sim = builder.build().unwrap();
        let (changes, sampled) = (sim.signal("changes"), sim.signal("sampled"));
        sim.run_until(3).unwrap();
        assert_eq!(sim.get(changes), 1u32.into());
        assert_eq!(sim.get(sampled), 25u8.into());
        sim.run_until(4).unwrap();
        assert_eq!(sim.get(changes), 2u32.into());
        assert_eq!(sim.get(sampled), 25u8.into());
    }
}
