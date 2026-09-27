use crate::Design;

cases! { StandardLibrary, "fifo_issue5";


    fn test_fifo_issue5_subtract_overflow(sim) {

        @setup { let ram = super::std_source(&["ram", "ram.veryl"]);
let fifo_ctrl =
super::std_source(&["fifo", "fifo_controller.veryl"]);
let fifo = super::std_source(&["fifo", "fifo.veryl"]);
let top = r#"
module Top (
i_clk: input clock,
i_rst: input reset,
i_push: input logic,
i_data: input logic<8>,
i_pop: input logic,
o_data: output logic<8>,
o_empty: output logic,
o_full: output logic,
) {
var almost_full: logic;
var word_count: logic<$clog2(16 + 1)>;
var clear: logic;
always_comb { clear = '0; }
inst u_fifo: fifo #(
WIDTH: 8,
DEPTH: 16,
) (
i_clk,
i_rst,
i_clear: clear,
o_empty,
o_almost_full: almost_full,
o_full,
o_word_count: word_count,
i_push,
i_data,
i_pop,
o_data,
);
}
"#;
let code = format!("{}\n{}\n{}\n{}", ram, fifo_ctrl, fifo, top); }
        @build Design::new(&code, "Top");

    }
}
