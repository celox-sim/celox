use celox::Simulator;

const SELECTOR_PKG_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/selector/selector_pkg.veryl");
const MUX_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/selector/mux.veryl");
const DEMUX_SRC: &str =
    include_str!("../../../deps/veryl/crates/std/veryl/src/selector/demux.veryl");

/// Build-only smoke test: mux with selector_pkg requires `calc_select_width` function
/// evaluation at compile time. Currently Celox cannot resolve this, so this test is ignored.
#[test]
#[ignore = "Celox does not yet support const function evaluation in module params (calc_select_width)"]
fn test_mux_build_smoke() {
    let top = r#"
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
        KIND   : selector_kind::BINARY,
    ) (
        i_select,
        i_data: data_arr,
        o_data,
    );
}
"#;
    let code = format!("{SELECTOR_PKG_SRC}\n{MUX_SRC}\n{top}");
    let _sim = Simulator::builder(&code, "Top").build().unwrap();
}

/// Build-only: binary demux
#[test]
#[ignore = "Celox does not yet support const function evaluation in module params (calc_select_width)"]
fn test_demux_build_smoke() {
    let top = r#"
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
        KIND   : selector_kind::BINARY,
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
    let code = format!("{SELECTOR_PKG_SRC}\n{DEMUX_SRC}\n{top}");
    let _sim = Simulator::builder(&code, "Top").build().unwrap();
}
