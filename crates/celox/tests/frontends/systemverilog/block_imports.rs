//! Package imports declared in procedural blocks and subroutine bodies
//! (IEEE 1800-2023 26.3, A.2.8): they bind the names the block references
//! after them, and nowhere else.

use std::path::Path;

const PACKAGE: &str = r#"`timescale 1ns/1ns
package p;
  function int f(); return 5; endfunction
  parameter int K = 7;
  typedef logic [3:0] nib_t;
  class db #(type T = int);
    static T stored;
    static function void set(T v); stored = v; endfunction
  endclass
endpackage
"#;

fn displays(sim: &mut celox::Simulation) -> Vec<String> {
    sim.drain_runtime_events()
        .into_iter()
        .filter_map(|event| match event {
            celox::RuntimeEvent::Display { message } => {
                Some(String::from_utf8_lossy(&message).into_owned())
            }
            _ => None,
        })
        .collect()
}

/// The displays of `source`, with the package, run until `until` with tops
/// `tops`, in each state mode.
fn run(source: &str, tops: &[&str], until: u64) -> Vec<Vec<String>> {
    let source = format!("`timescale 1ns/1ns\n{source}");
    [false, true]
        .into_iter()
        .map(|four_state| {
            let mut sim = celox::Simulation::from_sv_sources(
                vec![
                    (PACKAGE, Path::new("package.sv")),
                    (source.as_str(), Path::new("imports.sv")),
                ],
                "",
            )
            .tops(tops.iter().copied())
            .four_state(four_state)
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
            sim.run_until(until).unwrap();
            displays(&mut sim)
        })
        .collect()
}

#[test]
fn explicit_imports_of_blocks_bind_their_names() {
    let source = r#"module top; initial begin import p::f; $display("f %0d", f()); end endmodule
"#;
    for displays in run(source, &["top"], 1) {
        assert_eq!(displays, ["f 5"]);
    }
}

#[test]
fn wildcard_imports_of_blocks_bind_functions_parameters_and_types() {
    let source = r#"module top; initial begin import p::*; nib_t n; n = 4'hf; $display("w %0d %0d %0d", f(), K, n); end endmodule
"#;
    for displays in run(source, &["top"], 1) {
        assert_eq!(displays, ["w 5 7 15"]);
    }
}

#[test]
fn locals_declared_after_a_wildcard_import_hide_its_names() {
    let source = r#"module top; initial begin import p::*; int K; K = 3; $display("k %0d", K); end endmodule
"#;
    for displays in run(source, &["top"], 1) {
        assert_eq!(displays, ["k 3"]);
    }
}

#[test]
fn imports_of_blocks_name_class_specializations() {
    let source = r#"interface bfm; int x = 9; endinterface
module hdl_top; bfm i(); initial begin import p::db; db #(virtual bfm)::set(i); end endmodule
module hvl_top; initial begin #1 $display("x %0d", p::db #(virtual bfm)::stored.x); end endmodule
"#;
    for displays in run(source, &["hdl_top", "hvl_top"], 2) {
        assert_eq!(displays, ["x 9"]);
    }
}

#[test]
fn imports_of_subroutine_bodies_and_nested_blocks() {
    let source = r#"module top; task t(); import p::*; $display("t %0d", f() + K); endtask initial begin begin import p::K; $display("n %0d", K); end t(); end endmodule
"#;
    for displays in run(source, &["top"], 1) {
        assert_eq!(displays, ["n 7", "t 12"]);
    }
}

#[test]
fn names_imported_in_a_block_are_unknown_outside_it() {
    let source = format!(
        "`timescale 1ns/1ns\n{}",
        r#"module top; initial begin begin import p::f; end $display("%0d", f()); end endmodule
"#
    );
    let result = celox::Simulation::from_sv_sources(
        vec![
            (PACKAGE, Path::new("package.sv")),
            (source.as_str(), Path::new("imports.sv")),
        ],
        "top",
    )
    .build();
    assert!(
        result.is_err(),
        "`f` is unknown after the block that imports it"
    );
}

/// Packages that give a name several declarations.
const OVERLAPPING: &str = r#"
package pa; parameter int K = 4; function int f(); return 1; endfunction endpackage
package qa; parameter int pa = 20; function int f(); return 2; endfunction endpackage
package qb; parameter int pa = 30; endpackage
"#;

#[test]
fn explicit_imports_of_blocks_take_precedence_over_their_wildcard_imports() {
    let source = format!(
        "{OVERLAPPING}{}",
        r#"module top; initial begin import pa::f; import qa::*; $display("f %0d", f()); end endmodule"#
    );
    for displays in run(&source, &["top"], 1) {
        assert_eq!(displays, ["f 1"]);
    }
}

#[test]
fn package_qualifiers_in_blocks_are_no_wildcard_candidates() {
    // `qa` and `qb` both declare `pa`, which `pa::f` does not reference.
    let source = format!(
        "{OVERLAPPING}{}",
        r#"module top; initial begin import qa::*; import qb::*; int x; x = pa::K; $display("q %0d %0d", pa::f(), x); end endmodule"#
    );
    for displays in run(&source, &["top"], 1) {
        assert_eq!(displays, ["q 1 4"]);
    }
}

#[test]
fn explicit_imports_of_names_a_block_declares_are_rejected() {
    let source = format!(
        "`timescale 1ns/1ns\n{OVERLAPPING}{}",
        r#"module top; initial begin int f; import pa::f; f = 3; $display("%0d", f); end endmodule"#
    );
    let Err(error) = celox::Simulation::from_sv_sources(
        vec![
            (PACKAGE, Path::new("package.sv")),
            (source.as_str(), Path::new("imports.sv")),
        ],
        "top",
    )
    .build() else {
        panic!("`import pa::f;` names the block's `f`");
    };
    let error = error.to_string();
    assert!(error.contains("names an item the block"), "{error}");
}

#[test]
fn imports_of_blocks_leave_package_qualifiers_alone() {
    // `qc::*` binds the call `pa()` of the block to `qc::pa`; the qualifier
    // of `pa::f()` still names package `pa`.
    let source = format!(
        "{OVERLAPPING}{}",
        r#"package qc; function int pa(); return 40; endfunction endpackage
module top; initial begin import qc::*; $display("q %0d %0d", pa(), pa::f()); end endmodule"#
    );
    for displays in run(&source, &["top"], 1) {
        assert_eq!(displays, ["q 40 1"]);
    }
}
