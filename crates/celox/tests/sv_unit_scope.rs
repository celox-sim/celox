#![cfg(feature = "systemverilog")]
//! The compilation-unit scope `$unit` (IEEE 1800-2023 3.12.1): each source
//! file is a compilation unit of its own.

use celox::{ParserError, Simulator, SimulatorErrorKind};

fn build(sources: &[(&str, &str)]) -> Result<Simulator, celox::SimulatorError> {
    Simulator::from_sv_sources(
        sources
            .iter()
            .map(|(code, path)| (*code, std::path::Path::new(*path)))
            .collect(),
        "Top",
    )
    .build()
}

fn outputs(source: &str, names: &[&str]) -> Vec<u64> {
    let mut simulator = build(&[(source, "unit.sv")]).unwrap_or_else(|error| panic!("{error}"));
    names
        .iter()
        .map(|name| {
            let signal = simulator.signal(name);
            u64::try_from(simulator.get(signal)).unwrap()
        })
        .collect()
}

fn output(source: &str) -> u64 {
    outputs(source, &["y"])[0]
}

fn error(sources: &[(&str, &str)]) -> String {
    let error = build(sources).expect_err("the design must be rejected");
    match error.kind() {
        SimulatorErrorKind::SIRParser(ParserError::IllegalContext { detail, .. }) => detail.clone(),
        other => panic!("expected an illegal context, got {other:?}"),
    }
}

#[test]
fn modules_see_compilation_unit_items() {
    assert_eq!(
        output(
            "localparam int W = 5;
             typedef logic [3:0] nib_t;
             typedef enum logic [1:0] {A, B, C} e_t;
             module Top(output logic [7:0] y);
               nib_t n; e_t e;
               assign n = 4'h9; assign e = C;
               assign y = W * 10 + n + e + f(1);
             endmodule
             // A subroutine is visible before its declaration.
             function automatic int f(input int a); return a * 100; endfunction"
        ),
        50 + 9 + 2 + 100
    );
}

#[test]
fn compilation_unit_imports_apply_to_later_modules() {
    assert_eq!(
        output(
            "package p; localparam int W = 7; localparam int V = 1; endpackage
             import p::*;
             import p::V;
             module Top(output logic [7:0] y); assign y = W + V; endmodule"
        ),
        8
    );
}

#[test]
fn module_scopes_hide_compilation_unit_items() {
    // A local declaration, and a wildcard import of the module, come first;
    // `$unit::` names the compilation-unit item.
    assert_eq!(
        outputs(
            "package p; localparam int W = 3; endpackage
             localparam int W = 5;
             module M(output logic [7:0] v); import p::*; assign v = W; endmodule
             module Top(output logic [7:0] y, output logic [7:0] z, output logic [7:0] u);
               localparam int W = 6;
               assign y = W; assign z = $unit::W;
               M m(.v(u));
             endmodule",
            &["y", "z", "u"]
        ),
        [6, 5, 3]
    );
}

