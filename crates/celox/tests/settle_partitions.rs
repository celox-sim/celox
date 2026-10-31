//! A partitioned settle runs a partition only when state it reads changed
//! (#2013). Forced on every settle it can cut, it must match the single-pass
//! settle on every backend, whatever wrote the state: a register update, a
//! host write, a loaded state file.
use celox::{BigUint, SettlePartition, Simulator, SirPass};

/// Two clock domains feeding a combinational network deep enough to cut into
/// many partitions, with a memory read through a run-time index and a
/// combinational output depending on a host input only.
const DESIGN: &str = r#"
module Top (
    clk_a: input 'a clock,
    clk_b: input 'b clock,
    rst: input 'a reset,
    din: input 'a logic<8>,
    sel: input 'a logic<2>,
    bypass: input 'a logic<8>,
    sum: output 'a logic<8>,
    mixed: output 'a logic<8>,
    picked: output 'a logic<8>,
    direct: output 'a logic<8>,
    slow: output 'b logic<8>,
) {
    var ra: 'a logic<8>;
    var mem: 'a logic<8> [4];
    var rb: 'b logic<8>;
    var s1: 'a logic<8>;
    var s2: 'a logic<8>;
    var s3: 'a logic<8>;
    var s4: 'a logic<8>;
    always_ff (clk_a, rst) {
        if_reset {
            ra = 0;
        } else {
            ra = ra + din;
            mem[ra[1:0]] = ra ^ din;
        }
    }
    unsafe (cdc) {
        always_ff (clk_b) {
            rb = rb + s4;
        }
        assign s2 = s1 ^ rb;
        assign slow = rb;
    }
    assign s1 = ra + 8'd1;
    assign s3 = {s2[3:0], s1[7:4]};
    assign s4 = s3 + bypass;
    assign sum = s4 + s1;
    assign mixed = s3 ^ s2;
    assign picked = mem[sel];
    assign direct = bypass + 8'd3;
}
"#;

const OUTPUTS: [&str; 5] = ["sum", "mixed", "picked", "direct", "slow"];

fn partitioned() -> celox::SimulatorBuilder<'static, Simulator> {
    Simulator::builder(DESIGN, "Top")
        .settle_partition(SettlePartition::Always)
        .settle_check(true)
}

fn single_pass() -> celox::SimulatorBuilder<'static, Simulator> {
    Simulator::builder(DESIGN, "Top").disable_pass(SirPass::PartitionedSettle)
}

/// Drive both clocks and the inputs with a fixed pseudo-random sequence,
/// reading every output after each host write and each tick.
macro_rules! trace {
    ($sim:expr) => {{
        let sim = &mut $sim;
        let clk_a = sim.event("clk_a");
        let clk_b = sim.event("clk_b");
        let rst = sim.signal("rst");
        let din = sim.signal("din");
        let sel = sim.signal("sel");
        let bypass = sim.signal("bypass");
        let outputs = OUTPUTS.map(|name| sim.signal(name));
        let mut trace = Vec::new();
        macro_rules! read {
            () => {
                outputs
                    .iter()
                    .map(|&signal| sim.get(signal))
                    .collect::<Vec<BigUint>>()
            };
        }
        sim.modify(|io| io.set(rst, 0u8)).unwrap();
        sim.tick(clk_a).unwrap();
        sim.modify(|io| io.set(rst, 1u8)).unwrap();
        let mut seed = 0x2013u32;
        for _ in 0..64 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let bits = seed >> 8;
            // A host write alone must reach the combinational outputs.
            sim.modify(|io| {
                io.set(din, bits as u8);
                io.set(sel, (bits >> 8) as u8 & 3);
                io.set(bypass, (bits >> 10) as u8);
            })
            .unwrap();
            trace.push(read!());
            match bits >> 18 & 3 {
                0 => sim.tick(clk_a).unwrap(),
                1 => sim.tick(clk_b).unwrap(),
                2 => sim.tick_events(&[clk_a, clk_b]).unwrap(),
                _ => {}
            }
            trace.push(read!());
        }
        trace
    }};
}

