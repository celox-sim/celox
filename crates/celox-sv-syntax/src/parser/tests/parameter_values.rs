//! Parameter declarations without a value (IEEE 1800-2023 6.20.1).
use super::*;

#[test]
fn only_parameter_port_lists_omit_values() {
    for (code, name) in [
        ("module a; parameter A; endmodule", "A"),
        ("module a; localparam int B; endmodule", "B"),
        ("module a; parameter type T; endmodule", "T"),
        ("module a; initial begin parameter P; end endmodule", "P"),
    ] {
        let error = Declarations::parse(code, Path::new("parameters.sv"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!(
                "parameter `{name}` declared without a value outside a parameter port list"
            )),
            "{code}: {error}"
        );
    }
    for code in [
        "module a #(parameter A, parameter type T); endmodule",
        "module a #(parameter A = 1); parameter B = 2; localparam C = B; endmodule",
        "class C #(int P); endclass",
    ] {
        Declarations::parse(code, Path::new("parameters.sv"))
            .unwrap_or_else(|error| panic!("{code}: {error}"));
    }
}
