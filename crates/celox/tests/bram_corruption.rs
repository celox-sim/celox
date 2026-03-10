//! Regression test for memory array corruption on re-write.
//!
//! Writing to one index of an array (e.g. mem[0]) must not corrupt
//! values stored at other indices (e.g. mem[4], mem[8], mem[12]).

use celox::{OptimizeOptions, Simulator};

const BRAM_CODE: &str = r#"
module BramTdp #(
    param ADDR_W: u32 = 4,
    param DATA_W: u32 = 8,
) (
    clk: input clock,
    a_we  : input  logic        ,
    a_addr: input  logic<ADDR_W>,
    a_din : input  logic<DATA_W>,
    a_dout: output logic<DATA_W>,
    b_we  : input  logic        ,
    b_addr: input  logic<ADDR_W>,
    b_din : input  logic<DATA_W>,
    b_dout: output logic<DATA_W>,
) {
    const DEPTH: u32 = 1 << ADDR_W;
    var mem  : logic<DATA_W> [DEPTH];

    always_ff (clk) {
        if a_we {
            mem[a_addr] = a_din;
        }
        if b_we {
            mem[b_addr] = b_din;
        }
        a_dout = mem[a_addr];
        b_dout = mem[b_addr];
    }
}
"#;

fn run_bram_test(opts: OptimizeOptions) {
    let mut sim = Simulator::builder(BRAM_CODE, "BramTdp")
        .optimize_options(opts)
        .build()
        .unwrap();

    let clk = sim.event("clk");
    let a_we = sim.signal("a_we");
    let a_addr = sim.signal("a_addr");
    let a_din = sim.signal("a_din");
    let a_dout = sim.signal("a_dout");
    let b_we = sim.signal("b_we");
    let b_addr = sim.signal("b_addr");
    let b_din = sim.signal("b_din");

    sim.set(a_we, 0u8);
    sim.set(a_addr, 0u8);
    sim.set(a_din, 0u8);
    sim.set(b_we, 0u8);
    sim.set(b_addr, 0u8);
    sim.set(b_din, 0u8);
    sim.tick(clk).unwrap();

    // Write to addr 0, 4, 8, 12
    for addr in [0u8, 4, 8, 12] {
        sim.set(a_we, 1u8);
        sim.set(a_addr, addr);
        sim.set(a_din, 0xA0u8.wrapping_add(addr));
        sim.tick(clk).unwrap();
    }

    // Verify all writes survived
    for addr in [0u8, 4, 8, 12] {
        sim.set(a_we, 0u8);
        sim.set(a_addr, addr);
        sim.tick(clk).unwrap();
        let val: u8 = sim.get_as(a_dout);
        assert_eq!(
            val,
            0xA0u8.wrapping_add(addr),
            "Initial write: mem[{addr}] should be 0x{:02x}, got 0x{val:02x}",
            0xA0u8.wrapping_add(addr)
        );
    }

    // Re-write addr 0 with a new value
    sim.set(a_we, 1u8);
    sim.set(a_addr, 0u8);
    sim.set(a_din, 0xBBu8);
    sim.tick(clk).unwrap();

    // addr 0 should have the new value
    sim.set(a_we, 0u8);
    sim.set(a_addr, 0u8);
    sim.tick(clk).unwrap();
    let val: u8 = sim.get_as(a_dout);
    assert_eq!(val, 0xBB, "Re-write: mem[0] should be 0xBB, got 0x{val:02x}");

    // Other addresses must NOT be corrupted
    for addr in [4u8, 8, 12] {
        sim.set(a_addr, addr);
        sim.tick(clk).unwrap();
        let val: u8 = sim.get_as(a_dout);
        assert_eq!(
            val,
            0xA0u8.wrapping_add(addr),
            "After re-write of addr 0: mem[{addr}] should be 0x{:02x}, got 0x{val:02x}",
            0xA0u8.wrapping_add(addr)
        );
    }
}

#[test]
fn bram_rewrite_no_corruption_all_opts() {
    run_bram_test(OptimizeOptions::all());
}

#[test]
fn bram_rewrite_no_corruption_no_opts() {
    run_bram_test(OptimizeOptions::none());
}
