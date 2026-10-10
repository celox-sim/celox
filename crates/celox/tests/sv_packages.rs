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

/// An export makes imported declarations visible to the importers of the
/// package; an import through an export denotes the original declaration
/// (IEEE 1800-2023 26.6).
#[test]
fn packages_export_imported_names() {
    let p1 = "package p1; localparam int X = 3; localparam int Y = 4; endpackage";
    // `export p1::*` exports what the package imports from `p1`.
    assert_eq!(
        output(&format!(
            "{p1}
             package p2; import p1::X; export p1::*; endpackage
             module Top(output logic [7:0] y); import p2::*; assign y = X; endmodule"
        )),
        3
    );
    // Exports chain, and a declaration exported through two packages does
    // not make a wildcard reference ambiguous.
    assert_eq!(
        output(&format!(
            "{p1}
             package p2; import p1::X; export p1::*; endpackage
             package p3; import p1::*; import p2::*; export p2::*; localparam int Q = X; endpackage
             package p4; import p1::*; export p1::*; localparam int Z = X + 1; endpackage
             module Top(output logic [7:0] y); import p3::*; import p4::*;
               assign y = X * 10 + Q + Z; endmodule"
        )),
        30 + 3 + 4
    );
    // `export p::x` imports an unreferenced candidate; `export *::*` exports
    // every imported declaration.
    assert_eq!(
        output(&format!(
            "{p1}
             package p5; import p1::*; export p1::Y; endpackage
             package p6; export *::*; import p1::X; endpackage
             module Top(output logic [7:0] y); import p5::*; import p6::X;
               assign y = X * 10 + Y; endmodule"
        )),
        34
    );
    // An explicit import of an exported name, and of the same declaration
    // through two packages.
    assert_eq!(
        output(&format!(
            "{p1}
             package p2; import p1::X; export p1::X; endpackage
             module Top(output logic [7:0] y); import p2::X; import p1::X; assign y = X; endmodule"
        )),
        3
    );
}

#[test]
fn packages_export_only_imported_names() {
    let p1 = "package p1; localparam int X = 3; localparam int Y = 4; endpackage";
    // `Y` is a candidate for import into `p3` but not imported, so the
    // export does not make it available.
    let detail = error(&format!(
        "{p1}
         package p3; import p1::*; export p1::*; localparam int Q = X; endpackage
         module Top(output logic [7:0] y); import p3::Y; assign y = 0; endmodule"
    ));
    assert!(detail.contains("no item `Y`"), "{detail}");
    // A name a subroutine of the package declares is not imported by the
    // references to it there.
    let detail = error(&format!(
        "{p1}
         package p3; import p1::*; export p1::*;
           function automatic int f(input int X); return X; endfunction
         endpackage
         module Top(output logic [7:0] y); import p3::X; assign y = 0; endmodule"
    ));
    assert!(detail.contains("no item `X`"), "{detail}");
}

/// A qualified name may name an exported declaration: `p2::X` and `p1::X`
/// are the same declaration (IEEE 1800-2023 26.6).
#[test]
fn qualified_names_reach_exported_declarations() {
    assert_eq!(
        output(
            "package p1; localparam int X = 3; typedef logic [5:0] t; endpackage
             package p2; import p1::X; import p1::t; export p1::X, p1::t; endpackage
             module Top(output logic [7:0] y); p2::t v; assign v = '1; assign y = p2::X + v; endmodule"
        ),
        66
    );
}

/// A wildcard reference a function local shadows is not ambiguous, and a
/// named export binds its name before other references look it up.
#[test]
fn shadowed_and_exported_names_are_not_ambiguous() {
    assert_eq!(
        output(
            "package p; localparam int X = 3; endpackage
             package q; localparam int X = 5; endpackage
             module Top(output logic [7:0] y); import p::*; import q::*;
               function automatic int f(input int X); return X + 1; endfunction
               assign y = f(1); endmodule"
        ),
        2
    );
    assert_eq!(
        output(
            "package p; localparam int X = 3; endpackage
             package q; localparam int X = 5; endpackage
             package r; import p::*; import q::*; export p::X; localparam int Y = X; endpackage
             module Top(output logic [7:0] y); assign y = r::Y; endmodule"
        ),
        3
    );
}

