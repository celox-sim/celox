use celox::SimulatorBuilder;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
fn reset_constants_and_unknown_values_share_a_working_join(sim) {
    // Veryl's reference simulator disagrees on the known payload bits of the
    // mixed-X 64-bit value below. Exercise the four Celox backends directly.
    @omit_veryl;
    // The SV frontend rejects four-state always_ff event signals, as in the
    // existing four_state FF tests. Keep every Celox execution backend covered.
    @ignore_on(sv);
    @setup {
        let source = r#"
            module Top (
                clk: input clock, rst_n: input reset_async_low,
                enable: input logic, d: input logic<64>,
                a: output logic<64>, b: output logic<64>,
            ) {
                always_ff (clk, rst_n) {
                    if_reset { a = 64'h1357; }
                    else if enable { a = d; }
                }
                always_ff (clk) {
                    if !rst_n { b = 64'h2468; }
                    else { b = a; }
                }
            }
        "#;
    }
    @build SimulatorBuilder::new(source, "Top").opt_level(celox::OptLevel::O0).four_state(true);
    let clk = sim.event("clk");
    let rst = sim.signal("rst_n");
    let enable = sim.signal("enable");
    let d = sim.signal("d");
    let a = sim.signal("a");
    let b = sim.signal("b");
    for _ in 0..2 {
        sim.modify(|io| { io.set(rst, 0u8); io.set(enable, 0u8); io.set(d, 0u64); }).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get_four_state(a), (0x1357u32.into(), 0u8.into()));
        assert_eq!(sim.get_four_state(b), (0x2468u32.into(), 0u8.into()));
        sim.modify(|io| io.set(rst, 1u8)).unwrap();
        let mut previous = sim.get_four_state(a);
        for (value, mask, enabled) in [
            (0x1234_5678_9abc_def0u64, 0xff00_1234_5678_00ffu64, true),
            (0, 0, false),
            (0, u64::MAX, true),
            (u64::MAX, 0, false),
            (0xfedc_ba98_7654_3210, 0, true),
        ] {
            let expected = if enabled { ((value | mask).into(), mask.into()) } else { previous.clone() };
            sim.modify(|io| {
                io.set_four_state(d, (value | mask).into(), mask.into());
                io.set(enable, u8::from(enabled));
            }).unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(sim.get_four_state(a), expected);
            assert_eq!(sim.get_four_state(b), previous);
            previous = expected;
        }
    }
}
}

const STREAM: &str = r#"
module Top #(param UNUSED: u32 = 0,) (
    clk: input clock, rst_n: input reset_async_low,
    addr: input logic<4>, data: input logic<8>, q: output logic<8>,
) {
    var addr_q: logic<4>;
    var data_q: logic<8>;
    var ram: logic<8> [16];
    always_ff (clk, rst_n) {
        if_reset { addr_q = 0; data_q = 0; }
        else { addr_q = addr; data_q = data; }
    }
    always_ff (clk) { ram[addr_q] = data_q; }
    assign q = ram[addr];
}
"#;

#[test]
fn acyclic_shared_clock_inputs_do_not_require_a_working_copy() {
    use celox_design::{STABLE_REGION, WORKING_REGION};
    use celox_sir::SIRInstruction;
    let result = SimulatorBuilder::new(STREAM, "Top")
        .opt_level(celox::OptLevel::O0)
        .trace_pre_optimized_sir()
        .build_with_trace();
    let clk = result.res.unwrap().event("clk").addr;
    let program = result.trace.pre_optimized_sir.unwrap();
    let mut stores = 0;
    for instruction in program.sir.eval_apply_ffs[&clk]
        .iter()
        .flat_map(|unit| unit.blocks.values())
        .flat_map(|block| &block.instructions)
    {
        match instruction {
            SIRInstruction::Commit(source, destination, ..) => {
                assert_ne!(source.region, WORKING_REGION, "{instruction:?}");
                assert_ne!(destination.region, WORKING_REGION, "{instruction:?}");
            }
            SIRInstruction::Store(address, ..) => {
                assert_ne!(address.region, WORKING_REGION, "{instruction:?}");
                stores += usize::from(address.region == STABLE_REGION);
            }
            _ => {}
        }
    }
    // Dynamic RAM writes may use the sparse dirty set. The two scalar input
    // registers must update directly after their old values have been read.
    assert!(stores >= 2);
}

all_backends! {
fn acyclic_shared_clock_array_samples_the_previous_input_pair(sim) {
    @build SimulatorBuilder::new(STREAM, "Top").opt_level(celox::OptLevel::O0);
    let clk = sim.event("clk");
    let rst = sim.signal("rst_n");
    let addr = sim.signal("addr");
    let data = sim.signal("data");
    let q = sim.signal("q");
    sim.modify(|io| { io.set(rst, 0u8); io.set(addr, 0u8); io.set(data, 0u8); }).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for index in 0u8..16 {
        sim.modify(|io| { io.set(addr, index); io.set(data, index * 7 + 13); }).unwrap();
        sim.tick(clk).unwrap();
    }
    sim.tick(clk).unwrap();
    for index in 0u8..16 {
        sim.modify(|io| io.set(addr, index)).unwrap();
        assert_eq!(sim.get(q), (index * 7 + 13).into(), "row {index}");
    }
}
}

all_backends! {
fn reset_groups_sample_the_same_pre_edge_state(sim) {
    @setup {
        let source = r#"
            module Top #(param UNUSED: u32 = 0,) (
                clk: input clock,
                rst_n: input reset_async_low,
                a: output logic<8>,
                b: output logic<8>,
            ) {
                always_ff (clk, rst_n) {
                    if_reset { a = 8'h13; }
                    else { a = b; }
                }
                always_ff (clk) {
                    if !rst_n { b = 8'h57; }
                    else { b = a; }
                }
            }
        "#;
    }
    @build SimulatorBuilder::new(source, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst_n");
    let a = sim.signal("a");
    let b = sim.signal("b");
    sim.modify(|io| io.set(rst, 0u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(a), 0x13u8.into());
    assert_eq!(sim.get(b), 0x57u8.into());
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for cycle in 0..8 {
        sim.tick(clk).unwrap();
        let (want_a, want_b) = if cycle % 2 == 0 { (0x57u8, 0x13u8) } else { (0x13u8, 0x57u8) };
        assert_eq!(sim.get(a), want_a.into(), "cycle {cycle}");
        assert_eq!(sim.get(b), want_b.into(), "cycle {cycle}");
    }
}
}
