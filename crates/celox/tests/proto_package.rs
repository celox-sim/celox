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
        @ignore_on(sv);
        @case "proto_package::test_proto_package_builtin_comparison";
    }

    // Positive control: proto package with an explicit `lt` function works correctly.
    // This confirms that proto package function dispatch is functional.
    fn test_proto_package_with_custom_function(sim) {
        @ignore_on(sv);
        @case "proto_package::test_proto_package_with_custom_function";
    }
}
