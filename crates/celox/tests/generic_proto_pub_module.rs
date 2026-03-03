/// Correctness test for the `pub module` + generic module + `<:` pattern (issue #11).
///
/// A `pub module` wrapper over a generic module that uses `<:` on `PKG::Item` variables
/// must compile and simulate correctly. The veryl analyzer emits a spurious
/// `UnknownMember { member: "lt" }` error during `analyze_post_pass1` for this pattern,
/// but it is harmless — the IR uses `Op::Less` directly.
///
/// Note: `Simulator::builder` already discards non-`UnsupportedByIr` analyzer errors, so
/// this test exercises simulation correctness. The `gen_ts` NAPI path (Vite plugin) is
/// where the actual bug manifested; it required the same `UnknownMember` filter.
use celox::Simulator;

const CODE: &str = r#"
proto package ItemProto {
    type Item;
    // no lt() — comparison is done with <: directly
}

package ItemU16 for ItemProto {
    type Item = logic<16>;
}

// Generic module: uses <: on PKG::Item, never calls PKG::lt
module Sorter::<PKG: ItemProto> (
    clk  : input  clock    ,
    rst  : input  reset    ,
    d_in : input  PKG::Item,
    d_out: output PKG::Item,
) {
    var r: PKG::Item;
    always_ff (clk, rst) {
        if_reset { r = -1; }
        else if d_in <: r { r = d_in; }
    }
    assign d_out = r;
}

// pub module wrapper that fixes the generic parameter
pub module SorterU16 (
    clk  : input  clock    ,
    rst  : input  reset    ,
    d_in : input  logic<16>,
    d_out: output logic<16>,
) {
    inst s: Sorter::<ItemU16> (clk, rst, d_in, d_out);
}
"#;

#[test]
fn test_pub_module_wrapping_generic_proto_module() {
    let mut sim = Simulator::builder(CODE, "SorterU16")
        .build()
        .expect("SorterU16 should compile without error");

    let clk = sim.event("clk");
    let rst = sim.signal("rst");
    let d_in = sim.signal("d_in");
    let d_out = sim.signal("d_out");

    // Reset phase: rst=0 (active low async reset)
    sim.modify(|io| {
        io.set(rst, 0u8);
        io.set(d_in, 0u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(d_out), u16::MAX.into(), "after reset, d_out should be 0xFFFF");

    // Insert value 100
    sim.modify(|io| {
        io.set(rst, 1u8);
        io.set(d_in, 100u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(d_out), 100u16.into(), "after inserting 100, d_out should be 100");

    // Insert value 50 (smaller — should replace 100)
    sim.modify(|io| {
        io.set(d_in, 50u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(d_out), 50u16.into(), "after inserting 50, d_out should be 50");

    // Insert value 200 (larger — should not replace 50)
    sim.modify(|io| {
        io.set(d_in, 200u16);
    })
    .unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get(d_out), 50u16.into(), "after inserting 200, d_out should remain 50");
}
