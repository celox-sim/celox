use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

const RAM_SRC: &str = include_str!("../../../deps/veryl/crates/std/veryl/src/ram/ram.veryl");

all_backends! {

    // Dual-port RAM: write via port A, read via port B (BUFFER_OUT=false, combinational read)
    fn test_ram_write_read(sim) {
        @setup { let top = r#"
module Top (
clk  : input  clock,
rst  : input  reset,
i_wea : input  logic,
i_adra: input  logic<2>,
i_da  : input  logic<8>,
i_adrb: input  logic<2>,
o_qb  : output logic<8>,
) {
inst u_ram: ram #(
WORD_SIZE    : 4,
ADDRESS_WIDTH: 2,
DATA_WIDTH   : 8,
BUFFER_OUT   : 0,
USE_RESET    : 0,
) (
i_clk : clk,
i_rst : rst,
i_clr : 1'b0,
i_mea : 1'b1,
i_wea,
i_adra,
i_da,
i_meb : 1'b1,
i_adrb,
o_qb,
);
}
"#;
let code = format!("{RAM_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_wea = sim.signal("i_wea");
    let i_adra = sim.signal("i_adra");
    let i_da = sim.signal("i_da");
    let i_adrb = sim.signal("i_adrb");
    let o_qb = sim.signal("o_qb");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_wea, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Write 0xAA to address 0
    sim.modify(|io| {
        io.set(i_wea, 1u8);
        io.set(i_adra, 0u8);
        io.set(i_da, 0xAAu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Write 0xBB to address 1
    sim.modify(|io| {
        io.set(i_adra, 1u8);
        io.set(i_da, 0xBBu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Write 0xCC to address 2
    sim.modify(|io| {
        io.set(i_adra, 2u8);
        io.set(i_da, 0xCCu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Stop writing
    sim.modify(|io| io.set(i_wea, 0u8)).unwrap();

    // Read back: BUFFER_OUT=false means combinational read on port B
    // Address 0
    sim.modify(|io| io.set(i_adrb, 0u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_qb), 0xAA, "addr 0 should be 0xAA");

    // Address 1
    sim.modify(|io| io.set(i_adrb, 1u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_qb), 0xBB, "addr 1 should be 0xBB");

    // Address 2
    sim.modify(|io| io.set(i_adrb, 2u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_qb), 0xCC, "addr 2 should be 0xCC");

    }

    // Overwrite same address and verify latest value
    fn test_ram_overwrite(sim) {
        @setup { let top = r#"
module Top (
clk  : input  clock,
rst  : input  reset,
i_wea : input  logic,
i_adra: input  logic<2>,
i_da  : input  logic<8>,
i_adrb: input  logic<2>,
o_qb  : output logic<8>,
) {
inst u_ram: ram #(
WORD_SIZE    : 4,
ADDRESS_WIDTH: 2,
DATA_WIDTH   : 8,
BUFFER_OUT   : 0,
USE_RESET    : 0,
) (
i_clk : clk,
i_rst : rst,
i_clr : 1'b0,
i_mea : 1'b1,
i_wea,
i_adra,
i_da,
i_meb : 1'b1,
i_adrb,
o_qb,
);
}
"#;
let code = format!("{RAM_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_wea = sim.signal("i_wea");
    let i_adra = sim.signal("i_adra");
    let i_da = sim.signal("i_da");
    let i_adrb = sim.signal("i_adrb");
    let o_qb = sim.signal("o_qb");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_wea, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Write 0x11 to address 0
    sim.modify(|io| {
        io.set(i_wea, 1u8);
        io.set(i_adra, 0u8);
        io.set(i_da, 0x11u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();

    // Overwrite address 0 with 0x22
    sim.modify(|io| io.set(i_da, 0x22u8)).unwrap();
    sim.tick(clk).unwrap();

    // Read address 0 -- should be 0x22
    sim.modify(|io| {
        io.set(i_wea, 0u8);
        io.set(i_adrb, 0u8);
    })
    .unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.get_as::<u8>(o_qb),
        0x22,
        "overwritten value should be 0x22"
    );

    }

    // RAM with USE_RESET=true: clear via i_clr
    fn test_ram_reset_and_clear(sim) {
        @setup { let top = r#"
module Top (
clk   : input  clock,
rst   : input  reset,
i_wea : input  logic,
i_adra: input  logic<2>,
i_da  : input  logic<8>,
i_clr : input  logic,
i_adrb: input  logic<2>,
o_qb  : output logic<8>,
) {
inst u_ram: ram #(
WORD_SIZE    : 4,
ADDRESS_WIDTH: 2,
DATA_WIDTH   : 8,
BUFFER_OUT   : 0,
USE_RESET    : 1,
) (
i_clk : clk,
i_rst : rst,
i_clr,
i_mea : 1'b1,
i_wea,
i_adra,
i_da,
i_meb : 1'b1,
i_adrb,
o_qb,
);
}
"#;
let code = format!("{RAM_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_wea = sim.signal("i_wea");
    let i_adra = sim.signal("i_adra");
    let i_da = sim.signal("i_da");
    let i_clr = sim.signal("i_clr");
    let i_adrb = sim.signal("i_adrb");
    let o_qb = sim.signal("o_qb");

    // Reset (USE_RESET=true: reset clears RAM to 0)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_wea, 0u8);
        io.set(i_clr, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // After reset, all addresses should read 0
    sim.modify(|io| io.set(i_adrb, 0u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_qb), 0, "addr 0 should be 0 after reset");

    // Write 0xFF to address 0
    sim.modify(|io| {
        io.set(i_wea, 1u8);
        io.set(i_adra, 0u8);
        io.set(i_da, 0xFFu8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_wea, 0u8)).unwrap();

    // Verify written
    sim.modify(|io| io.set(i_adrb, 0u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_qb), 0xFF);

    // Clear via i_clr
    sim.modify(|io| io.set(i_clr, 1u8)).unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(i_clr, 0u8)).unwrap();

    // After clear, should read 0
    sim.modify(|io| io.set(i_adrb, 0u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_qb), 0, "addr 0 should be 0 after clear");

    }
}