fn reference() -> Vec<Vec<BigUint>> {
    trace!(single_pass().build().unwrap())
}

#[test]
fn partitioned_interpreter_matches_single_pass() {
    assert_eq!(
        trace!(partitioned().build_interpreter().unwrap()),
        reference()
    );
}

#[test]
fn partitioned_cranelift_matches_single_pass() {
    assert_eq!(
        trace!(partitioned().build_cranelift().unwrap()),
        reference()
    );
}

#[test]
fn partitioned_wasm_matches_single_pass() {
    assert_eq!(trace!(partitioned().build_wasm().unwrap()), reference());
}

#[cfg(any(
    all(target_arch = "x86_64", not(feature = "arm64-codegen")),
    all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
))]
#[test]
fn partitioned_native_matches_single_pass() {
    assert_eq!(trace!(partitioned().build_native().unwrap()), reference());
}

/// A count of dirty partitions says nothing about their share of the work:
/// unrelated partitions can contain much wider expressions or larger processes.
/// Updating one input must not promote its three readers to a full settle.
#[test]
fn sparse_partition_updates_do_not_promote_unrelated_work() {
    const SOURCE: &str = r#"
module Top (
    a: input logic<8>, b: input logic<8>, c: input logic<8>,
    a0: output logic<8>, a1: output logic<8>, a2: output logic<8>,
    b0: output logic<8>, b1: output logic<8>, b2: output logic<8>,
    c0: output logic<8>, c1: output logic<8>, c2: output logic<8>,
) {
    assign a0 = a + 8'd1; assign a1 = a + 8'd2; assign a2 = a + 8'd3;
    assign b0 = b + 8'd1; assign b1 = b + 8'd2; assign b2 = b + 8'd3;
    assign c0 = c + 8'd1; assign c1 = c + 8'd2; assign c2 = c + 8'd3;
}
"#;
    let mut sim = Simulator::builder(SOURCE, "Top")
        .settle_partition(SettlePartition::Always)
        .build_interpreter()
        .unwrap();
    let settle = sim.layout().settle.as_ref().unwrap().clone();
    let flags = || {
        let (memory, _) = sim.memory_as_ptr();
        // SAFETY: these offsets describe this simulator's state allocation.
        unsafe {
            std::slice::from_raw_parts(memory.add(settle.flags_offset), settle.partitions + 2)
                .to_vec()
        }
    };
    // Initial evaluation still runs every partition.
    let initial = flags();
    assert!(initial[..settle.partitions].iter().all(|&flag| flag == 1));
    sim.eval_comb().unwrap();
    let a = sim.signal("a");
    sim.modify(|io| io.set(a, 7u8)).unwrap();
    let (memory, _) = sim.memory_as_ptr();
    let pending =
        unsafe { std::slice::from_raw_parts(memory.add(settle.flags_offset), settle.partitions) };
    let dirty = pending.iter().filter(|&&flag| flag != 0).count();
    assert_eq!(dirty, 3);
    assert_eq!(settle.partitions, 9);
    sim.eval_comb().unwrap();
    let (memory, _) = sim.memory_as_ptr();
    assert_eq!(
        unsafe { *memory.add(settle.flags_offset + settle.partitions + 1) },
        0,
        "a partial update promoted unrelated partitions to a full settle"
    );
    let a2 = sim.signal("a2");
    let b2 = sim.signal("b2");
    assert_eq!(sim.get(a2), BigUint::from(10u8));
    assert_eq!(sim.get(b2), BigUint::from(3u8));

    // A retained raw view still requires full evaluation on every settle.
    let (memory, _) = sim.memory_as_mut_ptr();
    // SAFETY: a names an eight-bit input in this allocation.
    unsafe { *memory.add(a.offset) = 11 };
    sim.eval_comb().unwrap();
    assert_eq!(sim.get(a2), BigUint::from(14u8));
    assert_eq!(
        unsafe { *memory.add(settle.flags_offset + settle.partitions + 1) },
        1
    );
}

