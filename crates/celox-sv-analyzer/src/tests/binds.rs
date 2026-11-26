//! Bind directives (IEEE 1800-2023 23.11): where their instantiations go,
//! and what the analysis rejects of them.
use super::*;

fn analyze(source: &str) -> Result<Ir, AnalyzerError> {
    analyze_source(source, Path::new("binds.sv"))
}

const MODULES: &str = "module c(input logic [7:0] v); endmodule \
    module m(input logic [7:0] d); logic [7:0] q; endmodule \
    interface i(input logic [7:0] d); endinterface \
    interface w(input logic [7:0] v); endinterface \
    program p; endprogram ";

fn rejected(items: &str) -> AnalyzerError {
    let source = format!("{MODULES}{items}");
    match analyze(&source) {
        Ok(_) => panic!("{source}: accepted"),
        Err(error) => error.into_error(),
    }
}

#[test]
fn bound_instances_are_instances_of_their_target() {
    for directive in [
        "bind m c k(.v(q));",
        "module top; m u(.d(8'd1)); bind m c k(.v(q)); endmodule",
        "interface j; bind m c k(q); endinterface",
        // An instance of a generate block is no instance of `top` that the
        // target names.
        "module top; if (1) begin : g m m(.d(8'd1)); end bind m c k(.v(q)); endmodule",
    ] {
        let source = format!("{MODULES}{directive}");
        let ir = analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
        let target = ir
            .modules()
            .iter()
            .find(|module| module.name() == "m")
            .expect("module m");
        assert!(
            target
                .instances()
                .iter()
                .any(|instance| instance.name() == "k" && instance.module_name() == "c"),
            "{directive}"
        );
    }
}

#[test]
fn unsupported_bind_targets_name_themselves() {
    for (items, construct) in [
        (
            "module top; m u(.d(8'd1)); bind m: u c k(q); endmodule",
            "list of instances",
        ),
        (
            "module top; m u(.d(8'd1)); bind u c k(q); endmodule",
            "instance `u`",
        ),
        (
            "module top; m u(.d(8'd1)); bind top.u c k(q); endmodule",
            "instance `top.u`",
        ),
        ("bind nowhere c k(q);", "not a module or an interface"),
        (
            "module top; if (1) begin : g bind m c k(q); end endmodule",
            "generate block",
        ),
        (
            "module n(i b); bind m c k(.v(b.d)); endmodule",
            "interface port or instance `b`",
        ),
        (
            "module n; i b(.d(8'd0)); endmodule bind n c k(.v(b.d));",
            "interface port or instance `b`",
        ),
    ] {
        match rejected(items) {
            AnalyzerError::Unsupported(text) => {
                assert!(text.contains(construct), "{items}: unsupported `{text}`")
            }
            error => panic!("{items}: {error:?}, not unsupported `{construct}`"),
        }
    }
}

#[test]
fn illegal_bind_directives_are_errors() {
    for (items, rule) in [
        ("bind p c k(.v(8'd0));", "module or an interface"),
        (
            "bind i c k(.v(d));",
            "instantiates an interface or a checker",
        ),
        ("program q; bind m c k(q); endprogram", "not in `q`"),
        ("bind m logic x;", "gives an instantiation"),
        ("bind m logic [1:0] x;", "gives an instantiation"),
        ("bind m always @(d) q = d;", "gives an instantiation"),
        ("bind m c #(1);", "gives an instantiation"),
    ] {
        match rejected(items) {
            AnalyzerError::InvalidBind(text) => {
                assert!(text.contains(rule), "{items}: invalid `{text}`")
            }
            error => panic!("{items}: {error:?}, not an invalid bind `{rule}`"),
        }
    }
    // `bind` is a keyword (5.6.2, Annex B), whatever the parse of a bind
    // directive makes of it.
    assert!(analyze("module top(); bit bind; endmodule").is_err());
    // The bound instance is an instance of the target scope, so its name
    // clashes with the target's own names (23.11).
    let error = rejected("module n; c k(.v(8'd0)); endmodule bind n c k(.v(8'd1));");
    assert!(
        matches!(&error, AnalyzerError::DuplicateInstance { module, name } if module == "n" && name == "k"),
        "{error:?}"
    );
}

/// The instantiation of a directive after its target keeps the offsets of
/// the directive, where the interface pass must not take the modules between
/// them for parts of the target.
#[test]
fn modules_between_a_target_and_its_directive_are_not_the_target() {
    let source = "interface bus; logic hidden; logic shown; \
        modport narrow(input shown); endinterface \
        module t(bus.narrow b); logic [7:0] q; endmodule \
        module between(bus b); logic seen; assign seen = b.hidden; endmodule \
        module top; bus u(); t x(.b(u)); between y(.b(u)); endmodule \
        module c(input logic [7:0] v); endmodule \
        bind t c k(.v(q));";
    let ir = analyze(source).unwrap_or_else(|error| panic!("{error}"));
    let target = ir
        .modules()
        .iter()
        .find(|module| module.name() == "t")
        .expect("module t");
    assert!(
        target
            .instances()
            .iter()
            .any(|instance| instance.name() == "k"),
        "{target:?}"
    );
}

/// The names of named connections and parameter values are the bound
/// module's own, not references to interfaces of the target.
#[test]
fn formal_names_of_bound_instances_are_no_interface_references() {
    let source = format!(
        "{MODULES}module n; i b(.d(8'd0)); logic [7:0] q; endmodule \
         module f #(parameter int b = 1)(input logic [7:0] b2, input logic [7:0] v); endmodule \
         module g(input logic [7:0] b); endmodule \
         package pk; parameter int b = 3; endpackage \
         bind n g p(.b(q)); bind n f #(.b(2)) r(.b2(q), .v(q)); \
         bind n f #(.b(pk::b)) s(.b2(q), .v(q));"
    );
    analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
}

/// A module nested in the scope of a directive is a target it names, and a
/// package qualifier is no reference to an interface of that name.
#[test]
fn nested_targets_and_package_qualifiers() {
    let source = "module c(input logic [7:0] v); endmodule \
        package pk; parameter int K = 3; endpackage \
        interface bus; logic x; endinterface \
        module f #(parameter int K = 1)(input logic [7:0] v); endmodule \
        module Top(); module m(); logic [7:0] q; endmodule m u(); bus pk(); \
        bind m c p(.v(q)); bind Top f #(.K(pk::K)) r(.v(8'd0)); endmodule";
    let ir = analyze(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    let target = ir
        .modules()
        .iter()
        .find(|module| module.name().ends_with('m'))
        .expect("module m");
    assert!(
        target
            .instances()
            .iter()
            .any(|instance| instance.name() == "p"),
        "{target:?}"
    );
}

/// A module nested in another design element is no target of a directive
/// outside that element, which does not see it.
#[test]
fn nested_modules_of_other_scopes_are_no_targets() {
    let source = "module c(); endmodule \
        module Top(); module other(); module m(); endmodule m inner(); endmodule \
        other o(); bind m c p(); endmodule";
    match analyze(source).map_err(|error| error.into_error()) {
        Err(AnalyzerError::Unsupported(text)) => {
            assert!(text.contains("is not a module or an interface"), "{text}")
        }
        other => panic!("{source}: {other:?}"),
    }
}