#[test]
fn compilation_unit_variables_are_shared() {
    assert_eq!(
        output(
            "logic [7:0] shared;
             module W(input logic [7:0] a); always_comb shared = a + 8'd1; endmodule
             module R(output logic [7:0] b); assign b = $unit::shared; endmodule
             module Top(output logic [7:0] y); W w(.a(8'd9)); R r(.b(y)); endmodule"
        ),
        10
    );
}

#[test]
fn later_compilation_unit_items_are_not_visible() {
    let detail = error(&[(
        "module Top(output logic [7:0] y); assign y = $unit::W; endmodule
         localparam int W = 5;",
        "unit.sv",
    )]);
    assert!(detail.contains("no item `W`"), "{detail}");
}

#[test]
fn each_file_is_a_compilation_unit() {
    let result = build(&[
        ("localparam int W = 5;", "a.sv"),
        (
            "module Top(output logic [7:0] y); assign y = $unit::W; endmodule",
            "b.sv",
        ),
    ]);
    assert!(
        result.is_err(),
        "`W` belongs to the compilation unit of a.sv"
    );
}

/// The example of IEEE 1800-2023 3.12.1: `$unit::b` is not hidden by a
/// local `b` of a subroutine of the unit.
#[test]
fn unit_subroutines_name_unit_items_through_unit_scope() {
    assert_eq!(
        output(
            "localparam int b = 2;
             function automatic int t(input int a); int b; b = a + $unit::b; return b; endfunction
             module Top(output logic [7:0] y); assign y = t(5); endmodule"
        ),
        7
    );
}

#[test]
fn compilation_unit_variables_of_several_files_are_unsupported() {
    let error = build(&[
        ("logic [7:0] g;", "a.sv"),
        (
            "logic [7:0] h; module Top(output logic [7:0] y); assign y = 0; endmodule",
            "b.sv",
        ),
    ])
    .expect_err("compilation-unit variables of several files are unsupported");
    assert!(
        error
            .to_string()
            .contains("compilation-unit variables in more than one source file"),
        "{error}"
    );
}

/// An item of the unit no module uses is not analyzed for them.
#[test]
fn unused_unsupported_unit_items_are_ignored() {
    assert_eq!(
        output(
            "typedef union { int i; bit b; } bint;
             localparam int W = 4;
             module Top(output logic [7:0] y); assign y = 8'd3; endmodule"
        ),
        3
    );
    // A module that uses the unit reports why it cannot be analyzed.
    let error = build(&[(
        "typedef union { int i; bit b; } bint;
         localparam int W = 4;
         module Top(output logic [7:0] y); assign y = W; endmodule",
        "unit.sv",
    )])
    .expect_err("the unit cannot be analyzed");
    assert!(error.to_string().contains("union"), "{error}");
}

/// Review cases: conflicting unit imports, imports of unit subroutines, and
/// sources that share a path.
#[test]
fn unit_imports_conflict_and_stay_in_their_scope() {
    let detail = error(&[(
        "package p; localparam int X = 3; endpackage
         package q; localparam int X = 5; endpackage
         import p::X; import q::X;
         module Top(output logic [7:0] y); assign y = X; endmodule",
        "unit.sv",
    )]);
    assert!(detail.contains("`X`"), "{detail}");
    // An import in a subroutine of the unit is not one of the unit, so `X`
    // is unknown to the module.
    assert!(
        build(&[(
            "package p; localparam int X = 3; endpackage
             function automatic int f(); import p::*; return X; endfunction
             module Top(output logic [7:0] y); assign y = X; endmodule",
            "unit.sv",
        )])
        .is_err()
    );
    let error = build(&[
        ("localparam int W = 1;", "same.sv"),
        (
            "localparam int W = 2; module Top(output logic [7:0] y); assign y = W; endmodule",
            "same.sv",
        ),
    ])
    .expect_err("units of sources with one path are rejected");
    assert!(error.to_string().contains("another source"), "{error}");
    // Also when only one of them declares unit items.
    let error = build(&[
        ("localparam int W = 7;", "same.sv"),
        (
            "module Top(output logic [7:0] y); assign y = 0; endmodule",
            "same.sv",
        ),
    ])
    .expect_err("units of sources with one path are rejected");
    assert!(error.to_string().contains("another source"), "{error}");
    // An import in a subroutine of the unit is not one of its siblings'.
    assert!(
        build(&[(
            "package p; localparam int X = 3; endpackage
             function automatic int f(); import p::X; return X; endfunction
             localparam int Y = X;
             module Top(output logic [7:0] y); assign y = Y; endmodule",
            "unit.sv",
        )])
        .is_err()
    );
}

/// Review cases: unit import conflicts without a reference, `$unit::` in a
/// parameter override, and interfaces that name unit items.
#[test]
fn unit_conflicts_overrides_and_interfaces() {
    let detail = error(&[(
        "package p; localparam int X = 3; endpackage
         package q; localparam int X = 5; endpackage
         import p::X; import q::X;
         module Top(output logic [7:0] y); assign y = 0; endmodule",
        "unit.sv",
    )]);
    assert!(detail.contains("`X`"), "{detail}");
    assert_eq!(
        output(
            "localparam int W = 6;
             module Child #(parameter int N = 1) (output logic [7:0] v); assign v = N; endmodule
             module Top(output logic [7:0] y); Child #(.N($unit::W)) c(.v(y)); endmodule"
        ),
        6
    );
    let error = build(&[
        (
            "localparam int W = 2;
             interface I; logic [$unit::W-1:0] v; endinterface",
            "a.sv",
        ),
        (
            "module Top(output logic [7:0] y); I i(); assign i.v = 1; assign y = i.v; endmodule",
            "b.sv",
        ),
    ])
    .expect_err("the interface names an item of another file's unit");
    assert!(
        error.to_string().contains("compilation-unit item"),
        "{error}"
    );
}
