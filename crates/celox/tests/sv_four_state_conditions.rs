#![cfg(feature = "systemverilog")]
//! A four-state build decides which branch conditions are complements by
//! whether their operands can hold unknown bits. Operands are shared: a
//! counter that thirty-two conditional statements increment is thirty-two
//! nodes, each used by the next twice, not the 2^32 paths through them.
use celox::Simulation;
use std::path::Path;

/// The bit gather of the RISC-V `bcompress` instruction: the bits of `rs1`
/// that `rs2` selects, packed at the bottom.
#[test]
fn conditions_over_a_conditionally_incremented_counter_compile() {
    let source = r#"
    module Top(input logic [31:0] rs1, rs2, output logic [31:0] d);
      logic test_bit;
      integer i, j;
      always_comb begin
        j = 0;
        test_bit = 1'b0;
        d = 32'b0;
        for (i = 0; i < 32; i++) begin
          test_bit = rs2[i];
          if (test_bit) begin
            d[j] = rs1[i];
            j = j + 1;
          end
        end
      end
    endmodule
    "#;
    for four_state in [false, true] {
        let mut sim =
            Simulation::from_sv_sources(vec![(source, Path::new("bit_gather.sv"))], "Top")
                .four_state(four_state)
                .build()
                .unwrap();
        let (rs1, rs2, d) = (sim.signal("rs1"), sim.signal("rs2"), sim.signal("d"));
        for (value, mask, expected) in [
            (0xf0f0_a5a5u32, 0x0000_ff0fu32, 0x0000_0a55u32),
            (0xf0f0_a5a5, 0xffff_ffff, 0xf0f0_a5a5),
            (0xf0f0_a5a5, 0x8000_0001, 0x0000_0003),
            (0xf0f0_a5a5, 0, 0),
        ] {
            sim.modify(|io| {
                io.set(rs1, value);
                io.set(rs2, mask);
            })
            .unwrap();
            assert_eq!(
                u32::try_from(sim.get(d)).unwrap(),
                expected,
                "four-state {four_state}, rs2={mask:#x}"
            );
        }
    }
}