/// Loading a state file writes every object, so every partition reading one
/// must run again; the settle's flags themselves are not state.
#[test]
fn loaded_state_reaches_partitions() {
    let mut source = partitioned().build().unwrap();
    trace!(source);
    let file = source.save_state().unwrap();
    assert!(
        file.objects
            .iter()
            .all(|object| !object.path.contains("$settle")),
        "the settle flags are saved as state"
    );
    let mut target = partitioned().build().unwrap();
    // Settle once before the load, so no partition is pending.
    trace!(target);
    target.load_state(&file).unwrap();
    let mut plain = single_pass().build().unwrap();
    plain.load_state(&file).unwrap();
    assert_eq!(trace!(target), trace!(plain));
}

#[cfg(feature = "systemverilog")]
mod systemverilog {
    use std::path::Path;

    use celox::{BigUint, SettlePartition, Simulation, Simulator, SirPass};

    /// A clock gated through a latch (Xilinx BUFGCE): the latch is a process
    /// of the settle, and the gated clock's edge runs another domain.
    const GATED: &str = r#"
`timescale 1ns/1ns
module Top (
    output logic [7:0] fast,
    output logic [7:0] gated,
    output logic [7:0] view
);
    logic clk = 0;
    logic en = 0;
    logic en_l;
    logic [7:0] din = 0;
    always #5 clk = ~clk;
    always #7 en = ~en;
    always #3 din = din + 8'd29;
    always_latch begin
        if (!clk) en_l = en;
    end
    wire gclk = clk & en_l;
    initial begin fast = 0; gated = 0; end
    always_ff @(posedge clk) fast <= fast + din;
    always_ff @(posedge gclk) gated <= gated ^ fast;
    assign view = gated + fast;
endmodule
"#;

    /// A process writing two outputs runs at its first output's place in
    /// the schedule, so the second output changes there too.
    const SPLIT: &str = r#"
`timescale 1ns/1ns
module Top (
    output logic [3:0] y,
    output logic [3:0] b,
    output logic [1:0] w,
    output int n
);
    logic [3:0] a = 4'b0101;
    logic c = 0;
    always_comb begin y = 4'b0; if (c) y = a; b = a; end
    assign w = y[1:0];
    initial begin n = 0; #1 forever begin @(y); n++; end end
    initial begin
        #2 c = 1;
        #1 a = 4'b0110;
        #1 a = 4'b0111;
        #2 c = 0;
        #1 a = 4'b1001;
    end
endmodule
"#;