#[test]
fn illegal_exports_are_errors() {
    let p1 = "package p1; localparam int X = 3; endpackage
              package q; localparam int X = 5; endpackage";
    // An export refers to the name, so the package cannot declare it.
    let detail = error(&format!(
        "{p1}
         package p6; import p1::*; export p1::X; localparam int X = 1; endpackage
         module Top(output logic [7:0] y); assign y = 0; endmodule"
    ));
    assert!(
        detail.contains("names an item the package declares"),
        "{detail}"
    );
    // The package must import the name from the package it exports it from.
    let detail = error(&format!(
        "{p1}
         package p7; export p1::X; endpackage
         module Top(output logic [7:0] y); assign y = 0; endmodule"
    ));
    assert!(detail.contains("does not import"), "{detail}");
    let detail = error(&format!(
        "{p1}
         package p8; import q::X; import p1::*; export p1::X; endpackage
         module Top(output logic [7:0] y); assign y = 0; endmodule"
    ));
    assert!(detail.contains("`p1::X`"), "{detail}");
    let detail = error(&format!(
        "{p1}
         package p9; import p1::*; export p1::Z; endpackage
         module Top(output logic [7:0] y); assign y = 0; endmodule"
    ));
    assert!(detail.contains("no item `Z`"), "{detail}");
}

