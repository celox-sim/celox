#![cfg(feature = "emit")]

use celox_test_suite_veryl::emit::emit_veryl_sources;
use std::path::Path;

#[test]
fn event_polarities_follow_top_port_types_even_when_forwarded() {
    let source = r#"
module Child (
    i_clk: input clock_negedge,
    i_rst: input reset_async_high,
    d: input logic,
    q: output logic,
) {
    always_ff (i_clk, i_rst) {
        if_reset { q = 0; }
        else { q = d; }
    }
}
module Top (
    clock_in: input clock_negedge,
    reset_in: input reset_async_high,
    d: input logic,
    q: output logic,
) {
    inst child: Child (i_clk: clock_in, i_rst: reset_in, d, q);
}
"#;
    let emitted = emit_veryl_sources(&[(source, Path::new("forwarded.veryl"))]);
    let top = emitted.event_edges("Top").unwrap();
    assert_eq!(top.get("clock_in"), Some(&false));
    assert_eq!(top.get("reset_in"), Some(&true));
    assert!(!top.contains_key("i_clk"));
    assert!(!top.contains_key("d"));
    assert!(emitted.event_edges("Missing").is_none());
    let sv = emitted.as_sv_sources();
    assert!(sv[0].0.contains("negedge i_clk"));
    assert!(sv[0].0.contains("posedge i_rst"));
}
