//! Items that the grammar does not admit where they are written.
use super::*;

fn parse_error(code: &str) -> String {
    Declarations::parse(code, Path::new("items.sv"))
        .unwrap_err()
        .to_string()
}

fn accepts(code: &str) {
    Declarations::parse(code, Path::new("items.sv"))
        .unwrap_or_else(|error| panic!("{code}: {error}"));
}

/// A for-loop variable declaration names its data type, also after `var`
/// (IEEE 1800-2023 12.7.1, Syntax 12-5).
#[test]
fn for_loop_variables_need_a_data_type() {
    let error = parse_error(
        "module test; initial begin for (var [7:0] i = 0; i < 10; i++) begin end end endmodule",
    );
    assert!(
        error.contains("for-loop variable declaration without a data type"),
        "{error}"
    );
    accepts(
        "module test; initial begin for (var logic [7:0] i = 0; i < 10; i++) begin end end endmodule",
    );
    accepts(
        "module test; initial begin for (int i = 0, j = 1; i < 10; i++) begin end end endmodule",
    );
}

/// Generate regions do not nest (IEEE 1800-2023 27.3).
#[test]
fn generate_regions_do_not_nest() {
    let error = parse_error(
        "module top; genvar i; wire y; generate for (i = 0; i < 1; i = i + 1) begin end \
         generate for (i = 0; i < 1; i = i + 1) begin end endgenerate endgenerate endmodule",
    );
    assert!(
        error.contains("generate region nested in a generate region"),
        "{error}"
    );
    accepts(
        "module top; genvar i; generate for (i = 0; i < 1; i = i + 1) begin end endgenerate \
         generate if (1) begin end endgenerate endmodule",
    );
}

/// A generate block holds generate items, which a specparam declaration is
/// not (IEEE 1800-2023 27.3).
#[test]
fn generate_blocks_hold_no_specparams() {
    let error = parse_error(
        "module test #(parameter A = 1); generate if (A) begin specparam x = 10; end endgenerate endmodule",
    );
    assert!(
        error.contains("specparam declaration in a generate block"),
        "{error}"
    );
    accepts("module test; specparam x = 10; endmodule");
    accepts("module test; generate specparam x = 10; endgenerate endmodule");
}
