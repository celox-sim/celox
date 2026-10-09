#![cfg(feature = "systemverilog")]
//! Packages resolved as scopes (IEEE 1800-2023 26).

use celox::{ParserError, Simulator, SimulatorErrorKind};

fn output(source: &str) -> u64 {
    outputs(source, &["y"])[0]
}

fn outputs(source: &str, names: &[&str]) -> Vec<u64> {
    let mut simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("packages.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
    names
        .iter()
        .map(|name| {
            let signal = simulator.signal(name);
            u64::try_from(simulator.get(signal)).unwrap()
        })
        .collect()
}

fn error(source: &str) -> String {
    let error =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("packages.sv"))], "Top")
            .build()
            .expect_err("the design must be rejected");
    match error.kind() {
        SimulatorErrorKind::SIRParser(ParserError::IllegalContext { detail, .. }) => detail.clone(),
        other => panic!("expected an illegal context, got {other:?}"),
    }
}

#[test]
fn a_local_declaration_hides_a_wildcard_import() {
    assert_eq!(
        output(
            "package p; localparam int W = 3; endpackage
             module Top(output logic [7:0] y); import p::*; localparam int W = 5; assign y = W; endmodule"
        ),
        5
    );
}

#[test]
fn packages_may_declare_the_same_name() {
    assert_eq!(
        output(
            "package p; localparam int W = 3; endpackage
             package q; localparam int W = 5; endpackage
             module Top(output logic [7:0] y); assign y = p::W + q::W; endmodule"
        ),
        8
    );
}

#[test]
fn an_explicit_import_imports_only_its_name() {
    assert_eq!(
        output(
            "package p; localparam int A = 3; localparam int B = 7; endpackage
             module Top(output logic [7:0] y); import p::A; localparam int B = 1; assign y = A + B; endmodule"
        ),
        4
    );
}

#[test]
fn a_qualified_name_does_not_clash_with_a_local_one() {
    assert_eq!(
        output(
            "package p; localparam int W = 3; endpackage
             module Top(output logic [7:0] y); localparam int W = 5; assign y = p::W + W; endmodule"
        ),
        8
    );
}

#[test]
fn package_functions_call_the_package_items() {
    assert_eq!(
        output(
            "package p;
               function automatic int helper(int x); return x + 1; endfunction
               function automatic int f(int x); return helper(x); endfunction
             endpackage
             module Top(output logic [7:0] y);
               function automatic int helper(int x); return x + 100; endfunction
               assign y = 8'(p::f(1) + helper(0));
             endmodule"
        ),
        102
    );
}

#[test]
fn an_unreferenced_name_of_two_wildcard_imports_is_not_ambiguous() {
    assert_eq!(
        output(
            "package p; localparam int T = 1; localparam int A = 2; endpackage
             package q; localparam int T = 3; localparam int B = 4; endpackage
             module Top(output logic [7:0] y); import p::*; import q::*; assign y = A + B; endmodule"
        ),
        6
    );
}

#[test]
fn packages_provide_types_enums_and_their_dependencies() {
    assert_eq!(
        output(
            "package base; localparam int W = 4; typedef logic [W-1:0] word_t; endpackage
             package ext;
               import base::*;
               typedef enum logic [1:0] { IDLE, RUN, DONE } state_t;
               localparam word_t ONES = '1;
               function automatic word_t twice(word_t x); return x << 1; endfunction
             endpackage
             module Top import ext::*; (output logic [7:0] y);
               ext::state_t s;
               base::word_t w;
               assign s = DONE;
               assign w = twice(4'd3);
               assign y = {2'(s), w[3:0], 2'(ONES)};
             endmodule"
        ),
        (2 << 6) | (6 << 2) | 3
    );
}

#[test]
fn parameter_overrides_take_qualified_enum_members() {
    assert_eq!(
        output(
            "package p; typedef enum logic [1:0] { A, B, C } kind_t; endpackage
             module Child import p::*; #(parameter kind_t K = A) (output logic [7:0] y);
               assign y = 8'(K);
             endmodule
             module Top(output logic [7:0] y); Child #(.K(p::C)) c (.y(y)); endmodule"
        ),
        2
    );
}

#[test]
fn imports_bind_array_parameters_and_constant_variables() {
    assert_eq!(
        output(
            "package p;
               localparam logic [7:0] A [2] = '{8'd3, 8'd5};
               const int K = 7;
             endpackage
             module Top(output logic [7:0] y);
               import p::*;
               assign y = A[1] + p::A[0] + 8'(K) + 8'(p::K);
             endmodule"
        ),
        5 + 3 + 7 + 7
    );
}

#[test]
fn names_that_are_not_references_do_not_bind_imports() {
    // `A` is only a port name and a structure member here; neither looks
    // up the ambiguous wildcard-imported names.
    assert_eq!(
        output(
            "package p; localparam int A = 1; localparam int W = 4; endpackage
             package q; localparam int A = 2; endpackage
             module Child(input logic [7:0] A, output logic [7:0] y); assign y = A; endmodule
             module Top(output logic [7:0] y);
               import p::*; import q::*;
               typedef struct packed { logic [3:0] W; logic [3:0] A; } pair_t;
               pair_t pair;
               assign pair = '{W: 4'(W), A: 4'd6};
               Child c(.A(8'(pair.A) + 8'(pair.W)), .y(y));
             endmodule"
        ),
        10
    );
}

#[test]
fn a_generate_block_function_shadows_an_imported_one_only_inside() {
    assert_eq!(
        outputs(
            "package p; function automatic int f(); return 1; endfunction endpackage
             module Top(output logic [7:0] y, output logic [7:0] z);
               import p::*;
               assign y = 8'(f());
               if (1) begin : g
                 function automatic int f(); return 2; endfunction
                 assign z = 8'(f());
               end
             endmodule",
            &["y", "z"]
        ),
        [1, 2]
    );
}

#[test]
fn qualified_references_name_declared_items() {
    let detail = error(
        "package p; localparam int W = 3; endpackage
         module Top(output logic [7:0] y); assign y = p::Missing; endmodule",
    );
    assert!(detail.contains("no item `Missing`"), "{detail}");
    let detail = error("module Top(output logic [7:0] y); assign y = q::W; endmodule");
    assert!(detail.contains("unknown package `q`"), "{detail}");
}

#[test]
fn a_package_names_its_own_items_through_its_scope() {
    assert_eq!(
        output(
            "package p;
               localparam int A = 3;
               localparam int B = p::A + 1;
               typedef logic [p::B-1:0] word_t;
               function automatic p::word_t twice(p::word_t x); return x << 1; endfunction
             endpackage
             module Top(output logic [7:0] y); assign y = 8'(p::twice(4'(p::B))) + 8'($bits(p::word_t)); endmodule"
        ),
        8 + 4
    );
    let detail = error(
        "package p; localparam int A = p::Missing; endpackage
         module Top(output logic [7:0] y); assign y = 8'(p::A); endmodule",
    );
    assert!(detail.contains("no item `Missing`"), "{detail}");
}

