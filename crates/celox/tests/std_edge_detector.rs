use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

const TOP_W1: &str = r#"
module Top (
    clk      : input  clock,
    rst      : input  reset,
    i_clear  : input  logic,
    i_data   : input  logic,
    o_edge   : output logic,
    o_posedge: output logic,
    o_negedge: output logic,
) {
    inst u: edge_detector #(WIDTH: 1) (
        i_clk: clk,
        i_rst: rst,
        i_clear,
        i_data,
        o_edge,
        o_posedge,
        o_negedge,
    );
}
"#;

all_backends! {

    // Edge detector: output is combinational (assign), internal `data` register
    // updates on clock edge. So the output reflects the difference between
    // current i_data and the registered (previous-cycle) value.
    //
    // Flow: set i_data -> tick (FF captures i_data into `data`) -> change i_data
    //       -> eval_comb -> read outputs (edge between old data and new i_data)
    fn test_edge_detector_basic(sim) {
        @setup { let code = format!("{}\n{TOP_W1}", test_utils::veryl_std::source(&["edge_detector", "edge_detector.veryl"])); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_clear = sim.signal("i_clear");
    let i_data = sim.signal("i_data");
    let o_edge = sim.signal("o_edge");
    let o_posedge = sim.signal("o_posedge");
    let o_negedge = sim.signal("o_negedge");

    // Reset: data register = 0 (INITIAL_VALUE='0)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_clear, 0u8);
        io.set(i_data, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();

    // Tick to latch i_data=0 into data register
    sim.tick(clk).unwrap();

    // No edge when data and i_data are both 0
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_posedge), 0, "no edge: data=0, i_data=0");
    assert_eq!(sim.get_as::<u8>(o_negedge), 0);

    // Rising edge: change i_data to 1 (data register still 0)
    sim.modify(|io| io.set(i_data, 1u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_posedge), 1, "posedge: i_data=1, data=0");
    assert_eq!(sim.get_as::<u8>(o_negedge), 0);
    assert_eq!(sim.get_as::<u8>(o_edge), 1);

    // Tick to latch i_data=1 into data register
    sim.tick(clk).unwrap();

    // No edge: data=1, i_data=1
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_posedge), 0, "no edge: data=1, i_data=1");
    assert_eq!(sim.get_as::<u8>(o_negedge), 0);
    assert_eq!(sim.get_as::<u8>(o_edge), 0);

    // Falling edge: change i_data to 0 (data register is 1)
    sim.modify(|io| io.set(i_data, 0u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.get_as::<u8>(o_posedge), 0);
    assert_eq!(sim.get_as::<u8>(o_negedge), 1, "negedge: i_data=0, data=1");
    assert_eq!(sim.get_as::<u8>(o_edge), 1);

    }

    // Clear suppresses posedge/negedge outputs.
    //
    // Note: the stdlib edge_detector has operator precedence such that:
    //   o_edge    = i_data ^ (data & ~i_clear)   -- XOR, not fully masked by clear
    //   o_posedge = i_data & ~data & ~i_clear     -- AND, fully masked by clear
    //   o_negedge = ~i_data & data & ~i_clear     -- AND, fully masked by clear
    fn test_edge_detector_clear(sim) {
        @setup { let code = format!("{}\n{TOP_W1}", test_utils::veryl_std::source(&["edge_detector", "edge_detector.veryl"])); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_clear = sim.signal("i_clear");
    let i_data = sim.signal("i_data");
    let o_posedge = sim.signal("o_posedge");
    let o_negedge = sim.signal("o_negedge");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_clear, 0u8);
        io.set(i_data, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap(); // latch data=0

    // Create a rising edge condition (i_data=1, data=0) with clear active
    sim.modify(|io| {
        io.set(i_data, 1u8);
        io.set(i_clear, 1u8);
    })
    .unwrap();
    sim.eval_comb().unwrap();

    assert_eq!(
        sim.get_as::<u8>(o_posedge),
        0,
        "clear should suppress posedge"
    );
    assert_eq!(
        sim.get_as::<u8>(o_negedge),
        0,
        "clear should suppress negedge"
    );

    }

    // Multi-bit edge detection (WIDTH=4) using o_edge output.
    //
    // Note: o_edge uses XOR and correctly reflects per-bit changes.
    // o_posedge/o_negedge use AND with 1-bit ~i_clear which limits multi-bit
    // behavior due to zero-extension. We test o_edge for multi-bit correctness.
    fn test_edge_detector_multibit(sim) {
        @ignore_on(veryl);
        @setup { let top = r#"
module Top (
clk      : input  clock,
rst      : input  reset,
i_data   : input  logic<4>,
o_edge   : output logic<4>,
o_posedge: output logic<4>,
o_negedge: output logic<4>,
) {
inst u: edge_detector #(WIDTH: 4) (
i_clk  : clk,
i_rst  : rst,
i_clear: 1'b0,
i_data,
o_edge,
o_posedge,
o_negedge,
);
}
"#;
let code = format!("{}\n{top}", test_utils::veryl_std::source(&["edge_detector", "edge_detector.veryl"])); }
        @build Simulator::builder(&code, "Top");
    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let i_data = sim.signal("i_data");
    let o_edge = sim.signal("o_edge");

    // Reset
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(i_data, 0u8);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    sim.modify(|io| io.set(rst, 1u8)).unwrap();
    sim.tick(clk).unwrap(); // data register = 0b0000

    // Set bits 0 and 2 high (0b0101): edge detected on bits 0,2
    sim.modify(|io| io.set(i_data, 0b0101u8)).unwrap();
    sim.eval_comb().unwrap();
    // o_edge = i_data ^ (data & ~i_clear) = 0101 ^ (0000 & 0001) = 0101 ^ 0000 = 0101
    assert_eq!(
        sim.get_as::<u8>(o_edge),
        0b0101,
        "bits 0,2 should have edge"
    );

    // Tick to latch 0b0101 into data register
    sim.tick(clk).unwrap();

    // Change to 0b1010: all 4 bits change
    sim.modify(|io| io.set(i_data, 0b1010u8)).unwrap();
    sim.eval_comb().unwrap();
    // o_edge = 1010 ^ (0101 & 0001) = 1010 ^ 0001 = 1011
    // Note: due to 1-bit clear masking, o_edge doesn't equal i_data ^ data for bits > 0
    assert_eq!(
        sim.get_as::<u8>(o_edge),
        0b1011,
        "edge reflects XOR with masked data"
    );

    // Tick to latch 0b1010
    sim.tick(clk).unwrap();

    // No change: i_data=0b1010, data=0b1010
    sim.eval_comb().unwrap();
    // o_edge = 1010 ^ (1010 & 0001) = 1010 ^ 0000 = 1010
    // Due to masking, "no change" still shows edges on upper bits
    assert_eq!(
        sim.get_as::<u8>(o_edge),
        0b1010,
        "steady state: upper bits reflect masking artifact"
    );

    }
}
