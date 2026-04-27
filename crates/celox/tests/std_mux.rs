use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

const SELECTOR_PKG_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/selector/selector_pkg.veryl");
const MUX_SRC: &str = include_str!("../../../deps/veryl/crates/std/veryl/src/selector/mux.veryl");
const DEMUX_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/selector/demux.veryl");

all_backends! {

    // Build-only smoke test: mux with selector_pkg requires `calc_select_width`
    // evaluation while resolving module parameters and widths.
    fn test_mux_build_smoke(sim) {
        @ignore_on(veryl);
        @setup { let top = r#"
module Top (
i_select: input  logic<2>,
i_data0 : input  logic<8>,
i_data1 : input  logic<8>,
i_data2 : input  logic<8>,
i_data3 : input  logic<8>,
o_data  : output logic<8>,
) {
var data_arr: logic<8>[4];
always_comb {
data_arr[0] = i_data0;
data_arr[1] = i_data1;
data_arr[2] = i_data2;
data_arr[3] = i_data3;
}
inst u: mux #(
WIDTH  : 8,
ENTRIES: 4,
KIND   : selector_pkg::selector_kind::BINARY,
) (
i_select,
i_data: data_arr,
o_data,
);
}
"#;
let code = format!("{SELECTOR_PKG_SRC}\n{MUX_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");

    }

    // Build-only smoke test: binary demux also depends on selector_pkg compile-time
    // width resolution through `calc_select_width`.
    fn test_demux_build_smoke(sim) {
        @ignore_on(veryl);
        @setup { let top = r#"
module Top (
i_select: input  logic<2>,
i_data  : input  logic<8>,
o_data0 : output logic<8>,
o_data1 : output logic<8>,
o_data2 : output logic<8>,
o_data3 : output logic<8>,
) {
var data_arr: logic<8>[4];
inst u: demux #(
WIDTH  : 8,
ENTRIES: 4,
KIND   : selector_pkg::selector_kind::BINARY,
) (
i_select,
i_data,
o_data: data_arr,
);
always_comb {
o_data0 = data_arr[0];
o_data1 = data_arr[1];
o_data2 = data_arr[2];
o_data3 = data_arr[3];
}
}
"#;
let code = format!("{SELECTOR_PKG_SRC}\n{DEMUX_SRC}\n{top}"); }
        @build Simulator::builder(&code, "Top");

    }
}
