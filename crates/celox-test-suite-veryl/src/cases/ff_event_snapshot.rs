use crate::Design;

cases! { Sequential, "ff_event_snapshot";

fn ff_read_array_uses_previous_index(sim) {
    // Veryl 0.21.0 misses reads used only in indices/selects during FF
    // classification. Re-enable its reference run when read tracking is fixed.

    @setup {
        let source = r#"
            module Top (
                clk: input clock, rst_n: input reset_async_low,
                addr: input logic<2>, q: output logic<8>,
            ) {
                var addr_q: logic<2>;
                var mem: logic<8> [4];
                assign mem[0] = 8'h10;
                assign mem[1] = 8'h11;
                assign mem[2] = 8'h12;
                assign mem[3] = 8'h13;

                always_ff (clk) { addr_q = addr; }
                // Separate reset groups exercise dependency-based FF scheduling.
                always_ff (clk, rst_n) {
                    if_reset { q = 0; }
                    else { q = mem[addr_q]; }
                }
            }
        "#;
    }
    @build Design::new(source, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst_n");
    let addr = sim.signal("addr");
    let q = sim.signal("q");

    // Seed addr_q without relying on its initial value. It must only be read
    // as an index: a counter's self-read could mask missing index dependencies.
    sim.modify(|io| { io.set(rst, 0u8); io.set(addr, 0u8); }).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for (next_addr, expected) in [(3u8, 0x10u8), (1, 0x13), (2, 0x11), (0, 0x12)] {
        sim.modify(|io| io.set(addr, next_addr)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), expected.into(), "new addr input {next_addr}");
    }
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x10u8.into());
}



fn ff_read_bit_select_uses_previous_index(sim) {
    // Veryl 0.21.0 misses bit-select index reads during FF classification.
    // The SV frontend rejects this dynamic bit-select in always_ff lowering.

    @setup {
        let source = r#"
            module Top (
                clk: input clock, rst_n: input reset_async_low,
                index: input logic<2>, q: output logic,
            ) {
                var index_q: logic<2>;
                var bits: logic<4>;
                assign bits = 4'b1010;

                always_ff (clk) { index_q = index; }
                // Separate reset groups exercise dependency-based FF scheduling.
                always_ff (clk, rst_n) {
                    if_reset { q = 0; }
                    else { q = bits[index_q]; }
                }
            }
        "#;
    }
    @build Design::new(source, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst_n");
    let index = sim.signal("index");
    let q = sim.signal("q");

    sim.modify(|io| { io.set(rst, 0u8); io.set(index, 0u8); }).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for (next_index, expected) in [(3u8, 0u8), (0, 1), (1, 0), (2, 1)] {
        sim.modify(|io| io.set(index, next_index)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), expected.into(), "new bit index {next_index}");
    }
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0u8.into());
}



fn ff_read_part_select_uses_previous_index(sim) {
    // Veryl 0.21.0 misses part-select index reads during FF classification.
    // The SV frontend does not support indexed part-selects yet.

    @setup {
        let source = r#"
            module Top (
                clk: input clock, rst_n: input reset_async_low,
                index: input logic<2>, q: output logic<8>,
            ) {
                var index_q: logic<2>;
                var word: logic<32>;
                assign word = 32'h44332211;

                always_ff (clk) { index_q = index; }
                // Separate reset groups exercise dependency-based FF scheduling.
                always_ff (clk, rst_n) {
                    if_reset { q = 0; }
                    else { q = word[index_q * 8+:8]; }
                }
            }
        "#;
    }
    @build Design::new(source, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst_n");
    let index = sim.signal("index");
    let q = sim.signal("q");

    // Keep bit and part selects in separate designs: either read could
    // otherwise order the whole reader group before the writer group.
    sim.modify(|io| { io.set(rst, 0u8); io.set(index, 0u8); }).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    for (next_index, expected) in [(2u8, 0x11u8), (1, 0x33), (3, 0x22), (0, 0x44)] {
        sim.modify(|io| io.set(index, next_index)).unwrap();
        sim.tick(clk).unwrap();
        assert_eq!(sim.get(q), expected.into(), "new byte index {next_index}");
    }
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(q), 0x11u8.into());
}


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
    @build Design::new(source, "Top");
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
