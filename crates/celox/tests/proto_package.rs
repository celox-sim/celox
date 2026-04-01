use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    // Issue #11: `pub module` wrapper causes "PKG doesn't have member lt"
    // when proto package has no `lt` — the `<:` operator is a builtin comparison,
    // not a proto package function call.
    //
    // Uses a combinational circuit to test that `<:` on PKG::Item resolves
    // correctly to the builtin less-than operator.
    fn test_proto_package_builtin_comparison(sim) {
        @setup { let code = r#"
proto package ItemProto {
type Item;
}
package ItemU16 for ItemProto {
type Item = logic<16>;
}
module Comparator::<PKG: ItemProto> (
a    : input  PKG::Item,
b    : input  PKG::Item,
a_lt : output logic    ,
) {
assign a_lt = a <: b;
}
pub module ComparatorU16 (
a    : input  logic<16>,
b    : input  logic<16>,
a_lt : output logic    ,
) {
inst c: Comparator::<ItemU16> (a, b, a_lt);
}
"#; }
        @build Simulator::builder(code, "ComparatorU16");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let a_lt = sim.signal("a_lt");

    // 10 < 20 → true
    sim.modify(|io| {
        io.set(a, 10u16);
        io.set(b, 20u16);
    })
    .unwrap();
    assert_eq!(sim.get(a_lt), 1u8.into());

    // 20 < 10 → false
    sim.modify(|io| {
        io.set(a, 20u16);
        io.set(b, 10u16);
    })
    .unwrap();
    assert_eq!(sim.get(a_lt), 0u8.into());

    // 10 < 10 → false (strict less-than)
    sim.modify(|io| {
        io.set(a, 10u16);
        io.set(b, 10u16);
    })
    .unwrap();
    assert_eq!(sim.get(a_lt), 0u8.into());

    }

    // Positive control: proto package with an explicit `lt` function works correctly.
    // This confirms that proto package function dispatch is functional.
    fn test_proto_package_with_custom_function(sim) {
        @setup { let code = r#"
proto package ItemProto {
type Item;
function lt(
a: input Item,
b: input Item,
) -> logic;
}
package ItemU16 for ItemProto {
type Item = logic<16>;
function lt(
a: input Item,
b: input Item,
) -> logic {
return a <: b;
}
}
module Comparator::<PKG: ItemProto> (
a    : input  PKG::Item,
b    : input  PKG::Item,
a_lt : output logic    ,
) {
assign a_lt = PKG::lt(a, b);
}
pub module ComparatorU16 (
a    : input  logic<16>,
b    : input  logic<16>,
a_lt : output logic    ,
) {
inst c: Comparator::<ItemU16> (a, b, a_lt);
}
"#; }
        @build Simulator::builder(code, "ComparatorU16");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let a_lt = sim.signal("a_lt");

    // 10 < 20 → true
    sim.modify(|io| {
        io.set(a, 10u16);
        io.set(b, 20u16);
    })
    .unwrap();
    assert_eq!(sim.get(a_lt), 1u8.into());

    // 20 < 10 → false
    sim.modify(|io| {
        io.set(a, 20u16);
        io.set(b, 10u16);
    })
    .unwrap();
    assert_eq!(sim.get(a_lt), 0u8.into());

    // 10 < 10 → false
    sim.modify(|io| {
        io.set(a, 10u16);
        io.set(b, 10u16);
    })
    .unwrap();
    assert_eq!(sim.get(a_lt), 0u8.into());

    }
}