/// An exported package variable is the original package's one object.
#[test]
fn exported_package_variables_are_shared() {
    assert_eq!(
        output(
            "package p; logic [7:0] shared; function automatic logic [7:0] inc(logic [7:0] v);
               return v + 8'd1; endfunction endpackage
             package q; import p::*; export p::shared, p::inc; localparam int K = 2; endpackage
             module W(input logic [7:0] a); import q::*; always_comb shared = inc(a) + K; endmodule
             module Top(output logic [7:0] y); W w(.a(8'd9)); assign y = p::shared; endmodule"
        ),
        12
    );
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

/// A package variable is one object shared by every module (IEEE 1800-2023
/// 26.2): what one module writes, the others read.
#[test]
fn modules_share_package_variables() {
    assert_eq!(
        output(
            "package p; logic [7:0] shared; endpackage
             module W(input logic [7:0] a); import p::*; always_comb shared = a + 8'd1; endmodule
             module R(output logic [7:0] b); assign b = p::shared; endmodule
             module Top(output logic [7:0] y); import p::shared; W w(.a(8'd9)); R r(.b(y)); endmodule"
        ),
        10
    );
}

#[test]
fn package_variables_hold_state_across_clocks() {
    let source = "package p; logic [7:0] count = 8'd5; endpackage
        module Counter(input logic clk); always_ff @(posedge clk) p::count <= p::count + 8'd1; endmodule
        module Pass(input logic [7:0] a, output logic [7:0] b); assign b = a; endmodule
        module Top(input logic clk, output logic [7:0] y, output logic [7:0] z);
          Counter c(.clk(clk));
          assign y = p::count;
          Pass q(.a(p::count), .b(z));
        endmodule";
    let mut simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("count.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
    let clk = simulator.event("clk");
    for _ in 0..3 {
        simulator.tick(clk).unwrap();
    }
    let (y, z, count) = (
        simulator.signal("y"),
        simulator.signal("z"),
        simulator.signal("p::count"),
    );
    assert_eq!(simulator.get(y), 8u8.into());
    assert_eq!(simulator.get(z), 8u8.into());
    assert_eq!(simulator.get(count), 8u8.into());
}

#[test]
fn package_variables_take_their_initializers() {
    assert_eq!(
        output(
            "package p; logic [7:0] shared = 8'd42; endpackage
             module Top(output logic [7:0] y); assign y = p::shared; endmodule"
        ),
        42
    );
}

#[test]
fn rejects_package_variables_with_several_drivers() {
    // Two instances of one module are two drivers.
    let error = Simulator::from_sv_sources(
        vec![(
            "package p; logic [7:0] shared; endpackage
             module W(input logic [7:0] a); assign p::shared = a; endmodule
             module Top(output logic [7:0] y);
               W w1(.a(8'd1)); W w2(.a(8'd2));
               assign y = p::shared;
             endmodule",
            std::path::Path::new("drivers.sv"),
        )],
        "Top",
    )
    .build()
    .expect_err("two drivers of a package variable");
    match error.kind() {
        SimulatorErrorKind::SIRParser(ParserError::Unsupported { detail, .. }) => assert!(
            detail.contains("multiple drivers of package variable `p::shared`"),
            "{detail}"
        ),
        other => panic!("expected multiple drivers, got {other:?}"),
    }
}

#[test]
fn package_variables_are_shared_with_veryl_designs() {
    let sv = "package p; logic [7:0] shared; endpackage
        module W(input logic [7:0] a); assign p::shared = a; endmodule
        module R(output logic [7:0] b); assign b = p::shared; endmodule";
    let veryl = r#"
module Top (
    y: output logic<8>,
) {
    var a: logic<8>;
    assign a = 8'd7;
    inst w: $sv::W (a);
    inst r: $sv::R (b: y);
}
"#;
    let mut simulator = Simulator::builder(veryl, "Top")
        .with_sv_sources(vec![(sv, std::path::Path::new("shared.sv"))])
        .build()
        .unwrap_or_else(|error| panic!("{error}"));
    let y = simulator.signal("y");
    assert_eq!(simulator.get(y), 7u8.into());
}

#[test]
fn package_variables_cannot_be_clocks_yet() {
    let error = Simulator::from_sv_sources(
        vec![(
            "package p; logic clk; endpackage
             module Top(input logic c, output logic [7:0] y);
               assign p::clk = c;
               always_ff @(posedge p::clk) y <= y + 8'd1;
             endmodule",
            std::path::Path::new("clock.sv"),
        )],
        "Top",
    )
    .build()
    .expect_err("a package variable used as a clock");
    assert!(
        error
            .to_string()
            .contains("package variable `p::clk` used as a clock or reset"),
        "{error}"
    );
}

#[test]
fn package_variables_take_typedefs_slices_and_several_assignments() {
    // One process may assign a package variable several times; modules may
    // drive disjoint slices of one; and its type may be a typedef.
    assert_eq!(
        output(
            "package p; typedef logic [7:0] word_t; word_t shared; endpackage
             module Low(input logic a);
               always_comb begin p::shared[3:0] = 4'd1; if (a) p::shared[3:0] = 4'd2; end
             endmodule
             module High; assign p::shared[7:4] = 4'd3; endmodule
             module Top(output logic [7:0] y);
               Low l(.a(1'b1)); High h();
               assign y = p::shared;
             endmodule"
        ),
        0x32
    );
}

#[test]
fn escaped_package_variables_are_found_by_name() {
    let source = "package p; logic [7:0] \\v::w  = 8'd5; endpackage
        module Top(output logic [7:0] y); assign y = p::\\v::w ; endmodule";
    let mut simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("escaped.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
    // The escaped name keeps its backslash.
    let variable = simulator.signal("p::\\v::w");
    assert_eq!(simulator.get(variable), 5u8.into());
}

#[test]
fn veryl_instances_of_one_writer_are_several_drivers() {
    let sv = "package p; logic [7:0] shared; endpackage
        module W(input logic clk, input logic [7:0] a); always_ff @(posedge clk) p::shared <= a; endmodule";
    let veryl = r#"
module Top (
    clk: input clock,
    y: output logic<8>,
) {
    var a: logic<8>;
    assign a = 8'd7;
    inst w1: $sv::W (clk, a);
    inst w2: $sv::W (clk, a);
    assign y = a;
}
"#;
    let error = Simulator::builder(veryl, "Top")
        .with_sv_sources(vec![(sv, std::path::Path::new("writers.sv"))])
        .build()
        .expect_err("two instances writing one package variable");
    assert!(
        format!("{error:?}").contains("multiple drivers of package variable `p::shared`"),
        "{error:?}"
    );
}

#[test]
fn initial_blocks_write_package_variables_after_their_initializers() {
    let source = "package p; logic [7:0] shared = 8'd1; logic [7:0] other; endpackage
        module Top(output logic [7:0] y, output logic [7:0] z);
          initial begin p::shared = 8'd42; p::other = 8'd7; end
          assign y = p::shared;
          assign z = p::other;
        endmodule";
    let mut simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("initial.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
    let (y, z) = (simulator.signal("y"), simulator.signal("z"));
    assert_eq!(simulator.get(y), 42u8.into());
    assert_eq!(simulator.get(z), 7u8.into());
    // Diagnostics name the package variable as written.
    let address = simulator.program().get_addr(&[], &["p::shared"]).unwrap();
    assert_eq!(simulator.program().get_path(&address), "p::shared");
}

#[test]
fn package_structure_members_are_written_through_the_package_scope() {
    assert_eq!(
        output(
            "package p;
               typedef struct packed { logic [3:0] hi; logic [3:0] lo; } pair_t;
               pair_t shared;
             endpackage
             module Top(output logic [7:0] y);
               always_comb begin p::shared.hi = 4'd1; p::shared.lo = 4'd2; end
               assign y = p::shared;
             endmodule"
        ),
        0x12
    );
}

#[test]
fn rejects_unsupported_package_state() {
    for (source, expected) in [
        (
            "package p; nettype logic nt; nt shared; endpackage
             module Top(output logic [7:0] y); assign y = 0; endmodule",
            "package net `p::shared`",
        ),
        (
            "package p; logic [7:0] a = 8'd1; endpackage
             package q; logic [7:0] b = p::a; endpackage
             module Top(output logic [7:0] y); assign y = q::b; endmodule",
            "package variable initializer that is not constant in package `q`",
        ),
    ] {
        let error =
            Simulator::from_sv_sources(vec![(source, std::path::Path::new("state.sv"))], "Top")
                .build()
                .expect_err(expected);
        assert!(format!("{error:?}").contains(expected), "{error:?}");
    }
}

#[test]
fn veryl_instances_of_one_initializer_are_several_drivers() {
    let sv = "package p; logic [7:0] shared; endpackage
        module W(input logic [7:0] a); initial p::shared = 8'd1; endmodule";
    let veryl = r#"
module Top (
    y: output logic<8>,
) {
    var a: logic<8>;
    assign a = 8'd7;
    inst w1: $sv::W (a);
    inst w2: $sv::W (a);
    assign y = a;
}
"#;
    let error = Simulator::builder(veryl, "Top")
        .with_sv_sources(vec![(sv, std::path::Path::new("initial.sv"))])
        .build()
        .expect_err("two instances initializing one package variable");
    assert!(
        format!("{error:?}").contains("multiple drivers of package variable `p::shared`"),
        "{error:?}"
    );
}

#[test]
fn package_writers_may_initialize_disjoint_slices() {
    assert_eq!(
        output(
            "package p; logic [7:0] shared; endpackage
             module Low; initial p::shared[3:0] = 4'd1; endmodule
             module High; initial p::shared[7:4] = 4'd2; endmodule
             module Top(output logic [7:0] y); Low l(); High h(); assign y = p::shared; endmodule"
        ),
        0x21
    );
}

#[test]
fn a_self_qualified_package_variable_keeps_one_driver() {
    // The package names its own variable, so its state module binds an alias
    // of it; its initializer is still not a driver.
    assert_eq!(
        output(
            "package p;
               logic [7:0] shared = 8'd1;
               function automatic logic [7:0] get(); return p::shared; endfunction
             endpackage
             module Top(output logic [7:0] y);
               assign p::shared = 8'd9;
               assign y = p::get();
             endmodule"
        ),
        9
    );
}

#[test]
fn instance_paths_ending_in_scope_separators_keep_their_dot() {
    let source = "module C(output logic [7:0] y); assign y = 8'd4; endmodule
        module Top(output logic [7:0] y); C \\u:: (.y(y)); endmodule";
    let simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("escaped.sv"))], "Top")
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
    let program = simulator.program();
    let address = program.get_addr(&[("\\u::", 0)], &["y"]).unwrap();
    assert_eq!(program.get_path(&address), "\\u::.y");
}

/// A declaration of a nested block shadows a name only inside that block,
/// and a qualified name of an exported declaration may repeat.
#[test]
fn nested_block_declarations_shadow_only_inside_them() {
    assert_eq!(
        output(
            "package p1; localparam int X = 3; logic [7:0] shared; endpackage
             package q; import p1::*; export p1::*;
               function automatic int f(input int a);
                 begin int X; X = a; end
                 return a + X;
               endfunction
               localparam int Y = f(1);
               typedef logic [7:0] byte_t; byte_t unused;
               function automatic logic [7:0] g(); return shared; endfunction
             endpackage
             module W; always_comb q::shared = 8'd5; endmodule
             module Top(output logic [7:0] y); import q::X; W w();
               assign y = X * 10 + q::Y + q::shared + q::shared; endmodule"
        ),
        30 + 4 + 10
    );
}

/// A named export does not repair an earlier reference, and a name an
/// explicit import binds is not imported through a wildcard as well.
#[test]
fn exports_follow_lexical_order_and_explicit_imports() {
    let detail = error(
        "package p; localparam int X = 3; endpackage
         package q; localparam int X = 5; endpackage
         package r; import p::*; import q::*; localparam int Y = X; export p::X; endpackage
         module Top(output logic [7:0] y); assign y = r::Y; endmodule",
    );
    assert!(detail.contains("`X`"), "{detail}");
    let detail = error(
        "package p1; localparam int X = 3; endpackage
         package p2; import p1::X; export p1::X; endpackage
         package p3; import p2::X; import p1::*; localparam int Y = X; export p1::*; endpackage
         module Top(output logic [7:0] y); import p3::X; assign y = 0; endmodule",
    );
    assert!(detail.contains("no item `X`"), "{detail}");
}