#[test]
fn qualified_types_formals_and_escaped_names_resolve() {
    assert_eq!(
        output(
            "package a; typedef logic [3:0] T; localparam int X = 5;
               task automatic t(input int X, output int r); r = X + 1; endtask
               localparam int \\c::d = 4;
             endpackage
             package b; typedef logic [7:0] T; endpackage
             module Top(output logic [7:0] y);
               import a::*; import b::*;
               a::T v;
               int r;
               always_comb a::t(.X(1), .r(r));
               assign v = 4'(r);
               assign y = 8'(v) + 8'(a::\\c::d );
             endmodule"
        ),
        2 + 4
    );
}

#[test]
fn package_exports_are_unsupported() {
    let error = Simulator::from_sv_sources(
        vec![(
            "package base; localparam int X = 1; endpackage
             package ext; import base::*; export base::X; endpackage
             module Top(output logic [7:0] y); assign y = 0; endmodule",
            std::path::Path::new("packages.sv"),
        )],
        "Top",
    )
    .build()
    .expect_err("package exports are not supported");
    match error.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Unsupported { detail, .. }) => {
            assert!(detail.contains("package export declaration"), "{detail}")
        }
        other => panic!("expected an unsupported construct, got {other:?}"),
    }
}

#[test]
fn self_qualified_names_are_not_hidden_by_formals() {
    assert_eq!(
        output(
            "package p;
               localparam int X = 5;
               function automatic int f(input int X); return p::X * 10 + X; endfunction
             endpackage
             module Top(output logic [7:0] y); assign y = 8'(p::f(1)); endmodule"
        ),
        51
    );
}

#[test]
fn selects_of_package_parameters_use_their_declared_ranges() {
    assert_eq!(
        output(
            "package p; localparam logic [7:4] V = 4'b0001; endpackage
             module Top(output logic [7:0] y); assign y = {7'd0, p::V[4]}; endmodule"
        ),
        1
    );
}

#[test]
fn declarations_do_not_look_up_wildcard_imports() {
    assert_eq!(
        output(
            "package p; localparam int Top = 1; localparam int g = 1; endpackage
             package q; localparam int Top = 2; localparam int g = 2; endpackage
             module Top(output logic [7:0] y);
               import p::*; import q::*;
               if (1) begin : g
                 assign y = 8'd7;
               end
             endmodule"
        ),
        7
    );
}

#[test]
fn ambiguous_wildcard_references_are_errors() {
    let detail = error(
        "package p; localparam int T = 1; endpackage
         package q; localparam int T = 3; endpackage
         module Top(output logic [7:0] y); import p::*; import q::*; assign y = T; endmodule",
    );
    assert!(detail.contains("`T`"), "{detail}");
}

#[test]
fn an_explicit_import_of_a_local_name_is_an_error() {
    let detail = error(
        "package p; localparam int W = 3; endpackage
         module Top(output logic [7:0] y); import p::W; localparam int W = 5; assign y = W; endmodule",
    );
    assert!(detail.contains("`W`"), "{detail}");
}

#[test]
fn unknown_packages_and_items_are_errors() {
    let detail = error(
        "package p; localparam int W = 3; endpackage
         module Top(output logic [7:0] y); import p::X; assign y = 0; endmodule",
    );
    assert!(detail.contains("no item `X`"), "{detail}");
    let detail = error("module Top(output logic [7:0] y); import q::*; assign y = 0; endmodule");
    assert!(detail.contains("unknown package `q`"), "{detail}");
}

/// A package variable is one object shared by every module. Until it can be
/// shared, it is rejected (#1146).
#[test]
fn rejects_variables_shared_through_a_package() {
    let source = r#"
        package p; logic [7:0] shared; endpackage
        module W(input logic [7:0] a); assign p::shared = a; endmodule
        module Top(output logic [7:0] y);
            W w(.a(8'd9));
            assign y = p::shared;
        endmodule
    "#;
    let err = Simulator::from_sv_sources(vec![(source, std::path::Path::new("p.sv"))], "Top")
        .build()
        .expect_err("a package variable must be rejected");
    match err.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Unsupported {
            issue: 1146,
            detail,
            ..
        }) => {
            assert!(detail.contains("`p::shared`"), "{detail}")
        }
        other => panic!("expected the package variable to be unsupported, got: {other:?}"),
    }
}
