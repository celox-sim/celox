//! A wildcard import makes the names of its package candidates for the
//! references after it, and only those (IEEE 1800-2023 26.3): a reference
//! before it binds a declaration of an outer scope, in a module, a generate
//! block and a procedural block alike.

use std::path::Path;

/// Two packages declaring `f` and `c`, and a compilation unit declaring
/// them too.
const DECLARATIONS: &str = r#"`timescale 1ns/1ns
function automatic int f(); return 1; endfunction
int c = 1;
package pf; function automatic int f(); return 10; endfunction int c = 10; endpackage
package qf; function automatic int f(); return 20; endfunction int c = 20; endpackage
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

fn build(source: &str, four_state: bool) -> Result<celox::Simulation, celox::SimulatorError> {
    let source = format!("{DECLARATIONS}{source}");
    celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("imports.sv"))], "top")
        .four_state(four_state)
        .build()
}

/// The displays of `source` after the declarations, in each state mode.
fn run(source: &str) -> Vec<Vec<String>> {
    [false, true]
        .into_iter()
        .map(|four_state| {
            let mut sim = build(source, four_state).unwrap_or_else(|error| panic!("{error}"));
            sim.run_until(1).unwrap();
            displays(&mut sim)
        })
        .collect()
}

#[test]
fn references_before_a_wildcard_import_of_a_module_bind_the_compilation_unit() {
    let source = r#"module top;
  int x, y;
  initial begin x = f(); y = c; $display("%0d %0d", x, y); end
  import qf::*;
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["1 1"]);
    }
}

#[test]
fn references_after_a_wildcard_import_of_a_module_bind_the_package() {
    let source = r#"module top;
  import qf::*;
  initial $display("%0d %0d", f(), c);
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["20 20"]);
    }
}

#[test]
fn a_later_wildcard_import_of_a_module_is_no_conflict_for_earlier_references() {
    let source = r#"module top;
  import pf::*;
  initial $display("%0d %0d", f(), c);
  import qf::*;
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["10 10"]);
    }
}

#[test]
fn references_before_a_wildcard_import_of_a_generate_block_bind_the_compilation_unit() {
    let source = r#"module top;
  if (1) begin : g
    initial $display("%0d %0d", f(), c);
    import qf::*;
  end
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["1 1"]);
    }
}

#[test]
fn a_later_wildcard_import_of_a_generate_block_is_no_conflict_for_earlier_references() {
    let source = r#"module top;
  if (1) begin : g
    import pf::*;
    initial $display("%0d %0d", f(), c);
    import qf::*;
  end
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["10 10"]);
    }
}

#[test]
fn references_before_a_wildcard_import_of_a_procedural_block_bind_the_compilation_unit() {
    let source = r#"module top;
  initial begin
    int x = f();
    int y = c;
    import qf::*;
    $display("%0d %0d", x, y);
  end
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["1 1"]);
    }
}

/// (An initializer of a block local that reads a package variable through
/// a wildcard import of the block is not supported yet, so only `f` is
/// read here.)
#[test]
fn a_later_wildcard_import_of_a_procedural_block_is_no_conflict_for_earlier_references() {
    let source = r#"module top;
  initial begin
    import pf::*;
    int x = f();
    import qf::*;
    $display("%0d", x);
  end
endmodule
"#;
    for displays in run(source) {
        assert_eq!(displays, ["10"]);
    }
}

/// A name is bound once for a module: references before and after the
/// import would bind different declarations, which is rejected.
#[test]
fn references_on_both_sides_of_a_wildcard_import_of_a_module_are_rejected() {
    let source = r#"module top;
  initial $display("%0d", f());
  import qf::*;
  initial $display("%0d", f());
endmodule
"#;
    let Err(error) = build(source, false) else {
        panic!("references on both sides of the import must be rejected");
    };
    let error = error.to_string();
    assert!(error.contains("both before and after"), "{error}");
}

/// Packages only, for compilation units that import them.
const PACKAGES: &str = r#"`timescale 1ns/1ns
package pf; function automatic int f(); return 10; endfunction endpackage
package qf; function automatic int f(); return 20; endfunction endpackage
"#;

fn build_unit(source: &str) -> Result<celox::Simulation, celox::SimulatorError> {
    let source = format!("{PACKAGES}{source}");
    celox::Simulation::from_sv_sources(vec![(source.as_str(), Path::new("imports.sv"))], "top")
        .build()
}

