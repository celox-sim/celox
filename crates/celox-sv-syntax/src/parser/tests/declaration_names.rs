//! Declarations the grammar admits whose names, lifetimes or returns IEEE
//! 1800-2023 forbids.
use super::*;

fn parse_error(code: &str) -> String {
    Declarations::parse(code, Path::new("declarations.sv"))
        .unwrap_err()
        .to_string()
}

fn accepts(code: &str) {
    Declarations::parse(code, Path::new("declarations.sv"))
        .unwrap_or_else(|error| panic!("{code}: {error}"));
}

#[test]
fn nested_loop_generates_use_distinct_genvars() {
    let error = parse_error(
        "module top; genvar i; for (i = 100; i < 101; i = i + 1) \
         for (i = 100; i < 101; i = i + 1) initial $display(i); endmodule",
    );
    assert!(
        error.contains("loop generate nested in a loop generate of the same genvar `i`"),
        "{error}"
    );
    let error = parse_error(
        "module top; genvar i; for (i = 0; i < 2; i++) begin : a \
         for (i = 0; i < 2; i++) begin : b end end endmodule",
    );
    assert!(error.contains("of the same genvar `i`"), "{error}");
    accepts(
        "module top; genvar i, j; for (i = 0; i < 2; i++) begin : a \
         for (j = 0; j < 2; j++) begin : b end end \
         for (i = 0; i < 2; i++) begin : c end endmodule",
    );
}

#[test]
fn loop_generate_indices_are_genvars() {
    let error = parse_error(
        "module top; integer i = 0; generate for (i = 0; i < 4; i = i + 1) begin : U \
         reg [1:0] a = i; end endgenerate endmodule",
    );
    assert!(
        error.contains("loop generate index `i` is a variable, not a genvar"),
        "{error}"
    );
    accepts("module top; genvar i; for (i = 0; i < 4; i++) begin : U end endmodule");
    accepts("module top; for (genvar i = 0; i < 4; i++) begin : U end endmodule");
}

#[test]
fn generate_blocks_have_names_of_their_own() {
    for (code, name) in [
        (
            "module test; reg named; for (genvar gv = 0; gv < 1; gv++) begin : named end endmodule",
            "named",
        ),
        (
            "module test; for (genvar gv = 0; gv < 1; gv++) begin : match end \
             for (genvar gv = 0; gv < 1; gv++) begin : match end endmodule",
            "match",
        ),
        (
            "module m; localparam up = 1; if (up) begin : block1 wire w; end \
             initial begin : block1 end endmodule",
            "block1",
        ),
        (
            "module m; if (1) begin : g end if (1) begin : g end endmodule",
            "g",
        ),
        (
            "module m; wire g; case (1) 1: begin : g end endcase endmodule",
            "g",
        ),
        (
            "module m; sub g(); if (1) begin : g end endmodule module sub; endmodule",
            "g",
        ),
    ] {
        let error = parse_error(code);
        assert!(
            error.contains(&format!(
                "generate block `{name}` has the name of another declaration"
            )),
            "{code}: {error}"
        );
    }
    // The blocks of one conditional generate construct may share a name.
    accepts(
        "module m; parameter P = 1; if (P == 0) begin : g end else if (P == 1) begin : g end \
         else begin : g end case (P) 0: begin : h end default: begin : h end endcase endmodule",
    );
    accepts("module m; if (1) begin : g wire g; end initial begin : b end endmodule");
}

/// A class is a scope: one name declares one item of it (IEEE 1800-2023
/// 8.3, 6.19).
#[test]
fn class_members_have_names_of_their_own() {
    for (code, name) in [
        ("class C; enum { A = 10 } e; typedef int A; endclass", "A"),
        (
            "class C; int x; function void x(); endfunction endclass",
            "x",
        ),
        ("class C; int x; real x; endclass", "x"),
        (
            "class C #(parameter P = 1); localparam P = 2; endclass",
            "P",
        ),
        ("class C; typedef enum {X, Y} T; int Y; endclass", "Y"),
    ] {
        let error = parse_error(&format!("module test; {code} endmodule"));
        assert!(
            error.contains(&format!("`{name}` is declared more than once in class `C`")),
            "{code}: {error}"
        );
    }
    accepts(
        "module test; class C; typedef class D; typedef enum {X, Y} T; T a, b; \
         enum {Z} z; int y; function void f(); endfunction class D; int x; endclass endclass endmodule",
    );
}

/// The lifetime of a class method is automatic (IEEE 1800-2023 8.6).
#[test]
fn class_methods_have_no_static_lifetime() {
    for (code, method) in [
        ("class C; task static t(int x); endtask endclass", "t"),
        (
            "class C; function static int t(int x); return x; endfunction endclass",
            "t",
        ),
    ] {
        let error = parse_error(&format!("{code} module m; endmodule"));
        assert!(
            error.contains(&format!(
                "method `{method}` of class `C` declared with a static lifetime"
            )),
            "{code}: {error}"
        );
    }
    accepts(
        "class C; static function int f(); return 1; endfunction \
         task automatic t(); endtask endclass module m; endmodule",
    );
    accepts("module m; function static int f(); return 1; endfunction endmodule");
}

/// A task or a void function returns no value (IEEE 1800-2023 13.3,
/// 13.4.1).
#[test]
fn void_subroutines_return_no_value() {
    for (code, message) in [
        (
            "module main; function void bla(); return 10; endfunction endmodule",
            "return of a value from void function `bla`",
        ),
        (
            "module main; task t; if (1) begin return 1; end endtask endmodule",
            "return of a value from task `t`",
        ),
    ] {
        let error = parse_error(code);
        assert!(error.contains(message), "{code}: {error}");
    }
    accepts(
        "module main; function void f(); return; endfunction task t; return; endtask \
         function int g(); return 1; endfunction endmodule",
    );
}

/// Typedefs that name each other in a cycle resolve to no data type (IEEE
/// 1800-2023 6.18).
#[test]
fn typedefs_do_not_name_themselves() {
    for code in [
        "module test; typedef T1; typedef T1 T2; typedef T2 T1; T2 x; endmodule",
        "module test; typedef A A; endmodule",
        "package p; typedef C B; typedef B [3:0] C; endpackage",
    ] {
        let error = parse_error(code);
        assert!(
            error.contains("names itself through a cycle of typedefs"),
            "{code}: {error}"
        );
    }
    accepts("module test; typedef T1; typedef logic [3:0] T2; typedef T2 T1; T1 x; endmodule");
    accepts("package p; typedef int A; endpackage module test; typedef p::A A; endmodule");
}