    /// A whole-variable copy of a process output, read by another process.
    const COPY: &str = r#"
`timescale 1ns/1ns
module Top (
    output logic [7:0] r,
    output logic [7:0] q
);
    logic [7:0] a = 8'd3;
    logic s = 0;
    logic [7:0] p;
    logic [7:0] pc;
    always_comb begin p = 8'd0; q = 8'd0; if (s) begin p = a; q = ~a; end end
    assign pc = p;
    always_comb r = pc + 8'd1;
    always #2 a = a + 8'd5;
    always #3 s = ~s;
endmodule
"#;

    fn trace(
        source: &str,
        outputs: &[&str],
        end: u64,
        settle: Option<SettlePartition>,
    ) -> Vec<Vec<BigUint>> {
        let builder = Simulation::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top");
        let builder = match settle {
            Some(settle) => builder.settle_partition(settle).settle_check(true),
            None => builder.disable_pass(SirPass::PartitionedSettle),
        };
        let mut sim = builder.build().unwrap();
        let outputs: Vec<_> = outputs.iter().map(|name| sim.signal(name)).collect();
        let mut trace = Vec::new();
        for time in 0..end {
            sim.run_until(time).unwrap();
            trace.push(outputs.iter().map(|&signal| sim.get(signal)).collect());
        }
        trace
    }

    fn matches_single_pass(source: &str, outputs: &[&str], end: u64) -> Vec<Vec<BigUint>> {
        let single = trace(source, outputs, end, None);
        assert_eq!(
            trace(source, outputs, end, Some(SettlePartition::Always)),
            single
        );
        single
    }

    #[test]
    fn latch_gated_clock_matches_single_pass() {
        let single = matches_single_pass(GATED, &["fast", "gated", "view"], 200);
        // The gated domain must have run.
        assert_ne!(single.last().unwrap()[1], BigUint::from(0u8));
    }

    #[test]
    fn multi_output_process_matches_single_pass() {
        let single = matches_single_pass(SPLIT, &["y", "b", "w", "n"], 10);
        // At 5, y and w follow a through the intermediate 0.
        assert_eq!(single[5][2], BigUint::from(3u8));
    }

    /// A padded unpacked array (13-bit elements, two bytes each in the
    /// element-strided layout) whose element writes publish through one bulk
    /// commit of the whole array: the commit must mark the reader of the last
    /// element when only that element changes.
    const PADDED: &str = r#"
module Top (
    input logic clk,
    input logic we,
    input logic [2:0] idx,
    input logic [12:0] d,
    input logic [2:0] ri,
    output logic [12:0] q,
    output logic [12:0] last
);
    logic [12:0] m [8];
    always_ff @(posedge clk) if (we) m[idx] <= d;
    assign q = m[ri];
    assign last = m[7];
endmodule
"#;

    /// Whether `m` is element-strided, and the outputs after each tick.
    macro_rules! padded_trace {
        ($sim:expr) => {{
            let mut sim = $sim;
            let strided = sim.signal("m").array_layout.is_some();
            let (clk, we, idx, d, ri) = (
                sim.event("clk"),
                sim.signal("we"),
                sim.signal("idx"),
                sim.signal("d"),
                sim.signal("ri"),
            );
            let outputs = ["last", "q"].map(|name| sim.signal(name));
            let mut trace: Vec<Vec<BigUint>> = Vec::new();
            for step in 0..12u16 {
                sim.modify(|io| {
                    io.set(we, 1u8);
                    // Mostly the last element, sometimes the first.
                    io.set(idx, if step % 3 == 2 { 0u8 } else { 7u8 });
                    io.set(d, step.wrapping_mul(0x9e5) & 0x1fff);
                    io.set(ri, 0u8);
                })
                .unwrap();
                sim.tick(clk).unwrap();
                trace.push(outputs.iter().map(|&signal| sim.get(signal)).collect());
            }
            (strided, trace)
        }};
    }

    fn padded(four_state: bool) -> celox::SimulatorBuilder<'static, Simulator> {
        Simulator::from_sv_sources(vec![(PADDED, Path::new("padded.sv"))], "Top")
            .four_state(four_state)
    }

    #[test]
    fn element_writes_to_a_padded_array_match_single_pass() {
        for four_state in [false, true] {
            let (_, single) = padded_trace!(
                padded(four_state)
                    .disable_pass(SirPass::PartitionedSettle)
                    .build_interpreter()
                    .unwrap()
            );
            #[cfg(any(
                all(target_arch = "x86_64", not(feature = "arm64-codegen")),
                all(target_arch = "aarch64", not(feature = "x86_64-codegen"))
            ))]
            {
                let (strided, trace) = padded_trace!(
                    padded(four_state)
                        .settle_partition(SettlePartition::Always)
                        .build_native()
                        .unwrap()
                );
                assert!(strided, "native stores the array strided");
                assert_eq!(trace, single, "native, four_state={four_state}");
            }
            let (strided, trace) = padded_trace!(
                padded(four_state)
                    .settle_partition(SettlePartition::Always)
                    .build_tiered()
                    .unwrap()
            );
            assert!(strided, "the tiered interpreter stores the array strided");
            assert_eq!(trace, single, "tiered, four_state={four_state}");
        }
    }

    #[test]
    fn copy_of_a_process_output_matches_single_pass() {
        matches_single_pass(COPY, &["r", "q"], 40);
    }
}