/// A module may import again what its compilation unit imports: references
/// before and after its own import denote one declaration then.
#[test]
fn a_wildcard_import_of_a_module_may_repeat_one_of_its_compilation_unit() {
    let source = r#"import qf::*;
module top;
  initial $display("%0d", f());
  import qf::*;
  initial #1 $display("%0d", f());
endmodule
"#;
    let mut sim = build_unit(source).unwrap_or_else(|error| panic!("{error}"));
    sim.run_until(2).unwrap();
    assert_eq!(displays(&mut sim), ["20", "20"]);
}

/// The compilation unit imports another package than the module: the
/// references on the two sides of the module's import denote different
/// declarations.
#[test]
fn references_around_an_import_of_another_package_than_the_compilation_unit_are_rejected() {
    let source = r#"import pf::*;
module top;
  initial $display("%0d", f());
  import qf::*;
  initial $display("%0d", f());
endmodule
"#;
    let Err(error) = build_unit(source) else {
        panic!("references denoting different declarations must be rejected");
    };
    let error = error.to_string();
    assert!(error.contains("both before and after"), "{error}");
}

/// The same for a generate block: its import binds the name in the whole
/// block, so references before and after it that denote different
/// declarations are rejected.
#[test]
fn references_on_both_sides_of_a_wildcard_import_of_a_generate_block_are_rejected() {
    let source = r#"module top;
  if (1) begin : g
    initial $display("%0d", f());
    import qf::*;
    initial $display("%0d", f());
  end
endmodule
"#;
    let Err(error) = build(source, false) else {
        panic!("references on both sides of the block's import must be rejected");
    };
    let error = error.to_string();
    assert!(error.contains("both before and after"), "{error}");
}

/// A generate block may import again a package the module imports before
/// it: references on both sides of its import denote one declaration.
#[test]
fn a_wildcard_import_of_a_generate_block_may_repeat_one_of_its_module() {
    let source = r#"module top;
  import qf::*;
  if (1) begin : g
    initial $display("%0d", f());
    import qf::*;
    initial #1 $display("%0d", f());
  end
endmodule
"#;
    for four_state in [false, true] {
        let mut sim = build(source, four_state).unwrap_or_else(|error| panic!("{error}"));
        sim.run_until(2).unwrap();
        assert_eq!(displays(&mut sim), ["20", "20"]);
    }
}

/// A reference after two wildcard imports of a block whose packages both
/// declare the name sees both of them and is rejected as their conflict,
/// in a procedural block and a generate block alike: no reference binds
/// one of the two declarations silently. (IEEE 1800-2023 26.3 binds it to
/// the one an earlier reference already made locally visible: binding by
/// position is a follow-up.)
#[test]
fn references_after_two_wildcard_imports_of_a_block_are_rejected() {
    for source in [
        r#"module top;
  initial begin
    import pf::*;
    int x = f();
    import qf::*;
    int y = f();
    $display("%0d %0d", x, y);
  end
endmodule
"#,
        r#"module top;
  if (1) begin : g
    import pf::*;
    initial $display("%0d", f());
    import qf::*;
    initial #1 $display("%0d", f());
  end
endmodule
"#,
    ] {
        let Err(error) = build(source, false) else {
            panic!("a reference after both imports must be rejected:\n{source}");
        };
        let error = error.to_string();
        assert!(
            error.contains("declared by both wildcard-imported packages"),
            "{error}"
        );
    }
}

/// The import of an inner generate block binds the name in the whole inner
/// block, also for a reference there before it that the enclosing block's
/// import resolves: they denote different declarations, so it is rejected.
#[test]
fn references_inherited_from_an_enclosing_generate_import_are_checked() {
    let source = r#"module top;
  if (1) begin : g
    import pf::*;
    if (1) begin : h
      initial $display("%0d", f());
      import qf::*;
      initial #1 $display("%0d", f());
    end
  end
endmodule
"#;
    let Err(error) = build(source, false) else {
        panic!("the reference inherited from the enclosing import must be checked");
    };
    let error = error.to_string();
    assert!(error.contains("both before and after"), "{error}");
}

/// Every reference before `export p::x;` is checked, not only the first: a
/// later one after another wildcard import that also declares the name is
/// rejected rather than bound to the exported declaration.
#[test]
fn every_reference_before_an_export_is_checked() {
    let source = r#"package ex;
  import pf::*;
  function automatic int a(); return f(); endfunction
  import qf::*;
  function automatic int b(); return f(); endfunction
  export pf::f;
endpackage
module top;
  initial $display("%0d %0d", ex::a(), ex::b());
endmodule
"#;
    let Err(error) = build(source, false) else {
        panic!("the second reference before the export must be checked");
    };
    let error = error.to_string();
    assert!(
        error.contains("declared by both wildcard-imported packages")
            || error.contains("both before and after"),
        "{error}"
    );
}
