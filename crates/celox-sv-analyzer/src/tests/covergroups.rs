//! Covergroups (IEEE 1800-2023 clause 19): what the analysis makes of their
//! declarations and uses.
use super::*;

/// The issue that tracks what is left of clause 19: every construct name
/// that starts with `covergroup` maps to it.
const COVERGROUPS_ISSUE: u32 = 4294;

fn analyze(source: &str) -> Result<Ir, AnalyzerError> {
    analyze_source(source, Path::new("covergroups.sv"))
}

fn module<'a>(ir: &'a Ir, name: &str) -> &'a crate::ir::Module {
    ir.modules()
        .iter()
        .find(|module| module.name() == name)
        .unwrap_or_else(|| panic!("no module `{name}`"))
}

/// The subroutines of `module` that construct a covergroup.
fn constructors(module: &crate::ir::Module) -> Vec<&str> {
    module
        .subroutines()
        .iter()
        .map(|subroutine| subroutine.name.as_str())
        .filter(|name| name.starts_with("$covergroup::") && name.ends_with("::$new"))
        .collect()
}

/// The error of a module `Top` with `items`, after the declarations
/// `before` it.
fn rejected(before: &str, items: &str) -> AnalyzerError {
    let source = format!("{before} module Top; {items} endmodule");
    match analyze(&source) {
        Ok(_) => panic!("{source}: accepted"),
        Err(error) => error.into_error(),
    }
}

#[test]
fn covergroup_types_are_types_of_their_scopes() {
    // 19.3, 6.22: a covergroup is a type of the scope that declares it. Two
    // modules that declare `cg` declare two types; the type of a package
    // is one of the design.
    let ir = analyze(
        "module A; covergroup cg; endgroup cg c; initial c = new; endmodule \
         module B; covergroup cg; endgroup cg c; initial c = new; endmodule \
         module Top; A a(); B b(); endmodule",
    )
    .unwrap();
    let (a, b) = (
        constructors(module(&ir, "A")),
        constructors(module(&ir, "B")),
    );
    assert_eq!((a.len(), b.len()), (1, 1));
    assert_ne!(a, b);
    assert!(constructors(module(&ir, "Top")).is_empty());
    // The elaborations of one module with different parameters each declare
    // a type; with the same parameters they share it.
    let parameterized = "module M #(parameter int W = 1); covergroup cg; endgroup \
         cg c; initial c = new; endmodule";
    let packages = analyze_packages(&[]).unwrap();
    let elaborated_with = |overrides: &[(&str, i128)]| {
        let ir = analyze_source_module_with_packages(
            parameterized,
            Path::new("covergroups.sv"),
            "M",
            &overrides
                .iter()
                .map(|(name, value)| (name.to_string(), *value))
                .collect(),
            &packages,
            &mut packages.design(),
        )
        .unwrap();
        constructors(module(&ir, "M"))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let elaborated = |width: i128| elaborated_with(&[("W", width)]);
    assert_eq!(elaborated(2), elaborated(2));
    assert_ne!(elaborated(2), elaborated(3));
    // An elaboration that takes the default of a parameter and one that
    // writes the same value share the type (6.22): it is named by the values
    // of the parameters, not by the overrides.
    assert_eq!(elaborated_with(&[]), elaborated(1));
    assert_ne!(elaborated_with(&[]), elaborated(2));
    // The values are told by their types too: a value parameter without a
    // type takes the type of its value (6.20.2), so `1'b1` and `4'b1` make
    // two types, and `4'b1` and `4'h1` one. A parameter of a type converts
    // the value to it: `2`, `32'sd2` and `4'd2` are the same `int`.
    let typed = |source: &str, value: &str| {
        let ir = crate::analyze_source_module_with_parameter_expr_overrides(
            source,
            Path::new("covergroups.sv"),
            "M",
            &HashMap::from_iter([(
                "P".to_string(),
                crate::ir::ConstExpr::Literal(value.to_string()),
            )]),
            &crate::ModuleInterfaces::default(),
        )
        .unwrap_or_else(|error| panic!("{value}: {error}"));
        constructors(module(&ir, "M"))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let implicit = "module M #(parameter P = 1); covergroup cg; endgroup \
         cg c; initial c = new; endmodule";
    assert_ne!(typed(implicit, "1'b1"), typed(implicit, "4'b1"));
    assert_ne!(typed(implicit, "4'b1"), typed(implicit, "4'sb1"));
    assert_eq!(typed(implicit, "4'b1"), typed(implicit, "4'h1"));
    let int = "module M #(parameter int P = 1); covergroup cg; endgroup \
         cg c; initial c = new; endmodule";
    assert_eq!(typed(int, "2"), typed(int, "32'sd2"));
    assert_eq!(typed(int, "2"), typed(int, "4'd2"));
    assert_ne!(typed(int, "2"), typed(int, "3"));
    // Values the numeric constants do not hold: reals, values with unknown
    // bits and wide values are told by their resolved literals.
    let real = "module M #(parameter real P = 1.0); covergroup cg; endgroup \
         cg c; initial c = new; endmodule";
    assert_eq!(typed(real, "1.5"), typed(real, "1.50"));
    assert_ne!(typed(real, "1.5"), typed(real, "2.5"));
    assert_ne!(typed(implicit, "4'b1x0z"), typed(implicit, "4'b1x01"));
    assert_ne!(typed(implicit, "4'b1x0z"), typed(implicit, "4'b100z"));
    let wide = |high: &str| format!("140'h{high}{}", "0".repeat(32));
    assert_ne!(typed(implicit, &wide("1")), typed(implicit, &wide("2")));
    assert_eq!(typed(implicit, &wide("1")), typed(implicit, &wide("1")));
    // A covergroup of a package is named by the package.
    let ir = analyze(
        "package p; covergroup cg; endgroup endpackage \
         module Top; p::cg c; initial c = new; endmodule",
    )
    .unwrap();
    assert_eq!(
        constructors(module(&ir, "Top")),
        ["$covergroup::p::cg::$new"]
    );
    assert!(module(&ir, "Top").constructs_covergroup());
}

#[test]
fn covergroups_nothing_constructs_have_no_subroutines() {
    // A variable of a covergroup type is a handle the module declares; the
    // type has subroutines only where something constructs or samples it,
    // also when a package the module imports declares it.
    for source in [
        "module Top; covergroup cg; endgroup cg c; cg many[2]; endmodule",
        "package p; covergroup cg; endgroup endpackage \
         module Top; import p::*; cg c; p::cg d; endmodule",
        "covergroup ucg; endgroup module Top; ucg c; endmodule",
        "module Top; bit x; covergroup cg @(posedge x); coverpoint x; endgroup cg c; endmodule",
    ] {
        let ir = analyze(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        let top = module(&ir, "Top");
        assert!(
            top.signals().iter().any(|signal| signal.name() == "c"),
            "{source}"
        );
        assert!(
            top.subroutines()
                .iter()
                .all(|subroutine| !subroutine.name.starts_with("$covergroup::")),
            "{source}"
        );
        assert!(!top.constructs_covergroup(), "{source}");
    }
}

#[test]
fn covergroup_handles_are_assigned_and_compared_as_class_handles() {
    // 19.3: a variable of a covergroup type holds a handle or `null`. A
    // handle of another covergroup type, or of a class, is no value of it.
    analyze(
        "module Top; covergroup cg; endgroup cg a, b; bit same; \
         initial begin a = new; b = a; b = null; same = a == b; \
         if (a != null) a.sample(); end endmodule",
    )
    .unwrap();
    for (items, what) in [
        (
            "covergroup cg; endgroup covergroup other; endgroup \
             cg a; other b; initial begin b = new; a = b; end",
            "another covergroup type",
        ),
        (
            "class C; endclass covergroup cg; endgroup cg a; C c; initial begin c = new; a = c; end",
            "a class",
        ),
    ] {
        let error = rejected("", items);
        assert!(
            matches!(error, AnalyzerError::IncompatibleClassHandle { .. }),
            "{what}: {error}"
        );
    }
}

#[test]
fn embedded_covergroup_assignment_outside_constructor() {
    // 19.4: an embedded covergroup variable is assigned in the `new` method
    // of its class only, and that of the object being constructed.
    let class = |members: &str| {
        format!("class C; covergroup cg; endgroup {members} endclass C c; initial c = new;")
    };
    for members in [
        "function new(); cg = new; endfunction",
        "function new(); this.cg = new(); endfunction",
        "function new(); endfunction",
    ] {
        let source = format!("module Top; {} endmodule", class(members));
        analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
    for items in [
        class("function void make(); cg = new; endfunction"),
        class("function void take(C other); other.cg = null; endfunction"),
        format!(
            "{} class D extends C; function new(); cg = new; endfunction endclass D d; initial d = new;",
            class("")
        ),
        format!(
            "{} class D extends C; function new(C other); other.cg = null; endfunction endclass",
            class("")
        ),
        format!("{} initial c.cg = null;", class("")),
    ] {
        let error = rejected("", &items);
        assert_eq!(
            error,
            AnalyzerError::CovergroupAssignment {
                owner: "C".into(),
                name: "cg".into()
            },
            "{items}"
        );
        assert!(!error.is_unsupported(), "{items}");
    }
    // Inside that `new` method the standard does not say through which
    // handle. The subroutine that constructs the covergroup takes the
    // object being constructed, so another handle of the class than `this`
    // is not supported; it is no assignment outside the constructor.
    for members in [
        "function new(C other); if (other != null) other.cg = new; endfunction",
        "function new(C other); other.cg = null; endfunction",
        "function new(); C self = this; self.cg = new; endfunction",
    ] {
        let error = rejected("", &class(members));
        assert_eq!(
            error,
            AnalyzerError::Unsupported(
                "covergroup embedded in a class assigned through another handle than `this` (IEEE 1800-2023 19.4)"
                    .into()
            ),
            "{members}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
}

#[test]
fn typename_of_a_covergroup_type_is_rejected() {
    // 20.6.1 has no rule for the name of a covergroup type, and the type is
    // a reserved name no source spells: `$typename` of a variable of the
    // type, of the type, and of a type that holds one is rejected, never
    // answered with the reserved name.
    let covergroup =
        "covergroup cg; endgroup cg c; cg many[2]; struct { cg h; int n; } s; string t;";
    for operand in ["c", "cg", "many", "s"] {
        let error = rejected(
            "",
            &format!("{covergroup} initial t = $typename({operand});"),
        );
        assert_eq!(
            error,
            AnalyzerError::Unsupported(
                "covergroup type as the operand of `$typename` (IEEE 1800-2023 19.3, 20.6.1)"
                    .into()
            ),
            "{operand}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
    for (before, items) in [
        (
            "package p; covergroup pcg; endgroup endpackage",
            "p::pcg c; string t; initial t = $typename(c);",
        ),
        (
            "covergroup ucg; endgroup",
            "ucg c; string t; initial t = $typename(c);",
        ),
        (
            "",
            "class C; covergroup cg; endgroup function new(); cg = new; endfunction endclass \
             C o; string t; initial begin o = new; t = $typename(o.cg); end",
        ),
    ] {
        let error = rejected(before, items);
        let AnalyzerError::Unsupported(text) = &error else {
            panic!("{items}: {error}");
        };
        assert!(
            text.starts_with("covergroup type as the operand of `$typename`"),
            "{items}: {text}"
        );
    }
    // The class that holds an embedded covergroup keeps its name, and a
    // design that never asks for the name of a covergroup type analyzes.
    analyze(
        "module Top; covergroup cg; endgroup cg c; int n; \
         class C; covergroup ecg; endgroup function new(); ecg = new; endfunction endclass \
         C o; string t; initial begin t = $typename(n); t = $typename(o); end endmodule",
    )
    .unwrap();
}

#[test]
fn illegal_covergroup_declarations_are_errors() {
    // What the standard forbids is an error of the declaration, not a
    // construct Celox does not support.
    for (before, items, rule) in [
        // 8.26.9: an interface class declares no covergroup.
        (
            "interface class I; covergroup cg; endgroup pure virtual function void f(); endclass \
             class C implements I; virtual function void f(); endfunction endclass",
            "C c; initial c = new;",
            "8.26.9",
        ),
        // 19.3: the arguments of a covergroup are inputs or references.
        ("", "covergroup cg(output int x); endgroup cg c;", "19.3"),
        ("", "covergroup cg(inout int x); endgroup cg c;", "19.3"),
        // 19.8.1: the sample method has no output argument, and none of
        // the name of an argument of the covergroup.
        (
            "",
            "covergroup cg with function sample(output int x); endgroup cg c;",
            "19.8.1",
        ),
        (
            "",
            "covergroup cg(int x) with function sample(int x); endgroup cg c;",
            "19.8.1",
        ),
    ] {
        let error = rejected(before, items);
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{items}: {error}");
        };
        assert!(text.contains(rule), "{items}: {text}");
        assert!(!error.is_unsupported());
    }
    // An inout argument of the sample method is legal (19.8.1 excludes
    // outputs only).
    analyze(
        "module Top; covergroup cg with function sample(inout int x); endgroup \
         cg c; int v; initial begin c = new; c.sample(v); end endmodule",
    )
    .unwrap();
}

#[test]
fn predefined_methods_the_standard_excludes_are_errors() {
    // 19.8: `get_coverage` is the only static method of a covergroup, and
    // `sample` returns nothing.
    let covergroup = "covergroup cg; endgroup cg c; real r;";
    for (items, rule) in [
        ("initial r = cg::get_inst_coverage();", "19.8"),
        ("initial cg::sample();", "19.8"),
        ("initial cg::start();", "19.8"),
        ("initial cg::stop();", "19.8"),
        ("initial cg::set_inst_name(\"x\");", "19.8"),
        ("initial r = c.sample();", "19.8"),
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{items}: {error}");
        };
        assert!(text.contains(rule), "{items}: {text}");
    }
    analyze(&format!(
        "module Top; {covergroup} initial begin c = new; r = cg::get_coverage(); \
         r = c.get_coverage(); r = c.get_inst_coverage(); r = c.get_coverage; end endmodule"
    ))
    .unwrap();
}

#[test]
fn unsupported_covergroup_constructs_are_rejected_where_used() {
    // Each construct the analysis does not support in a covergroup is
    // rejected where the type is constructed or one of its methods called,
    // with a text that starts with `covergroup `, which the tracking issue
    // of covergroups keys on; the declaration alone, and a variable of the
    // type, analyze.
    let constructs = [
        (
            "bit x, y; covergroup cg; xy: cross x, y { bins b = xy with (x > y); } endgroup",
            "covergroup cross with or matches selection (IEEE 1800-2023 19.6.1.2)",
        ),
        (
            "task automatic t; endtask covergroup cg @@(begin t); endgroup",
            "covergroup block event (IEEE 1800-2023 19.3)",
        ),
        (
            "covergroup cg(int v[2]); endgroup",
            "covergroup argument of unpacked type (IEEE 1800-2023 19.3)",
        ),
        // A `let` is not expanded after the name of a covergroup: one that
        // the default or the type of an argument names is rejected, rather
        // than bound as a call of a function that nothing declares.
        (
            "let twice(x) = x + x; covergroup cg(int v = twice(1)); endgroup",
            "covergroup argument that names `let` `twice` (IEEE 1800-2023 11.12, 19.3)",
        ),
        (
            "let twice(x) = x + x; covergroup cg(bit [twice(1):0] v = 0); endgroup",
            "covergroup argument that names `let` `twice` (IEEE 1800-2023 11.12, 19.3)",
        ),
        (
            "let three = 3; covergroup cg with function sample(int three = three); endgroup",
            "covergroup argument that names `let` `three` (IEEE 1800-2023 11.12, 19.3)",
        ),
    ];
    for (declaration, construct) in constructs {
        for unused in ["", "cg c;"] {
            let source = format!("module Top; {declaration} {unused} endmodule");
            analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
        }
        for used in [
            "cg c = new;",
            "cg c; initial c = new();",
            "cg c; initial c.sample();",
            "cg c; real r; initial r = c.get_inst_coverage();",
            "real r; initial r = cg::get_coverage();",
        ] {
            let error = rejected("", &format!("{declaration} {used}"));
            assert_eq!(
                error,
                AnalyzerError::Unsupported(construct.to_string()),
                "{declaration} {used}"
            );
            assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
        }
    }
    // What a covergroup without items does not support yet.
    let covergroup = "covergroup cg; endgroup cg c; real r; int a, b;";
    for (items, construct) in [
        (
            "cg d; initial d = new c;",
            "covergroup shallow copy with new",
        ),
        (
            "bit clk, s; sequence q; (s, c.sample()); endsequence \
             cover property (@(posedge clk) q);",
            "covergroup sample call in a sequence match item",
        ),
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        let AnalyzerError::Unsupported(text) = &error else {
            panic!("{items}: {error}");
        };
        assert!(text.starts_with(construct), "{items}: {text}");
        assert!(text.contains("IEEE 1800-2023"), "{items}: {text}");
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
    // A formal the analysis cannot bind is rejected as an argument of the
    // covergroup, where the covergroup is used.
    for declaration in [
        "bit v; covergroup cg(type(v) x); endgroup",
        "covergroup cg(undeclared_t x); endgroup",
    ] {
        analyze(&format!("module Top; {declaration} cg c; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        let error = rejected("", &format!("{declaration} cg c = new(1);"));
        let AnalyzerError::Unsupported(text) = &error else {
            panic!("{declaration}: {error}");
        };
        assert!(
            text.starts_with("covergroup argument ("),
            "{declaration}: {text}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
    // The subroutines of an embedded covergroup take the object that holds
    // it, which only the variable of the covergroup names.
    let embedded = |methods: &str| {
        format!(
            "class C; covergroup cg; endgroup function new(); cg = new; endfunction {methods} \
             endclass C a, b; bit first; real r;"
        )
    };
    for (methods, items) in [
        (
            "",
            "initial begin a = new; b = new; (first ? a.cg : b.cg).sample(); end",
        ),
        (
            "",
            "initial begin a = new; b = new; r = (first ? a.cg : b.cg).get_inst_coverage(); end",
        ),
        (
            "function void pick(C other, bit first); (first ? cg : other.cg).sample(); endfunction",
            "initial begin a = new; b = new; a.pick(b, first); end",
        ),
    ] {
        let error = rejected("", &format!("{} {items}", embedded(methods)));
        assert_eq!(
            error,
            AnalyzerError::Unsupported(
                "covergroup embedded in a class used through an expression that is not its variable (IEEE 1800-2023 19.4)"
                    .into()
            ),
            "{items}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
    // A covergroup of a checker is no type of the scope around the
    // checker, and checkers are not elaborated: a use of the name says
    // which covergroup it is.
    let error = rejected("", "checker ck; covergroup cg; endgroup endchecker cg c;");
    assert_eq!(
        error,
        AnalyzerError::Unsupported("covergroup declared in a checker (IEEE 1800-2023 19.3)".into())
    );
    assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    analyze("module Top; checker ck; covergroup cg; endgroup endchecker endmodule").unwrap();
    // A covergroup of a loop generate block would be a type of each
    // iteration: it is not modelled, and a use of its name says so.
    let error = rejected(
        "",
        "for (genvar i = 0; i < 2; i++) begin : g covergroup cg; endgroup cg c; end",
    );
    assert_eq!(
        error,
        AnalyzerError::Unsupported(
            "covergroup `cg` declared in a loop generate block (IEEE 1800-2023 19.3, 27.4)".into()
        )
    );
    assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    // Unused, the declaration analyzes.
    analyze("module Top; for (genvar i = 0; i < 2; i++) begin : g covergroup cg; endgroup end endmodule")
        .unwrap();
}

#[test]
fn covergroups_bind_in_modules_with_interface_instances() {
    // A module with an interface instance binds its interface instances
    // before its other items, from a view of its declarations: the
    // covergroups are among them, so that what the view declares with
    // their types binds there as in any module. Here that is a variable of
    // the type that a connection of the instance reads, a function with a
    // formal of the type that a connection calls, and a structure with a
    // member of the type whose other member is connected.
    let ir = analyze(
        "interface I(input logic unmade); endinterface \
         module Top; covergroup cg(int a = 1); endgroup cg c; \
         I h(.unmade(c == null)); \
         initial begin c = new; c.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert!(top.signals().iter().any(|signal| signal.name() == "c"));
    assert_eq!(constructors(top).len(), 1);
    for source in [
        "interface I(input logic unmade); endinterface \
         module Top; covergroup cg; endgroup cg c; \
         function automatic bit isnull(cg h); return h == null; endfunction \
         I h(.unmade(isnull(c))); endmodule",
        "interface I(input logic [3:0] v); endinterface \
         module Top; covergroup cg; endgroup \
         struct { cg h; logic [3:0] n; } s; \
         I h(.v(s.n)); endmodule",
    ] {
        let ir = analyze(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert!(
            module(&ir, "Top")
                .signals()
                .iter()
                .any(|signal| signal.name() == "h$unmade" || signal.name() == "h$v"),
            "{source}"
        );
    }
}

#[test]
fn covergroup_formals_resolve_imported_names() {
    // The types and defaults of the formals of a covergroup are bound in
    // the scope that declares it: a name a wildcard import makes visible
    // there is imported for them too (26.3). A default is evaluated in that
    // scope (13.5.3), so the `a` of the default of `b` is the `a` of the
    // package, as that of `c` is, and not the formal before it.
    let ir = analyze(
        "package p; typedef logic [3:0] nibble_t; localparam int LIMIT = 7; \
         localparam int a = 9; endpackage \
         module Top; import p::*; \
         covergroup cg(nibble_t a = LIMIT, int b = a, int c = p::a) \
         with function sample(nibble_t s); endgroup \
         cg c; initial begin c = new; c.sample(4'd3); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert_eq!(constructors(top).len(), 1);
    let constructor = top
        .subroutines()
        .iter()
        .find(|subroutine| subroutine.name.ends_with("::$new"))
        .unwrap();
    let defaults: Vec<_> = constructor
        .params
        .iter()
        .map(|param| param.default.clone())
        .collect();
    assert!(defaults.iter().all(Option::is_some), "{defaults:?}");
    assert_eq!(defaults[1], defaults[2]);
    assert_ne!(defaults[0], defaults[1]);
    // A `let` of another design element is no `let` of this one.
    analyze(
        "module Other; let v = 1; int w = v; endmodule \
         module Top; int v; covergroup cg(int a = v); endgroup cg c; initial c = new; endmodule",
    )
    .unwrap();
}

#[test]
fn coverage_database_tasks_warn() {
    // 19.9: Celox keeps no coverage database, so the tasks that name or
    // load one do nothing but evaluate their argument, which a warning
    // says.
    let ir = analyze(
        "module Top; initial begin $set_coverage_db_name(\"db\"); \
         $load_coverage_db(\"db\"); end endmodule",
    )
    .unwrap();
    let tasks: Vec<&str> = ir
        .warnings()
        .iter()
        .filter_map(|warning| match warning {
            crate::ir::AnalyzerWarning::NoCoverageDatabase { task, site } => {
                assert!(site.is_some());
                Some(*task)
            }
            _ => None,
        })
        .collect();
    assert_eq!(tasks, ["$set_coverage_db_name", "$load_coverage_db"]);
    let ir = analyze("module Top; real r; initial r = $get_coverage(); endmodule").unwrap();
    assert!(ir.warnings().is_empty());
}

#[test]
fn covergroup_arguments_are_checked_as_those_of_a_subroutine() {
    // 19.3, 19.8.1, 13.5: the actuals of `new` and of `sample` bind to the
    // formals of the covergroup and of its sample method as those of any
    // subroutine do, and the type system checks them.
    let covergroup = "class K; endclass class L; endclass \
         covergroup cg(int a, K k = null) with function sample(int v, string s = \"d\"); endgroup \
         cg c; K k; L l; int i;";
    analyze(&format!(
        "module Top; {covergroup} initial begin k = new; c = new(1); c = new(.k(k), .a(2)); \
         c = new(3, k); c.sample(1); c.sample(.s(\"x\"), .v(2)); c.sample(3, \"y\"); end endmodule"
    ))
    .unwrap();
    for (items, detail) in [
        ("initial c = new;", "no actual for argument `a`"),
        ("initial c = new();", "no actual for argument `a`"),
        ("initial c = new(1, k, 2);", "3 actuals for 2 formals"),
        ("initial c = new(.b(1));", "no formal named `b`"),
        (
            "initial begin c = new(1); c.sample(); end",
            "no actual for argument `v`",
        ),
        (
            "initial begin c = new(1); c.sample(1, \"x\", 2); end",
            "3 actuals for 2 formals",
        ),
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        let AnalyzerError::InvalidCallArguments { detail: found, .. } = &error else {
            panic!("{items}: {error}");
        };
        assert!(found.contains(detail), "{items}: {found}");
    }
    // A class handle formal takes a handle of its class.
    let error = rejected("", &format!("{covergroup} initial c = new(1, l);"));
    assert!(
        matches!(error, AnalyzerError::IncompatibleClassHandle { .. }),
        "{error}"
    );
    let error = rejected("", &format!("{covergroup} initial c = new(1, i);"));
    assert!(
        matches!(error, AnalyzerError::NotClassHandle { .. }),
        "{error}"
    );
    // A covergroup without formals takes no actual.
    let error = rejected(
        "",
        "covergroup plain; endgroup plain p; initial begin p = new(1); end",
    );
    assert!(
        matches!(error, AnalyzerError::InvalidCallArguments { .. }),
        "{error}"
    );
}

#[test]
fn properties_of_scope_covergroup_types_are_ordinary_handles() {
    // 19.4 restricts the variable of an embedded covergroup to the
    // constructor of its class. A property a class declares with the type
    // of a covergroup of a package or module is a handle like any other:
    // assigned in any method and through any handle of the object.
    for source in [
        "package p; covergroup pcg; endgroup \
         class C; pcg h; pcg many[$]; \
         function void set(pcg x); h = x; many.push_back(x); endfunction \
         function void hit(); h.sample(); endfunction endclass endpackage \
         module Top; p::C c; p::pcg g; initial begin c = new; g = new; c.set(g); c.hit(); \
         c.h = g; c.h.sample(); c.h = null; end endmodule",
        "module Top; covergroup cg; endgroup \
         class C; cg h; function new(); h = new; endfunction \
         function void renew(); h = new; endfunction endclass \
         C c; initial begin c = new; c.h.sample(); c.renew(); c.h = null; end endmodule",
    ] {
        analyze(source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

/// The descriptor that module `module` registers for the covergroup type
/// shown as `display`.
fn descriptor(
    module: &crate::ir::Module,
    display: &str,
) -> std::sync::Arc<celox_heap::coverage::TypeDescriptor> {
    use crate::ir::{Callee, CoverageOp, Expr, ObjectOp, Stmt};
    let mut found = Vec::new();
    for subroutine in module
        .subroutines()
        .iter()
        .filter(|subroutine| subroutine.name.ends_with("::$type"))
    {
        for stmt in &subroutine.body {
            stmt.walk(&mut |stmt| {
                if let Stmt::Eval(Expr::Call {
                    callee: Callee::Object(op),
                    ..
                }) = stmt
                    && let ObjectOp::Coverage(CoverageOp::Register(descriptor)) = &**op
                {
                    found.push(descriptor.clone());
                }
            });
        }
    }
    found
        .into_iter()
        .find(|descriptor| descriptor.name == display)
        .unwrap_or_else(|| panic!("no descriptor of `{display}`"))
}

/// The coverage operations of the subroutine of `module` whose name ends
/// with `suffix`, in the order of its statements.
fn coverage_ops(module: &crate::ir::Module, suffix: &str) -> Vec<crate::ir::CoverageOp> {
    use crate::ir::{Callee, Expr, ObjectOp, Stmt};
    fn of_expr(expr: &Expr, ops: &mut Vec<crate::ir::CoverageOp>) {
        if let Expr::Call { callee, args, .. } = expr {
            for arg in args.iter().flatten() {
                of_expr(arg, ops);
            }
            if let Callee::Object(op) = callee
                && let ObjectOp::Coverage(op) = &**op
            {
                ops.push(op.clone());
            }
        }
    }
    let subroutine = module
        .subroutines()
        .iter()
        .find(|subroutine| subroutine.name.ends_with(suffix))
        .unwrap_or_else(|| panic!("no subroutine `{suffix}`"));
    let mut ops = Vec::new();
    for stmt in &subroutine.body {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::Eval(expr) | Stmt::Assign { rhs: expr, .. } => of_expr(expr, &mut ops),
            Stmt::If { condition, .. } => of_expr(condition, &mut ops),
            Stmt::SystemTask { args, .. } => {
                for arg in args {
                    if let crate::procedural::SystemTaskArg::Expr(expr) = arg {
                        of_expr(expr, &mut ops);
                    }
                }
            }
            _ => {}
        });
    }
    ops
}

/// The bin value warnings of `ir`: each with its coverpoint, its bin and
/// what it says.
fn bin_warnings(ir: &Ir) -> Vec<(String, String, String)> {
    ir.warnings()
        .iter()
        .filter_map(|warning| match warning {
            crate::ir::AnalyzerWarning::CovergroupBinValue {
                covergroup,
                item,
                bin,
                detail,
                site,
            } => {
                assert_eq!(covergroup, "Top.g1");
                assert!(site.is_some());
                Some((item.clone(), bin.clone(), detail.clone()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn coverpoints_compile_to_the_descriptor_of_the_type() {
    use celox_heap::coverage::{
        BinValues, Bound, InputDescriptor, Operand, OptionId, OptionValue, RangeDescriptor,
        ValueType,
    };
    // 19.5: the coverpoints with their names, types and bins, the options
    // of the definition and what a construction hands over.
    let ir = analyze(
        "module Top; typedef enum logic [1:0] { A, B = 2 } e_t; \
         logic [3:0] a; bit signed [7:0] s; e_t e; int unsigned lim; \
         covergroup cg(int hi, string label); \
         option.name = label; option.goal = 10 + 5; option.per_instance = 1; \
         coverpoint a { bins low = {0, [1:3]}; bins top = {[hi:$]}; option.weight = hi; } \
         neg: coverpoint s; \
         coverpoint e; \
         coverpoint a + 4'd1; \
         bit [1:0] typed: coverpoint s; \
         endgroup \
         cg c; initial begin c = new(2, \"x\"); c.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let descriptor = descriptor(top, "Top.cg");
    assert!(!descriptor.embedded);
    let points: Vec<_> = descriptor
        .points
        .iter()
        .map(|point| {
            (
                point.name.as_str(),
                point.generated_name,
                point.ty.width,
                point.ty.signed,
                point.ty.four_state,
                point.ty.members.len(),
                point.bins.len(),
            )
        })
        .collect();
    assert_eq!(
        points,
        [
            ("a", false, 4, false, true, 0, 2),
            ("neg", false, 8, true, false, 0, 0),
            ("e", false, 2, false, true, 2, 0),
            ("$point4", true, 4, false, true, 0, 0),
            ("typed", false, 2, false, false, 0, 0),
        ]
    );
    let members: Vec<_> = descriptor.points[2]
        .ty
        .members
        .iter()
        .map(|(name, value)| (name.as_str(), value.value.to_string()))
        .collect();
    assert_eq!(members, [("A", "0".to_string()), ("B", "2".to_string())]);
    // The constants are folded with their types; what reads an argument is
    // an input of the construction.
    let int = ValueType {
        width: 32,
        signed: true,
    };
    assert_eq!(
        descriptor.inputs,
        [
            InputDescriptor::String,
            InputDescriptor::Integral(int),
            InputDescriptor::Integral(int),
        ]
    );
    let constant = |operand: &Operand| match operand {
        Operand::Constant(constant) => (constant.ty, constant.value.to_string()),
        Operand::Input(index) => panic!("input {index}"),
    };
    let BinValues::Ranges { ranges, .. } = &descriptor.points[0].bins[0].values else {
        panic!("a bin of ranges");
    };
    let [
        RangeDescriptor::Value(zero),
        RangeDescriptor::Range { low, high },
    ] = ranges.as_slice()
    else {
        panic!("{ranges:?}");
    };
    assert_eq!(constant(zero), (int, "0".to_string()));
    let (Bound::Operand(low), Bound::Operand(high)) = (low, high) else {
        panic!("{ranges:?}");
    };
    assert_eq!(constant(low).1, "1");
    assert_eq!(constant(high).1, "3");
    let BinValues::Ranges { ranges, .. } = &descriptor.points[0].bins[1].values else {
        panic!("a bin of ranges");
    };
    assert_eq!(
        ranges.as_slice(),
        [RangeDescriptor::Range {
            low: Bound::Operand(Operand::Input(1)),
            high: Bound::Open
        }]
    );
    let options: Vec<_> = descriptor
        .options
        .iter()
        .map(|init| (init.item, init.option, &init.value))
        .collect();
    assert_eq!(options.len(), 4);
    assert_eq!(
        (options[0].0, options[0].1, options[0].2),
        (0, OptionId::Name, &OptionValue::StringInput(0))
    );
    assert!(
        matches!(options[1], (0, OptionId::Goal, OptionValue::Operand(operand)) if constant(operand).1 == "15")
    );
    assert!(matches!(
        options[2],
        (
            0,
            OptionId::PerInstance,
            OptionValue::Operand(Operand::Constant(_))
        )
    ));
    assert_eq!(
        (options[3].0, options[3].1, options[3].2),
        (
            1,
            OptionId::Weight,
            &OptionValue::Operand(Operand::Input(2))
        )
    );
    // No argument is read at a sample, so the instance keeps none.
    assert!(descriptor.formals.is_empty());
}

#[test]
fn samples_evaluate_before_they_hand_values_over() {
    use crate::ir::CoverageOp;
    // 19.5: the guard and the value of every coverpoint are evaluated
    // before the first value reaches the heap, in calls that follow each
    // other: coverpoints that fit 64 bits together share one, and one of
    // more than 64 bits has its own. The arguments of the covergroup that
    // a coverpoint reads are kept by the instance (19.3).
    let ir = analyze(
        "module Top; bit [31:0] a, b; bit [15:0] c; bit [99:0] w; bit en; \
         covergroup cg(int base, logic [70:0] wide, int unused) with function sample(bit [7:0] v); \
         coverpoint a; coverpoint b iff (en); coverpoint c; \
         coverpoint w iff (en); \
         coverpoint v + base[7:0]; coverpoint wide[70:64]; \
         endgroup \
         cg x; initial begin x = new(1, 71'd2, 3); x.sample(8'd4); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert_eq!(
        coverage_ops(top, "::$sample"),
        [
            CoverageOp::Sampling,
            CoverageOp::Formal {
                slot: 0,
                width: 32,
                signed: true,
                four_state: false
            },
            CoverageOp::Formal {
                slot: 1,
                width: 71,
                signed: false,
                four_state: true
            },
            CoverageOp::Points {
                first: 1,
                count: 2,
                width: 64
            },
            CoverageOp::Points {
                first: 3,
                count: 1,
                width: 16
            },
            CoverageOp::Point {
                item: 4,
                width: 100,
                signed: false
            },
            CoverageOp::Points {
                first: 5,
                count: 2,
                width: 15
            },
            CoverageOp::SampleEnd,
        ]
    );
    let new = coverage_ops(top, "::$new");
    assert_eq!(
        new,
        [
            CoverageOp::New {
                class: match &new[0] {
                    CoverageOp::New { class, .. } => *class,
                    other => panic!("{other:?}"),
                },
                size: 0,
                handles: Vec::new()
            },
            CoverageOp::FormalSet {
                slot: 0,
                width: 32,
                signed: true
            },
            CoverageOp::FormalSet {
                slot: 1,
                width: 71,
                signed: false
            },
            CoverageOp::Build,
        ]
    );
    assert_eq!(
        descriptor(top, "Top.cg")
            .formals
            .iter()
            .map(|formal| (formal.width, formal.signed))
            .collect::<Vec<_>>(),
        [(32, true), (71, false)]
    );
    // A string or a class handle a coverpoint reads is a property of the
    // object of the instance, which the collector finds.
    let ir = analyze(
        "class K; bit [1:0] v; endclass \
         module Top; covergroup cg(string s, K k, int n); \
         coverpoint s.len() + n; coverpoint k.v; endgroup \
         cg x; K k; initial begin k = new; x = new(\"ab\", k, 1); x.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let new = coverage_ops(top, "::$new");
    let CoverageOp::New { size, handles, .. } = &new[0] else {
        panic!("{new:?}");
    };
    assert_eq!(*size, 16);
    assert_eq!(
        handles.as_slice(),
        [
            (0, crate::ir::HandleProperty::String),
            (8, crate::ir::HandleProperty::Object)
        ]
    );
    assert_eq!(descriptor(top, "Top.cg").formals.len(), 1);
}

#[test]
fn bin_values_outside_the_coverpoint_type_warn() {
    // 19.5.7: the examples of the standard. For `b1` the range `[6:10]`,
    // for `b2` the range `[1:10]` and the values -1 and 15, for `b3` the
    // ranges `[2:5]` and `[6:10]`, for `b4` the range `[1:10]` and 15.
    let ir = analyze(
        "module Top; bit [2:0] p1; bit signed [2:0] p2; \
         covergroup g1; \
         coverpoint p1 { bins b1 = { 1, [2:5], [6:10]}; bins b2 = {-1, [1:10], 15}; } \
         coverpoint p2 { bins b3 = { 1, [2:5], [6:10]}; bins b4 = {-1, [1:10], 15}; } \
         endgroup \
         g1 g; initial g = new; endmodule",
    )
    .unwrap();
    let warnings = bin_warnings(&ir);
    let of = |bin: &str| {
        warnings
            .iter()
            .filter(|(_, found, _)| found == bin)
            .map(|(item, _, detail)| (item.as_str(), detail.as_str()))
            .collect::<Vec<_>>()
    };
    let limited = |range: &str| {
        format!(
            "range {range} is limited to the values of the coverpoint type (IEEE 1800-2023 19.5.7)"
        )
    };
    let dropped = |value: &str| {
        format!(
            "value {value} is not a value of the coverpoint type and does not take part (IEEE 1800-2023 19.5.7)"
        )
    };
    assert_eq!(of("b1"), [("p1", limited("[6:10]").as_str())]);
    assert_eq!(
        of("b2"),
        [
            ("p1", dropped("-1").as_str()),
            ("p1", limited("[1:10]").as_str()),
            ("p1", dropped("15").as_str())
        ]
    );
    assert_eq!(
        of("b3"),
        [
            ("p2", limited("[2:5]").as_str()),
            (
                "p2",
                "range [6:10] holds no value of the coverpoint type and does not take part (IEEE 1800-2023 19.5.7)"
            )
        ]
    );
    assert_eq!(
        of("b4"),
        [
            ("p2", limited("[1:10]").as_str()),
            ("p2", dropped("15").as_str())
        ]
    );
    // The type has the warnings of its constants at each analysis, and no
    // warning at a construction: nothing of it is known only then.
    assert!(!coverage_ops(module(&ir, "Top"), "::$new").contains(&crate::ir::CoverageOp::Message));
    // A value an argument gives is known at the construction, which then
    // warns (see `input_dependent_bin_warns_at_run_time`).
    let ir = analyze(
        "module Top; bit [2:0] p1; covergroup g1(int hi); coverpoint p1 { bins b = {[1:hi]}; } \
         endgroup g1 g; initial g = new(9); endmodule",
    )
    .unwrap();
    assert!(bin_warnings(&ir).is_empty());
    assert!(coverage_ops(module(&ir, "Top"), "::$new").contains(&crate::ir::CoverageOp::Message));
    // Such a construction does not warn again of the constants of the
    // type: the descriptor has the values of a constant element that take
    // part, as constants of the type of the coverpoint, in place of an
    // element the analysis warned of. Of `{15, [6:10], -1, [$:20]}` on
    // `bit [2:0]` they are `[6:7]` and `[0:7]`; on `bit signed [2:0]`
    // `[-2:9]` is `[-2:3]`.
    let ir = analyze(
        "module Top; bit [2:0] p1; bit signed [2:0] p2; covergroup g1(int hi); \
         coverpoint p1 { bins a = {15, [6:10], 3, -1, [$:20]}; bins b = {[1:hi]}; } \
         coverpoint p2 { bins c = {[-2:9]}; } \
         endgroup g1 g; initial g = new(9); endmodule",
    )
    .unwrap();
    assert_eq!(
        bin_warnings(&ir)
            .iter()
            .map(|(item, bin, _)| (item.as_str(), bin.as_str()))
            .collect::<Vec<_>>(),
        [
            ("p1", "a"),
            ("p1", "a"),
            ("p1", "a"),
            ("p1", "a"),
            ("p2", "c")
        ]
    );
    assert!(coverage_ops(module(&ir, "Top"), "::$new").contains(&crate::ir::CoverageOp::Message));
    {
        use celox_heap::coverage::{
            BinValues, Bound, Operand, RangeDescriptor, RangeValue, ResolvedItem, resolve,
        };
        let descriptor = descriptor(module(&ir, "Top"), "Top.g1");
        // Each element of the bins `bin` of coverpoint `point` with the
        // type of its first operand, and what it resolves to.
        let elements = |point: usize, bin: usize| {
            let point = &descriptor.points[point];
            let BinValues::Ranges { ranges, .. } = &point.bins[bin].values else {
                panic!("a bin of ranges");
            };
            ranges
                .iter()
                .map(|range| {
                    let constant = |operand: &Operand| match operand {
                        Operand::Constant(constant) => constant.clone(),
                        Operand::Input(index) => panic!("input {index}"),
                    };
                    let (ty, resolved) = match range {
                        RangeDescriptor::Value(value) => {
                            let value = constant(value);
                            (value.ty, resolve(&point.ty, RangeValue::Value(&value)))
                        }
                        RangeDescriptor::Range {
                            low: Bound::Operand(low),
                            high: Bound::Operand(high),
                        } => {
                            let (low, high) = (constant(low), constant(high));
                            let value = RangeValue::Range {
                                low: Some(&low),
                                high: Some(&high),
                            };
                            (low.ty, resolve(&point.ty, value))
                        }
                        range => panic!("{range:?}"),
                    };
                    assert_eq!(resolved.warnings, [] as [String; 0]);
                    let [ResolvedItem::Range { low, high }] = resolved.items.as_slice() else {
                        panic!("{resolved:?}");
                    };
                    ((ty.width, ty.signed), low.to_string(), high.to_string())
                })
                .collect::<Vec<_>>()
        };
        let range = |ty, low: &str, high: &str| (ty, low.to_string(), high.to_string());
        // The element that fits, `3`, stays as it is written: an `int`.
        assert_eq!(
            elements(0, 0),
            [
                range((3, false), "6", "7"),
                range((32, true), "3", "3"),
                range((3, false), "0", "7"),
            ]
        );
        assert_eq!(elements(1, 0), [range((3, true), "-2", "3")]);
    }
    // A covergroup that nothing constructs has no warning: its bins are
    // not compiled.
    let ir = analyze(
        "module Top; bit [2:0] p1; covergroup g1; coverpoint p1 { bins b = {15}; } endgroup \
         endmodule",
    )
    .unwrap();
    assert!(bin_warnings(&ir).is_empty());
}

#[test]
fn covergroup_items_the_standard_excludes_are_errors() {
    // What the declaration alone shows is an error where the covergroup is
    // declared, whether or not something uses it.
    for (declaration, rule) in [
        // 19.7: one definition assigns an option once.
        (
            "bit x; covergroup cg; coverpoint x { option.weight = 1; option.weight = 2; } endgroup",
            "`option.weight` is assigned twice in coverpoint `x`",
        ),
        (
            "covergroup cg; option.goal = 1; option.per_instance = 1; option.goal = 2; endgroup",
            "`option.goal` is assigned twice in covergroup `Top.cg`",
        ),
        (
            "covergroup cg; type_option.goal = 1; type_option.goal = 2; endgroup",
            "`type_option.goal` is assigned twice",
        ),
        // 19.10: the members of `option` and `type_option`, by level.
        (
            "covergroup cg; option.wieght = 1; endgroup",
            "`option` has no member `wieght`",
        ),
        (
            "covergroup cg; type_option.per_instance = 1; endgroup",
            "`type_option` has no member `per_instance`",
        ),
        (
            "bit x; covergroup cg; coverpoint x { option.per_instance = 1; } endgroup",
            "`option.per_instance` is assigned in a coverpoint",
        ),
        (
            "bit x; covergroup cg; coverpoint x { option.name = \"n\"; } endgroup",
            "`option.name` is assigned in a coverpoint",
        ),
        (
            "bit x; covergroup cg; coverpoint x { type_option.strobe = 1; } endgroup",
            "`type_option.strobe` is assigned in a coverpoint",
        ),
        // 19.5: the items of a covergroup have one name each.
        (
            "bit x, y; covergroup cg; coverpoint x; x: coverpoint y; endgroup",
            "two items named `x`",
        ),
        (
            "bit x, y; covergroup cg; a: coverpoint x; a: cross x, y; endgroup",
            "two items named `a`",
        ),
        // 19.8.1: an argument of `sample` is visible in coverpoint
        // expressions and guards only.
        (
            "covergroup cg with function sample(int v, bit b); coverpoint v; \
             option.per_instance = b; endgroup",
            "argument `b` of the sample method of covergroup `Top.cg` is used in an option value",
        ),
        (
            "bit [3:0] x; covergroup cg with function sample(int v); \
             coverpoint x { bins b = {[0:v]}; } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        (
            "bit [3:0] x; covergroup cg with function sample(int v); \
             coverpoint x { bins b[v] = {[0:3]}; } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        (
            "bit [3:0] x; covergroup cg with function sample(int v); \
             coverpoint x { bins b = {[0:7]} with (item < v); } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        (
            "bit [3:0] x; covergroup cg with function sample(int v); \
             coverpoint x { bins b = {[4 +/- v]}; } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        // In a `with` expression `item` is the value of the filter
        // (19.5.1.1), but another argument of `sample` stays an error, and
        // `item` outside the `with` expression is the argument.
        (
            "covergroup cg with function sample(int item, int v); \
             coverpoint item { bins b[] = {[0:3]} with (item > v); } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        (
            "covergroup cg with function sample(int item); \
             coverpoint item { bins b[item] = {[0:3]}; } endgroup",
            "argument `item` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        (
            "covergroup cg with function sample(int item); \
             coverpoint item { bins b = {[0:item]} with (item > 1); } endgroup",
            "argument `item` of the sample method of covergroup `Top.cg` is used in a bin",
        ),
        // 19.5: a coverpoint is a scope, which names each of its bins
        // once.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins a[] = {[0:3]}; ignore_bins a = {2}; } \
             endgroup",
            "coverpoint `x` of covergroup `Top.cg` has two bins named `a`",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins a = {1}; bins a = {2}; } endgroup",
            "coverpoint `x` of covergroup `Top.cg` has two bins named `a`",
        ),
        // 19.5: "A default bin cannot be explicitly ignored".
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {1}; ignore_bins rest = default; } \
             endgroup",
            "`ignore_bins rest` of coverpoint `x` of covergroup `Top.cg` is a default bin",
        ),
        // 19.5.1.1: before `with`, only the name of the coverpoint of the
        // bin stands for all its values.
        (
            "bit [3:0] x, y; covergroup cg; coverpoint x; \
             coverpoint y { bins b = x with (item > 2); } endgroup",
            "bin `b` of coverpoint `y` of covergroup `Top.cg` takes the values of `x`",
        ),
        (
            "bit [3:0] x; covergroup cg; a: coverpoint x { bins b = x with (item > 2); } endgroup",
            "bin `b` of coverpoint `a` of covergroup `Top.cg` takes the values of `x`",
        ),
    ] {
        for used in ["", "cg c; initial c = new;"] {
            let error = rejected("", &format!("{declaration} {used}"));
            let AnalyzerError::InvalidCovergroup(text) = &error else {
                panic!("{declaration}: {error}");
            };
            assert!(text.contains(rule), "{declaration}: {text}");
            assert!(text.contains("IEEE 1800-2023 19."), "{declaration}: {text}");
            assert!(!error.is_unsupported());
        }
    }
    // What the standard allows: the weight of the covergroup beside those
    // of its coverpoints (the example of 19.5), `option.weight` beside
    // `type_option.weight` in one coverpoint (that of 19.7.1), an item
    // named as a variable of the scope, and two unnamed coverpoints of
    // expressions.
    for declaration in [
        "bit x, y; covergroup cg(int c); option.weight = c; \
         a: coverpoint x { option.weight = 2; } b: coverpoint y { option.weight = 3; } endgroup",
        "bit x; covergroup cg; coverpoint x { option.weight = 2; type_option.weight = 3; } \
         endgroup",
        "bit [1:0] x, y; covergroup cg; y: coverpoint x; coverpoint x + 1; coverpoint x + 2; \
         endgroup",
    ] {
        analyze(&format!("module Top; {declaration} cg c; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
    }
    // 19.5.1.1 beside 19.8.1: `item` of a `with` expression is the value of
    // the filter, also where `sample` has an argument of that name.
    analyze(
        "module Top; covergroup cg with function sample(int item); \
         cp: coverpoint item { bins b[] = {[0:3]} with (item > 1); } endgroup \
         cg c; initial begin c = new; c.sample(2); end endmodule",
    )
    .unwrap();
}

#[test]
fn covergroup_expressions_the_standard_excludes_are_errors() {
    // What needs the names of the scope is an error where the subroutines
    // of the type are made: where the type is used.
    for (declaration, rule) in [
        // 19.5: a bin names constants and arguments of the covergroup.
        (
            "bit [3:0] x; int limit; covergroup cg; coverpoint x { bins b = {[0:limit]}; } \
             endgroup",
            "`limit` in bin `b` of coverpoint `x` of covergroup `Top.cg` is neither a constant nor \
             an argument of the covergroup",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {x}; } endgroup",
            "`x` in bin `b`",
        ),
        // A function of a bin only reads its arguments.
        (
            "bit [3:0] x; function automatic int f(output int o); o = 1; return 2; endfunction \
             covergroup cg(int a = 1); coverpoint x { bins b = {f(a)}; } endgroup",
            "the function called in bin `b` of coverpoint `x` of covergroup `Top.cg` has an \
             output, inout or ref argument",
        ),
        // Of the system functions only a constant call.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {$urandom}; } endgroup",
            "the call of `$urandom` in bin `b` of coverpoint `x` of covergroup `Top.cg` is not a \
             constant system function call",
        ),
        // 19.5: a coverpoint is of an integral type, and its expression is
        // assignment compatible with the type it declares.
        (
            "string s; covergroup cg; coverpoint s; endgroup",
            "coverpoint `s` of covergroup `Top.cg` is not of an integral type",
        ),
    ] {
        analyze(&format!("module Top; {declaration} cg c; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        let error = rejected("", &format!("{declaration} cg c; initial c = new;"));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{declaration}: {error}");
        };
        let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.contains(&rule), "{declaration}: {text}");
        assert!(
            text.contains("(IEEE 1800-2023 19.5)"),
            "{declaration}: {text}"
        );
    }
    // The type system rejects what a coverpoint type does not take.
    let error = rejected(
        "",
        "typedef enum { A, B } e_t; int v; covergroup cg; e_t k: coverpoint v; endgroup \
         cg c; initial c = new;",
    );
    assert!(
        matches!(error, AnalyzerError::IncompatibleAssignment { .. }),
        "{error}"
    );
    // An option value is no such expression: it reads what the scope of
    // `new` has (19.7). Nor is a constant a variable: a parameter, a
    // member of an enumeration, a constant of a package, a variable in the
    // argument of a constant system function, and a constant of the class
    // of an embedded covergroup (19.4).
    analyze(
        "package p; localparam int PL = 3; endpackage \
         module Top #(parameter int P = 2); import p::*; typedef enum { E0, E1 } e_t; \
         localparam bit [3:0] L = 4'd9; bit [3:0] x; int weight; \
         covergroup cg(int a); option.weight = weight; \
         coverpoint x { bins b = {P, L, E1, PL, p::PL, a, $bits(x), [a + P:$clog2(L)]}; } \
         endgroup \
         cg c; initial c = new(1); endmodule \
         class K; const int limit = 4; static int shared = 2; bit [3:0] v; int plain; \
         covergroup cg; option.goal = plain; coverpoint v { bins b = {limit, shared}; } endgroup \
         function new(); cg = new; endfunction endclass",
    )
    .unwrap();
    // The same constants selected of their scope, of the object and of
    // its base class, a method of the object called as a member of it,
    // and the member of an argument of the covergroup. A member of
    // unpacked structure type of the class is not told apart from a
    // constant, as a static member is not.
    analyze(
        "package p; localparam int PL = 3; typedef enum { P0, P1 } e_t; \
         localparam real R = 2.5; typedef struct packed { logic [7:0] depth; } cfg_t; \
         localparam cfg_t CFG = '{depth: 4}; \
         function automatic int twice(int v); return 2 * v; endfunction endpackage \
         class H; int x; endclass \
         class B; const int low = 1; endclass \
         class K extends B; const int limit = 4; static int shared = 2; bit [3:0] v; \
         typedef struct { int f; } s_t; s_t s; \
         function int top(); return 9; endfunction \
         covergroup cg(H h); \
         coverpoint v { bins b = {this.limit, super.low, K::shared, this.shared, p::PL, p::P1, \
         p::twice(2), this.top(), h.x, s.f, this.s.f, int'(p::R), p::CFG.depth}; } endgroup \
         function new(H h); cg = new(h); endfunction endclass \
         module Top; K k; H h; initial begin h = new; k = new(h); end endmodule",
    )
    .unwrap();
    let error = analyze(
        "class K; bit [3:0] v; int plain; \
         covergroup cg; coverpoint v { bins b = {plain}; } endgroup \
         function new(); cg = new; endfunction endclass \
         module Top; K k; initial k = new; endmodule",
    )
    .unwrap_err()
    .into_error();
    assert!(
        matches!(&error, AnalyzerError::InvalidCovergroup(text) if text.contains("`plain` in bin `b`")),
        "{error}"
    );
    // A variable is one however it is named: selected of its package, of
    // the object, of the base class of the object or of an instance, and
    // read for the object a method is called of.
    let embedded = |members: &str, operand: &str| {
        format!(
            "class H; function int get(); return 1; endfunction endclass \
             class B; int x; endclass \
             class K extends B; bit [3:0] v; int plain; H held; {members} \
             covergroup cg; coverpoint v {{ bins b = {{{operand}}}; }} endgroup \
             function new(); cg = new; endfunction endclass \
             module Top; K k; initial k = new; endmodule"
        )
    };
    let scoped = |operand: &str| {
        format!(
            "package p; int v; endpackage \
             module Sub; int v; endmodule \
             module Top; bit [3:0] x; Sub u(); \
             covergroup cg; coverpoint x {{ bins b = {{{operand}}}; }} endgroup \
             cg c; initial c = new; endmodule"
        )
    };
    for (source, name) in [
        (scoped("p::v"), "`p::v`"),
        (scoped("[0:p::v + 1]"), "`p::v`"),
        (scoped("u.v"), "`u`"),
        (embedded("", "this.plain"), "`this.plain`"),
        (embedded("", "super.x"), "`super.x`"),
        (embedded("", "[this.plain:$]"), "`this.plain`"),
        (embedded("", "this.held.get()"), "`this.held`"),
        (embedded("", "held.get()"), "`held`"),
    ] {
        let error = analyze(&source).unwrap_err().into_error();
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{source}: {error}");
        };
        assert!(
            text.contains(&format!(
                "{name} in bin `b` of coverpoint `{}` of covergroup",
                if source.contains("class K") { "v" } else { "x" }
            )) && text.contains("is neither a constant nor an argument of the covergroup")
                && text.contains("(IEEE 1800-2023 19.5)"),
            "{source}: {text}"
        );
    }
}

#[test]
fn unsupported_coverpoint_constructs_are_rejected_where_used() {
    // What the analysis does not support of a coverpoint, a bin and an
    // option is rejected where the type is used, with a text that maps to
    // the tracking issue of covergroups; the declaration alone analyzes.
    let constructs = [
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {1}; bins others[2] = default; } \
             endgroup",
            "covergroup default bin array with a size (IEEE 1800-2023 19.5)",
        ),
        // A construction is asked about no more than 1,048,576 values of
        // a bin; only the filter of one `bins` bin is evaluated at each
        // sample instead.
        (
            "int x; covergroup cg; coverpoint x { ignore_bins b = {[0:2000000]} with (item % 2 \
             == 0); } endgroup",
            "covergroup with filter over more than 1048576 candidate values on a bin array, \
             `ignore_bins` or `illegal_bins`, or over a wildcard value of more than 65536 ranges \
             of values (IEEE 1800-2023 19.5.1.1)",
        ),
        (
            "int x; covergroup cg; coverpoint x { illegal_bins b = x with (item < 0); } endgroup",
            "covergroup with filter over more than 1048576 candidate values on a bin array, \
             `ignore_bins` or `illegal_bins`, or over a wildcard value of more than 65536 ranges \
             of values (IEEE 1800-2023 19.5.1.1)",
        ),
        (
            "int x; covergroup cg; coverpoint x { bins b[] = x with (item < 4); } endgroup",
            "covergroup with filter over more than 1048576 candidate values on a bin array, \
             `ignore_bins` or `illegal_bins`, or over a wildcard value of more than 65536 ranges \
             of values (IEEE 1800-2023 19.5.1.1)",
        ),
        // A wildcard value of more ranges of values than are listed is
        // matched as a pattern, which only one bin may have.
        (
            "longint x; covergroup cg; coverpoint x { wildcard bins b[] = \
             {64'h???????????????1}; } endgroup",
            "covergroup wildcard value of more than 65536 ranges of values in a bin array or a \
             bin with a filter (IEEE 1800-2023 19.5.4)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {[4 +/- 0.5]}; } endgroup",
            "covergroup bin range with a tolerance that is not integral (IEEE 1800-2023 19.5.1, \
             11.4.13)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = '{1, 2}; } endgroup",
            "covergroup bins of a set expression that is neither a dynamic array, a queue or an \
             array of an object with integral elements of at most 64 bits nor an array parameter \
             with integral elements (IEEE 1800-2023 19.5.1.2)",
        ),
        // The values of a wildcard transition value matched as a pattern
        // are not listed: no bin array has one, and no ignored or illegal
        // transition can be taken from it.
        (
            "longint x; covergroup cg; coverpoint x { wildcard bins b[] = \
             (64'h???????????????1 => 0); } endgroup",
            "covergroup wildcard value of more than 65536 ranges of values in a bin array or a \
             bin with a filter (IEEE 1800-2023 19.5.4)",
        ),
        (
            "longint x; covergroup cg; coverpoint x { wildcard bins b = \
             (64'h???????????????1 => 0); ignore_bins i = (1 => 2); } endgroup",
            "covergroup ignored or illegal transition beside a wildcard transition value of more \
             than 65536 ranges of values (IEEE 1800-2023 19.5.4, 19.5.5, 19.5.6)",
        ),
        // A repetition of no sample is an empty sequence (16.9.2.1), and a
        // count of 65536 samples or more is beyond the states of the
        // transitions of a coverpoint.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2 [* 0]); } endgroup",
            "covergroup transition repetition of no sample (IEEE 1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2 [* 0:1] => 3); } endgroup",
            "covergroup transition repetition of no sample (IEEE 1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2 [* 70000]); } endgroup",
            "covergroup transition repetition count above 65535, beyond the 65536 states the \
             transitions of a coverpoint may have (IEEE 1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; \
             coverpoint x { bins b = (1 => 2 [* 68'h1_0000_0000_0000_0003]); } endgroup",
            "covergroup transition repetition count above 65535, beyond the 65536 states the \
             transitions of a coverpoint may have (IEEE 1800-2023 19.5.2)",
        ),
        // `(e)` that may be an array takes the path of a set expression.
        (
            "bit [3:0] x; localparam bit [3:0] S [3] = '{1, 2, 3}; \
             localparam bit [3:0] T [2] = '{4, 5}; localparam bit SEL = 1; \
             covergroup cg; coverpoint x { bins a[] = (SEL ? S : T); } endgroup",
            "covergroup bins of a set expression that is neither a dynamic array, a queue or an \
             array of an object with integral elements of at most 64 bits nor an array parameter \
             with integral elements (IEEE 1800-2023 19.5.1.2)",
        ),
        (
            "covergroup cg; type_option.real_interval = 2.0; endgroup",
            "covergroup type_option.real_interval (IEEE 1800-2023 19.7.1)",
        ),
        (
            "bit x; covergroup cg; coverpoint x { type_option.real_interval = 0.5; } endgroup",
            "covergroup type_option.real_interval (IEEE 1800-2023 19.7.1)",
        ),
        (
            "parameter string S = \"s\"; covergroup cg; type_option.comment = S; endgroup",
            "covergroup type_option.comment that is no string literal (IEEE 1800-2023 19.7.1)",
        ),
        (
            "real r; covergroup cg; coverpoint r; endgroup",
            "covergroup real coverpoint (IEEE 1800-2023 19.5)",
        ),
        (
            "typedef enum logic [1:0] { A, B = 2'b1x } e_t; e_t e; \
             covergroup cg; coverpoint e; endgroup",
            "covergroup coverpoint of an enumeration whose members have no known values (IEEE \
             1800-2023 19.5.3)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {1.5}; } endgroup",
            "covergroup bin value that is not integral (IEEE 1800-2023 19.5.1)",
        ),
        (
            "bit [3:0] x; covergroup cg(real r); coverpoint x + int'(r); endgroup",
            "covergroup argument `r` of a type that is not integral, a string or a class, read at \
             a sample (IEEE 1800-2023 19.3)",
        ),
        // A `let` is not expanded after the name of a covergroup: one a
        // coverpoint expression, a guard, a bin or an option value names
        // is rejected, unless an argument has its name.
        (
            "bit [3:0] x; let twice(v) = v + v; covergroup cg; coverpoint twice(x); endgroup",
            "covergroup expression that names `let` `twice` (IEEE 1800-2023 11.12, 19.5)",
        ),
        (
            "bit [3:0] x; let on = 1'b1; covergroup cg; coverpoint x iff (on); endgroup",
            "covergroup expression that names `let` `on` (IEEE 1800-2023 11.12, 19.5)",
        ),
        (
            "bit [3:0] x; let top = 4; covergroup cg; coverpoint x { bins b = {[0:top]}; } \
             endgroup",
            "covergroup expression that names `let` `top` (IEEE 1800-2023 11.12, 19.5)",
        ),
        (
            "let w = 2; covergroup cg; option.weight = w; endgroup",
            "covergroup expression that names `let` `w` (IEEE 1800-2023 11.12, 19.5)",
        ),
    ];
    for (declaration, construct) in constructs {
        let construct = construct.split_whitespace().collect::<Vec<_>>().join(" ");
        for unused in ["", "cg c;"] {
            let source = format!("module Top; {declaration} {unused} endmodule");
            analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
        }
        let arguments = if declaration.contains("cg(real r)") {
            "(1.0)"
        } else {
            ""
        };
        for used in [
            format!("cg c = new{arguments};"),
            format!("cg c; initial c = new{arguments};"),
            "cg c; initial c.sample();".to_string(),
            "real r0; initial r0 = cg::get_coverage();".to_string(),
        ] {
            let error = rejected("", &format!("{declaration} {used}"));
            assert_eq!(
                error,
                AnalyzerError::Unsupported(construct.clone()),
                "{declaration} {used}"
            );
            assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
        }
    }
    // An argument of the covergroup or of its sample method hides a `let`
    // of its name in the items (19.8.1).
    analyze(
        "module Top; bit [3:0] x; let top = 4; let v = 1; \
         covergroup cg(int top) with function sample(bit [3:0] v); \
         coverpoint v { bins b = {[0:top]}; } endgroup \
         cg c; initial begin c = new(3); c.sample(x); end endmodule",
    )
    .unwrap();
    // What a package covergroup that nothing of the package uses does not
    // support is rejected where a module uses the type.
    let package = "package p; real r; covergroup pcg; coverpoint r; endgroup \
         covergroup fine(int a); coverpoint a; endgroup endpackage";
    analyze(&format!(
        "{package} module Top; p::fine f; initial begin f = new(1); f.sample(); end endmodule"
    ))
    .unwrap();
    let error = rejected(package, "p::pcg c; initial c = new;");
    assert_eq!(
        error,
        AnalyzerError::Unsupported("covergroup real coverpoint (IEEE 1800-2023 19.5)".to_string())
    );
    assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
}

#[test]
fn coverpoint_of_a_clocking_block_signal_is_rejected() {
    // 19.5 allows a signal of a clocking block as a coverpoint, a clockvar
    // whose type a covergroup expression does not yet take.
    let error = rejected(
        "",
        "bit clk; bit [1:0] d; clocking cb @(posedge clk); input d; endclocking \
         covergroup cg; coverpoint cb.d; endgroup cg c; initial c = new;",
    );
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn covergroup_items_resolve_imported_names() {
    // The expressions of the items of a covergroup are bound in the scope
    // that declares it, after its arguments: a name a wildcard import makes
    // visible there is imported for them too (26.3), in a coverpoint
    // expression, a guard, a bin, an option value and a coverpoint type.
    // A name an argument has is the argument, and imports nothing.
    let ir = analyze(
        "package p; typedef bit [3:0] nibble_t; localparam int LIMIT = 7; \
         localparam int WEIGHT = 3; localparam bit ON = 1; \
         function automatic bit [3:0] half(bit [3:0] v); return v / 2; endfunction \
         int hidden = 1; endpackage \
         module Top; import p::*; bit [3:0] x; \
         covergroup cg(int hidden); \
         option.weight = WEIGHT; \
         nibble_t typed: coverpoint half(x) iff (ON) { bins b = {[0:LIMIT]}; bins h = {hidden}; } \
         endgroup \
         cg c; initial begin c = new(2); c.sample(); end endmodule",
    )
    .unwrap();
    let descriptor = descriptor(module(&ir, "Top"), "Top.cg");
    assert_eq!(descriptor.points[0].ty.width, 4);
    assert_eq!(descriptor.inputs.len(), 1);
    // A local declaration after the covergroup that a name of it would
    // have imported is no error for a name an argument hides (26.3).
    analyze(
        "package p; int hidden = 1; endpackage \
         module Top; import p::*; bit [3:0] x; \
         covergroup cg(int hidden); coverpoint x { bins h = {hidden}; } endgroup \
         int hidden; \
         cg c; initial c = new(2); endmodule",
    )
    .unwrap();
}

/// The bin of `point` named `name`.
fn bin<'a>(
    point: &'a celox_heap::coverage::PointDescriptor,
    name: &str,
) -> &'a celox_heap::coverage::BinDescriptor {
    point
        .bins
        .iter()
        .find(|bin| bin.name == name)
        .unwrap_or_else(|| panic!("no bin `{name}` of `{}`", point.name))
}

/// The range list of `bin` as the values and bounds of its constants, with
/// `$` for an open bound, and its filter.
fn constant_ranges(
    bin: &celox_heap::coverage::BinDescriptor,
) -> (Vec<String>, Option<celox_heap::coverage::BinFilter>) {
    use celox_heap::coverage::{BinValues, Bound, Operand, RangeDescriptor};
    let BinValues::Ranges { ranges, filter } = &bin.values else {
        panic!("bin `{}` is no bin of ranges: {:?}", bin.name, bin.values);
    };
    let operand = |operand: &Operand| match operand {
        Operand::Constant(constant) if constant.mask.bits() != 0 => {
            format!("{:b}/{:b}", constant.value, constant.mask)
        }
        // A signed constant reads as its two's complement.
        Operand::Constant(constant)
            if constant.ty.signed && constant.value.bit(u64::from(constant.ty.width) - 1) =>
        {
            let modulus = num_bigint::BigInt::from(1) << constant.ty.width;
            (num_bigint::BigInt::from(constant.value.clone()) - modulus).to_string()
        }
        Operand::Constant(constant) => constant.value.to_string(),
        Operand::Input(index) => format!("input{index}"),
    };
    let bound = |bound: &Bound| match bound {
        Bound::Open => "$".to_string(),
        Bound::Operand(value) => operand(value),
    };
    (
        ranges
            .iter()
            .map(|range| match range {
                RangeDescriptor::Value(value) => operand(value),
                RangeDescriptor::Range { low, high } => format!("[{}:{}]", bound(low), bound(high)),
            })
            .collect(),
        *filter,
    )
}

#[test]
fn bin_kinds_compile_to_the_descriptor_of_the_type() {
    use celox_heap::coverage::{
        BinArray, BinFilter, BinKind, BinValues, InputDescriptor, Operand, ValueType,
    };
    // 19.5.1 to 19.5.6: the arrays, kinds, guards, filters and values of
    // the bins of a definition, and what a construction and a sample do
    // for them.
    let ir = analyze(
        "module Top; logic [3:0] a; bit [7:0] b; bit en; longint big; \
         localparam int S [3] = '{5, 1, 9}; typedef int q_t[$]; \
         function automatic bit keep(bit [7:0] v); return v[0]; endfunction \
         function automatic q_t made(int n); q_t q; q.push_back(n); return q; endfunction \
         covergroup cg(int n, int lim); \
         coverpoint a { \
           bins one[] = {[0:3]}; \
           bins some[n] = {[4:11]}; \
           bins two[2] = {12, 13} iff (en); \
           wildcard bins w = {4'b1?0?}; \
           ignore_bins ig = {14}; \
           illegal_bins bad = {15} iff (!en); \
           bins rest[] = default; \
         } \
         coverpoint b { \
           bins mod3[] = {[0:255]} with (item % 3 == 0); \
           bins f = b with (keep(item) && item < lim); \
           bins set[] = S; \
           bins q = made(n); \
           bins tol = {[100 +/- 5], [50 +%- 10]}; \
           bins near = {[lim +/- 1]}; \
           illegal_bins other = default; \
         } \
         coverpoint big { \
           bins even = big with (item % 2 == 0 && item > lim) iff (en); \
           bins low = {[0:9]}; \
           bins neg = {[-7 +%- 25], [-20 +/- -3]}; \
         } \
         endgroup \
         cg c; initial begin c = new(2, 100); c.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let descriptor = descriptor(top, "Top.cg");
    let [a, b, big] = descriptor.points.as_slice() else {
        panic!("{:?}", descriptor.points);
    };
    // The bins of `a`: their arrays, kinds and guards.
    let shape = |point, name| {
        let bin = bin(point, name);
        (bin.kind, bin.array.clone(), bin.wildcard, bin.iff)
    };
    assert_eq!(
        shape(a, "one"),
        (BinKind::Bins, BinArray::Open, false, None)
    );
    assert_eq!(
        shape(a, "some"),
        (
            BinKind::Bins,
            BinArray::Fixed(Operand::Input(0)),
            false,
            None
        )
    );
    assert!(matches!(
        shape(a, "two"),
        (BinKind::Bins, BinArray::Fixed(Operand::Constant(size)), false, Some(0))
            if size.value == 2u8.into()
    ));
    assert_eq!(shape(a, "w"), (BinKind::Bins, BinArray::Scalar, true, None));
    // `?` is z: the bits 2 and 0 of the mask, none of the value.
    assert_eq!(constant_ranges(bin(a, "w")).0, ["1000/101"]);
    assert_eq!(
        shape(a, "ig"),
        (BinKind::Ignore, BinArray::Scalar, false, None)
    );
    assert_eq!(
        shape(a, "bad"),
        (BinKind::Illegal, BinArray::Scalar, false, Some(1))
    );
    assert_eq!(
        shape(a, "rest"),
        (BinKind::Bins, BinArray::Open, false, None)
    );
    assert_eq!(bin(a, "rest").values, BinValues::Default);
    assert_eq!(a.predicates, 2);
    // The filters of `b` are asked when an instance is constructed, in
    // the order of the declarations; `b with` is every value of `b`.
    assert_eq!(
        constant_ranges(bin(b, "mod3")),
        (vec!["[0:255]".to_string()], Some(BinFilter::Enumerate(0)))
    );
    assert_eq!(
        constant_ranges(bin(b, "f")),
        (vec!["[$:$]".to_string()], Some(BinFilter::Enumerate(1)))
    );
    let byte = ValueType {
        width: 8,
        signed: false,
    };
    assert_eq!(
        descriptor
            .filters
            .iter()
            .map(|filter| filter.components.clone())
            .collect::<Vec<_>>(),
        [vec![byte], vec![byte]]
    );
    // What the filters keep is decided by `item` and by `lim`, which is
    // handed over: constructions share their bins.
    assert!(descriptor.filters.iter().all(|filter| filter.shared));
    // The elements of a constant array are the values of the bin; those
    // of the queue a function returns are handed over.
    assert_eq!(
        constant_ranges(bin(b, "set")),
        (vec!["5".to_string(), "1".into(), "9".into()], None)
    );
    assert_eq!(bin(b, "set").array, BinArray::Open);
    assert_eq!(bin(b, "q").values, BinValues::Set(0));
    let int = ValueType {
        width: 32,
        signed: true,
    };
    assert_eq!(descriptor.sets, [int]);
    // 11.4.13: 95 to 105, and 10 percent of 50 is 5. 25 percent of -7 is
    // -1, and the bounds -6 and -8 are put in order, as are -17 and -23.
    assert_eq!(constant_ranges(bin(b, "tol")).0, ["[95:105]", "[45:55]"]);
    assert_eq!(constant_ranges(bin(big, "neg")).0, ["[-8:-6]", "[-23:-17]"]);
    assert_eq!(bin(b, "other").kind, BinKind::Illegal);
    assert_eq!(bin(b, "other").values, BinValues::Default);
    assert_eq!(b.predicates, 0);
    // A construction is not asked about the 2^64 values of `big`: the
    // filter is a predicate of each sample, after the guard of the bin.
    assert_eq!(
        constant_ranges(bin(big, "even")),
        (vec!["[$:$]".to_string()], Some(BinFilter::Predicate(1)))
    );
    assert_eq!(bin(big, "even").iff, Some(0));
    assert_eq!(big.predicates, 2);
    // The inputs: the size of `some`, `lim` as what the filter of `f`
    // reads, and the two bounds of `near`. The sample-time filter of
    // `even` reads `lim` as an argument the instance keeps.
    assert_eq!(
        descriptor.inputs,
        [
            InputDescriptor::Integral(int),
            InputDescriptor::Integral(int),
            InputDescriptor::Integral(int),
            InputDescriptor::Integral(int),
        ]
    );
    assert_eq!(constant_ranges(bin(b, "near")).0, ["[input2:input3]"]);
    assert_eq!(descriptor.formals, [int]);
    // `N::$new` hands over the inputs and the elements of the queue, then
    // asks each filter about its values, then builds.
    use crate::ir::CoverageOp;
    let name = |op: &CoverageOp| match op {
        CoverageOp::New { .. } => "new".to_string(),
        CoverageOp::FormalSet { slot, .. } => format!("formal_set{slot}"),
        CoverageOp::Input { index, .. } => format!("input{index}"),
        CoverageOp::SetPush { set, .. } => format!("set_push{set}"),
        CoverageOp::FilterNext { filter } => format!("filter_next{filter}"),
        CoverageOp::FilterItem { filter, .. } => format!("filter_item{filter}"),
        CoverageOp::Build => "build".to_string(),
        CoverageOp::Message => "message".to_string(),
        CoverageOp::Sampling => "sampling".to_string(),
        CoverageOp::Formal { slot, .. } => format!("formal{slot}"),
        CoverageOp::Points { first, count, .. } => format!("points{first}+{count}"),
        CoverageOp::Predicates { item, count } => format!("predicates{item}:{count}"),
        CoverageOp::SampleEnd => "sample_end".to_string(),
        other => panic!("{other:?}"),
    };
    let ops = |suffix: &str| {
        coverage_ops(top, suffix)
            .iter()
            .map(name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ops("cg::$new"),
        [
            "new",
            "formal_set0",
            "input0",
            "input1",
            "set_push0",
            "input2",
            "input3",
            "filter_next0",
            "filter_item0",
            "filter_next0",
            "filter_next1",
            "filter_item1",
            "filter_next1",
            "build",
            "message",
        ]
    );
    // `N::$sample` evaluates the predicates with the values and hands
    // them over after them; the type has illegal bins, so a sample
    // reports what it hit.
    assert_eq!(
        ops("cg::$sample"),
        [
            "sampling",
            "formal0",
            "points1+2",
            "points3+1",
            "predicates1:2",
            "predicates3:2",
            "sample_end",
            "message",
        ]
    );
    // A type without an illegal bin has no such report, and one whose
    // bins are all constants no warning of a construction.
    let ir = analyze(
        "module Top; bit [3:0] a; covergroup cg; \
         coverpoint a { bins b[] = {[0:3]}; ignore_bins ig = {2}; bins rest = default; } \
         endgroup cg c; initial begin c = new; c.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert!(
        !coverage_ops(top, "cg::$sample")
            .iter()
            .chain(&coverage_ops(top, "cg::$new"))
            .any(|op| matches!(op, CoverageOp::Message | CoverageOp::Predicates { .. }))
    );
}

#[test]
fn bin_expressions_the_standard_excludes_are_errors() {
    // What needs the names of the scope and the values of the bins is an
    // error where the type is used.
    for (declaration, rule) in [
        // 19.5.1: the number of bins of an array is positive.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[0] = {[0:3]}; } endgroup",
            "the number of bins of bin `b` of coverpoint `x` of covergroup `Top.cg` is not positive \
             (IEEE 1800-2023 19.5.1)",
        ),
        (
            "bit [3:0] x; localparam int N = -2; \
             covergroup cg; coverpoint x { bins b[N] = {[0:3]}; } endgroup",
            "the number of bins of bin `b` of coverpoint `x` of covergroup `Top.cg` is not positive \
             (IEEE 1800-2023 19.5.1)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[2.0] = {[0:3]}; } endgroup",
            "the number of bins of bin `b` of coverpoint `x` of covergroup `Top.cg` is not an \
             integral expression (IEEE 1800-2023 19.5.1)",
        ),
        // 19.5.1.2: a set expression is "an expression yielding an array of
        // values".
        (
            "bit [3:0] x; localparam int K = 3; \
             covergroup cg; coverpoint x { bins b = K; } endgroup",
            "the values of bin `b` of coverpoint `x` of covergroup `Top.cg` are no array (IEEE \
             1800-2023 19.5.1.2)",
        ),
        // 19.5: a `with` expression and a set expression name constants
        // and arguments only, beside `item` in the first.
        (
            "bit [3:0] x; int limit; \
             covergroup cg; coverpoint x { bins b = {[0:7]} with (item < limit); } endgroup",
            "`limit` in bin `b` of coverpoint `x` of covergroup `Top.cg` is neither a constant nor \
             an argument of the covergroup (IEEE 1800-2023 19.5)",
        ),
        (
            "bit [3:0] x; int q [$]; covergroup cg; coverpoint x { bins b = q; } endgroup",
            "`q` in bin `b` of coverpoint `x` of covergroup `Top.cg` is neither a constant nor an \
             argument of the covergroup (IEEE 1800-2023 19.5)",
        ),
        (
            "bit [3:0] x; \
             function automatic bit f(output int o, input int v); o = v; return 1; endfunction \
             covergroup cg; coverpoint x { bins b = {[0:7]} with (f(item, item)); } endgroup",
            "the function called in bin `b` of coverpoint `x` of covergroup `Top.cg` has an \
             output, inout or ref argument (IEEE 1800-2023 19.5)",
        ),
        // A.2.11, footnote 31: the `with` expression gives a value that an
        // integral type takes.
        (
            "bit [3:0] x; covergroup cg(string s); \
             coverpoint x { bins b = {[0:7]} with (s); } endgroup",
            "the `with` expression of bin `b` of coverpoint `x` of covergroup `Top.cg` is not of a \
             type that is assignment compatible with an integral type (IEEE 1800-2023 19.5.1.1)",
        ),
        // 19.5.1.2: no associative array.
        (
            "bit [3:0] x; typedef int a_t [int]; \
             function automatic a_t made(); a_t a; a[1] = 2; return a; endfunction \
             covergroup cg; coverpoint x { bins b = made(); } endgroup",
            "the values of bin `b` of coverpoint `x` of covergroup `Top.cg` are an associative \
             array (IEEE 1800-2023 19.5.1.2)",
        ),
    ] {
        analyze(&format!("module Top; {declaration} cg c; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        let arguments = if declaration.contains("cg(string s)") {
            "(\"s\")"
        } else {
            ""
        };
        let error = rejected(
            "",
            &format!("{declaration} cg c; initial c = new{arguments};"),
        );
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{declaration}: {error}");
        };
        let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(*text, rule, "{declaration}");
    }
    // `item` is the value a filter is asked about, whatever else has the
    // name: an argument of the covergroup, a variable or a `let` of the
    // scope, or a name a wildcard import would bring.
    for declaration in [
        "bit [3:0] x; covergroup cg(int item); \
         coverpoint x { bins b = {[0:7]} with (item < 4); } endgroup \
         cg c; initial c = new(9);",
        "bit [3:0] x; int item; let twice(v) = v + v; \
         covergroup cg; coverpoint x { bins b[] = x with (item[0]); } endgroup \
         cg c; initial c = new;",
        "bit [3:0] x; let item = 1; \
         covergroup cg; coverpoint x { ignore_bins b = {[0:7]} with (item > 5); } endgroup \
         cg c; initial c = new;",
    ] {
        analyze(&format!("module Top; {declaration} endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
    }
    let ir = analyze(
        "package p; localparam int item = 3; localparam int top = 5; endpackage \
         module Top; import p::*; bit [3:0] x; \
         covergroup cg; coverpoint x { bins b[] = {[0:top]} with (item != 3); } endgroup \
         cg c; initial c = new; localparam int item = 7; endmodule",
    );
    // The import of `top` is taken; `item` of the package is not named, so
    // the declaration of `item` after it is none of an imported name
    // (26.3).
    ir.unwrap();
    // A wildcard value warns of what it stands for, not of its bits.
    let ir = analyze(
        "module Top; bit [3:0] x; covergroup g1; coverpoint x { \
         wildcard bins some = {4'b1??1}; \
         wildcard bins wide = {5'b1??1?}; \
         bins plain = {4'b1??1}; } endgroup \
         g1 c; initial c = new; endmodule",
    )
    .unwrap();
    let warned = bin_warnings(&ir);
    assert_eq!(warned.len(), 2, "{warned:?}");
    assert_eq!((warned[0].0.as_str(), warned[0].1.as_str()), ("x", "wide"));
    assert_eq!((warned[1].0.as_str(), warned[1].1.as_str()), ("x", "plain"));
    // The elements of an array parameter are constants of the bin: those
    // the coverpoint type has not are warned of once, and take no part.
    let ir = analyze(
        "module Top; bit [3:0] x; localparam int S [4] = '{1, 300, 5, -2}; \
         covergroup g1; coverpoint x { bins s[] = S; } endgroup \
         g1 c; initial c = new; endmodule",
    )
    .unwrap();
    let warned = bin_warnings(&ir);
    assert_eq!(warned.len(), 1, "{warned:?}");
    assert_eq!((warned[0].0.as_str(), warned[0].1.as_str()), ("x", "s"));
    assert!(
        warned[0].2.contains("300") && warned[0].2.contains("so do other elements"),
        "{}",
        warned[0].2
    );
    assert_eq!(
        constant_ranges(bin(
            &descriptor(module(&ir, "Top"), "Top.g1").points[0],
            "s"
        ))
        .0,
        ["1", "5"]
    );
}

#[test]
fn filters_share_bins_by_what_they_read() {
    use celox_heap::coverage::InputDescriptor;
    // 19.5.1.1: what a `with` expression reads beside `item` and constants
    // is handed to each construction, an integral value and a string, and
    // decides its bins with them. A filter that reads an object, whose
    // state is not handed over, shares no bins: a method of the class of
    // an embedded covergroup, which reads the object, and a handle among
    // its operands.
    let shared = |source: &str, display: &str| {
        let ir = analyze(source).unwrap_or_else(|error| panic!("{source}: {error}"));
        let descriptor = descriptor(module(&ir, "Top"), display);
        (
            descriptor
                .filters
                .iter()
                .map(|filter| filter.shared)
                .collect::<Vec<_>>(),
            descriptor.inputs.clone(),
        )
    };
    let int = InputDescriptor::Integral(celox_heap::coverage::ValueType {
        width: 32,
        signed: true,
    });
    assert_eq!(
        shared(
            "module Top; bit [3:0] x; \
             function automatic bit f(int v, int l); return v < l; endfunction \
             covergroup cg(int lim, string s); coverpoint x { \
             bins a[] = {[0:7]} with (item < lim); \
             bins b[] = {[0:7]} with (s == \"x\" ? item < 2 : f(item, lim)); \
             bins c[] = {[0:7]} with (s.len() > item); } endgroup \
             cg c; initial c = new(3, \"x\"); endmodule",
            "Top.cg",
        ),
        (
            vec![true, true, true],
            vec![int.clone(), InputDescriptor::String]
        )
    );
    assert_eq!(
        shared(
            "module Top; class C; const int limit; bit [3:0] v; \
             covergroup cg; coverpoint v { \
             bins a[] = {[0:7]} with (ok(item)); \
             bins b[] = {[0:7]} with (item < limit); } endgroup \
             function bit ok(int x); return x < limit; endfunction \
             function new(int l); limit = l; cg = new; endfunction endclass \
             C c; initial c = new(2); endmodule",
            "C::cg",
        ),
        // The second filter reads the constant by its name: it is handed
        // over. The type shares no bins while the first is of it.
        (vec![false, true], vec![int.clone()])
    );
    assert_eq!(
        shared(
            "module Top; bit [3:0] x; \
             class K; int top; function bit below(int v); return v < top; endfunction endclass \
             function automatic bit under(K k, int v); return v < k.top; endfunction \
             covergroup cg(K h); coverpoint x { \
             bins a[] = {[0:7]} with (h.below(item)); \
             bins b[] = {[0:7]} with (item < h.top); \
             bins c[] = {[0:7]} with (under(h, item)); \
             bins d[] = {[0:7]} with (h != null && item < 2); } endgroup \
             cg c; initial c = new(null); endmodule",
            "Top.cg",
        ),
        (vec![false, false, false, false], vec![])
    );
}

/// Select expression `select` as text: `binsof(<position>[.<bin>][ ∩
/// <values>])`, `!` before a negated one, and `and(…)`, `or(…)`.
fn select_text(select: &celox_heap::coverage::Select) -> String {
    use celox_heap::coverage::{Bound, Operand, RangeDescriptor, Select};
    let operand = |operand: &Operand| match operand {
        Operand::Constant(constant) => constant.value.to_string(),
        Operand::Input(index) => format!("input{index}"),
    };
    let bound = |bound: &Bound| match bound {
        Bound::Open => "$".to_string(),
        Bound::Operand(value) => operand(value),
    };
    match select {
        Select::BinsOf {
            position,
            bin,
            intersect,
            negate,
        } => format!(
            "{}binsof({position}{}{})",
            if *negate { "!" } else { "" },
            bin.map_or_else(String::new, |bin| format!(".{bin}")),
            intersect
                .as_ref()
                .map_or_else(String::new, |ranges| format!(
                    " ∩ {}",
                    ranges
                        .iter()
                        .map(|range| match range {
                            RangeDescriptor::Value(value) => operand(value),
                            RangeDescriptor::Range { low, high } =>
                                format!("[{}:{}]", bound(low), bound(high)),
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                ))
        ),
        Select::And(parts) | Select::Or(parts) => format!(
            "{}({})",
            if matches!(select, Select::And(_)) {
                "and"
            } else {
                "or"
            },
            parts.iter().map(select_text).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[test]
fn crosses_compile_to_the_descriptor_of_the_type() {
    use celox_heap::coverage::{BinKind, Operand, OptionId, OptionValue};
    // 19.6: the crosses with what they cross, a variable through a
    // coverpoint of its own after those the covergroup declares, their
    // guards, bins and select expressions, and the options of the
    // definition for the covergroup and for a cross (Table 19-2).
    let ir = analyze(
        "module Top; bit [3:0] v, w; bit [1:0] k; bit en, on; \
         covergroup g1(int lo); \
         option.cross_retain_auto_bins = 0; \
         option.cross_num_print_missing = 3; \
         a: coverpoint v { bins low[] = {[0:3]}; bins high = {[4:15]}; } \
         coverpoint w; \
         vw: cross a, w, k iff (en) { \
           option.weight = 2; \
           option.cross_retain_auto_bins = 1; \
           bins b1 = binsof(a.high) && (binsof(w) intersect {[lo:7], 20} && binsof(k)) iff (on); \
           ignore_bins i = !binsof(a) intersect {1} || (binsof(k) intersect {0} || binsof(w) intersect {2}); \
           illegal_bins x = vw; \
         } \
         cross k, w; \
         endgroup \
         g1 c; initial begin c = new(2); c.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let descriptor = descriptor(top, "Top.g1");
    let points: Vec<_> = descriptor
        .points
        .iter()
        .map(|point| (point.name.as_str(), point.ty.width, point.bins.len()))
        .collect();
    assert_eq!(points, [("a", 4, 2), ("w", 4, 0), ("k", 2, 0)]);
    let [vw, kw] = descriptor.crosses.as_slice() else {
        panic!("{:?}", descriptor.crosses);
    };
    assert_eq!(
        (
            vw.name.as_str(),
            vw.points.as_slice(),
            vw.iff,
            vw.predicates
        ),
        ("vw", [0, 1, 2].as_slice(), Some(0), 2)
    );
    assert_eq!(
        (
            kw.name.as_str(),
            kw.points.as_slice(),
            kw.iff,
            kw.predicates,
            kw.bins.len()
        ),
        ("$cross2", [2, 1].as_slice(), None, 0, 0)
    );
    let bins: Vec<_> = vw
        .bins
        .iter()
        .map(|bin| {
            (
                bin.name.as_str(),
                bin.kind,
                bin.iff,
                select_text(&bin.select),
            )
        })
        .collect();
    // Parentheses group alone: a chain of `&&` or `||` is one node. The
    // value 20 is none of `w` and is dropped with a warning; the
    // identifier of the cross is every bin tuple (19.6.1.2).
    assert_eq!(
        bins,
        [
            (
                "b1",
                BinKind::Bins,
                Some(1),
                "and(binsof(0.1), binsof(1 ∩ [input0:7]), binsof(2))".to_string()
            ),
            (
                "i",
                BinKind::Ignore,
                None,
                "or(!binsof(0 ∩ 1), binsof(2 ∩ 0), binsof(1 ∩ 2))".to_string()
            ),
            ("x", BinKind::Illegal, None, "binsof(0)".to_string()),
        ]
    );
    let options: Vec<_> = descriptor
        .options
        .iter()
        .map(|init| {
            let OptionValue::Operand(Operand::Constant(value)) = &init.value else {
                panic!("{init:?}");
            };
            (init.item, init.option, value.value.to_string())
        })
        .collect();
    assert_eq!(
        options,
        [
            (0, OptionId::CrossRetainAutoBins, "0".to_string()),
            (0, OptionId::CrossNumPrintMissing, "3".to_string()),
            (4, OptionId::Weight, "2".to_string()),
            (4, OptionId::CrossRetainAutoBins, "1".to_string()),
        ]
    );
    let warnings = bin_warnings(&ir);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(
        (warnings[0].0.as_str(), warnings[0].1.as_str()),
        ("vw", "b1")
    );
    assert!(
        warnings[0].2.starts_with("`intersect` of coverpoint `w`: "),
        "{warnings:?}"
    );
    // A sample hands over the values of the three coverpoints in one call
    // and the guards of `vw` in one more, after them; the second cross has
    // no guard and no call. An illegal cross product is reported.
    let ops = coverage_ops(top, "::$sample");
    let names: Vec<_> = ops
        .iter()
        .map(|op| match op {
            crate::ir::CoverageOp::Points { first, count, .. } => format!("points {first}+{count}"),
            crate::ir::CoverageOp::Predicates { item, count } => {
                format!("predicates {item}:{count}")
            }
            op => format!("{op:?}"),
        })
        .collect();
    let predicates = names
        .iter()
        .position(|name| name == "predicates 4:2")
        .unwrap_or_else(|| panic!("{names:?}"));
    let points = names
        .iter()
        .position(|name| name == "points 1+3")
        .unwrap_or_else(|| panic!("{names:?}"));
    assert!(points < predicates, "{names:?}");
    assert_eq!(
        names
            .iter()
            .filter(|name| name.starts_with("predicates"))
            .count(),
        1,
        "{names:?}"
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, crate::ir::CoverageOp::Message)),
        "{names:?}"
    );
}

#[test]
fn cross_declarations_the_standard_excludes_are_errors() {
    // What the declaration alone shows is an error where the covergroup is
    // declared, whether or not something uses it.
    for (declaration, rule) in [
        // 19.6.1: `binsof` names a coverpoint or a bin of one.
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = binsof(z); } endgroup",
            "bin `b` of cross `c` of covergroup `Top.cg` selects the bins of `z`, which is no \
             coverpoint of the covergroup and no variable that it crosses (IEEE 1800-2023 19.6.1)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; a: coverpoint x { bins lo = {0}; } \
             c: cross a, y { bins b = binsof(a.hi); } endgroup",
            "selects `a.hi`, which is no bin that coverpoint `a` declares (IEEE 1800-2023 19.6.1)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = binsof(x.auto); } endgroup",
            "selects `x.auto`, which is no bin that coverpoint `x` declares",
        ),
        // 19.6: a cross name is visible only after a covergroup variable
        // or `::`, so a cross item of that name is a variable, whose
        // implicit coverpoint has the name of the cross (19.5).
        (
            "bit [1:0] x, y, c; covergroup cg; c: cross x, y; d: cross c, x; endgroup",
            "covergroup `Top.cg` has two items named `c`: the coverpoint of the variable that \
             cross `d` of covergroup `Top.cg` crosses and a cross (IEEE 1800-2023 19.5, 19.6)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; x: cross x, y; endgroup",
            "covergroup `Top.cg` has two items named `x`: the coverpoint of the variable that \
             cross `x` of covergroup `Top.cg` crosses and a cross",
        ),
        // 19.6.1.2: only the identifier of the enclosing cross.
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y; d: cross x, y { bins b = c; } endgroup",
            "bin `b` of cross `d` of covergroup `Top.cg` selects the bin tuples of cross `c`",
        ),
        // 19.6: the label of a cross is a scope of its bins.
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = binsof(x); \
             ignore_bins b = binsof(y); } endgroup",
            "cross `c` of covergroup `Top.cg` has two bins named `b` (IEEE 1800-2023 19.6)",
        ),
        // 19.7, 19.10: the options of a cross.
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { option.weight = 1; option.weight = 2; } \
             endgroup",
            "`option.weight` is assigned twice in cross `c`",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { option.auto_bin_max = 2; } endgroup",
            "`option.auto_bin_max` is assigned in a cross",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { option.per_instance = 1; } endgroup",
            "`option.per_instance` is assigned in a cross",
        ),
        (
            "bit [1:0] x, y; covergroup cg; coverpoint x { option.cross_retain_auto_bins = 0; } \
             endgroup",
            "`option.cross_retain_auto_bins` is assigned in a coverpoint",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { type_option.strobe = 1; } endgroup",
            "`type_option.strobe` is assigned in a cross",
        ),
        // 19.8.1: an argument of `sample` is no value of a cross bin.
        (
            "bit [1:0] y; covergroup cg with function sample(bit [1:0] x, int v); \
             c: cross x, y { bins b = binsof(x) intersect {v}; } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in a cross bin",
        ),
        (
            "bit [1:0] x, y; covergroup cg with function sample(int v); \
             c: cross x, y { option.weight = v; } endgroup",
            "argument `v` of the sample method of covergroup `Top.cg` is used in an option value",
        ),
        // 19.5: one name for each item, a variable a cross crosses too.
        (
            "bit [1:0] x, y; covergroup cg; c: coverpoint x; c: cross x, y; endgroup",
            "two items named `c`",
        ),
    ] {
        for used in ["", "cg c0; initial c0 = new;"] {
            let error = rejected("", &format!("{declaration} {used}"));
            let AnalyzerError::InvalidCovergroup(text) = &error else {
                panic!("{declaration}: {error}");
            };
            let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(text.contains(&rule), "{declaration}: {text}");
            assert!(text.contains("IEEE 1800-2023 19."), "{declaration}: {text}");
            assert!(!error.is_unsupported());
        }
    }
    // What needs the names of the scope is an error where the type is
    // used: a cross item that is a constant, a real or a string, and a
    // name in an `intersect` list that is neither a constant nor an
    // argument (19.5).
    for (declaration, rule) in [
        (
            "localparam int P = 1; bit [1:0] y; covergroup cg; cross P, y; endgroup",
            "a cross of covergroup `Top.cg` crosses `P`, which is neither a coverpoint nor a \
             variable (IEEE 1800-2023 19.6)",
        ),
        (
            "function automatic bit [1:0] f(); return 1; endfunction bit [1:0] y; \
             covergroup cg; cross f, y; endgroup",
            "a cross of covergroup `Top.cg` crosses `f`, which is neither a coverpoint nor a \
             variable (IEEE 1800-2023 19.6)",
        ),
        // A name that nothing declares, and the label of a coverpoint of
        // another covergroup (19.6: "Attempts to cross items from
        // different coverage groups shall result in a compiler error").
        (
            "bit [1:0] y; covergroup cg; cross nothere, y; endgroup",
            "a cross of covergroup `Top.cg` crosses `nothere`, which is neither a coverpoint nor a \
             variable (IEEE 1800-2023 19.6)",
        ),
        (
            "bit [1:0] y; covergroup other; lab: coverpoint y; endgroup \
             covergroup cg; cross lab, y; endgroup",
            "a cross of covergroup `Top.cg` crosses `lab`, which is neither a coverpoint nor a \
             variable (IEEE 1800-2023 19.6)",
        ),
        (
            "real r; bit [1:0] y; covergroup cg; cross r, y; endgroup",
            "crosses the real variable `r`: a cross shall not directly include a real variable \
             (IEEE 1800-2023 19.6)",
        ),
        (
            "string s; bit [1:0] y; covergroup cg; cross s, y; endgroup",
            "crosses `s`, which is not of an integral type",
        ),
        (
            "int limit; bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = binsof(x) \
             intersect {limit}; } endgroup",
            "`limit` in bin `b` of cross `c` of covergroup `Top.cg` is neither a constant nor an \
             argument of the covergroup (IEEE 1800-2023 19.5)",
        ),
    ] {
        analyze(&format!("module Top; {declaration} cg c0; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        let error = rejected("", &format!("{declaration} cg c0; initial c0 = new;"));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{declaration}: {error}");
        };
        let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.contains(&rule), "{declaration}: {text}");
    }
    // What the standard allows: a cross of a variable that a coverpoint
    // without a label is of, an argument of the covergroup and one of
    // `sample` crossed (19.8.1), and the identifier of the enclosing cross.
    analyze(
        "module Top; bit [1:0] x, y; \
         covergroup cg(bit [1:0] f) with function sample(bit s); \
         coverpoint x; c: cross x, y, f, s { bins all = c; bins b = binsof(f) && binsof(s); } \
         endgroup cg c0; initial begin c0 = new(1); c0.sample(0); end endmodule",
    )
    .unwrap();
}

#[test]
fn unsupported_cross_constructs_are_rejected_where_used() {
    // What the analysis does not support of a cross is rejected where the
    // type is used, with a text that maps to the tracking issue of
    // covergroups; the declaration alone analyzes.
    let constructs = [
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = c with (x > y); } endgroup",
            "covergroup cross with or matches selection (IEEE 1800-2023 19.6.1.2)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = binsof(x) with (x > 1) \
             matches 2; } endgroup",
            "covergroup cross with or matches selection (IEEE 1800-2023 19.6.1.2)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = c matches $; } endgroup",
            "covergroup cross with or matches selection (IEEE 1800-2023 19.6.1.2)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { bins b = (binsof(x) && binsof(y)) \
             with (x == y); } endgroup",
            "covergroup cross with or matches selection (IEEE 1800-2023 19.6.1.2)",
        ),
        (
            "bit [1:0] x, y; typedef struct { bit [1:0] a, b; } t_t; t_t q[$]; \
             covergroup cg; c: cross x, y { bins b = q; } endgroup",
            "covergroup cross set expression (IEEE 1800-2023 19.6.1.3, 19.6.1.4)",
        ),
        (
            "bit [1:0] x, y; covergroup cg; c: cross x, y { \
             function CrossQueueType f(); f.push_back('{1, 1}); endfunction \
             bins b = f(); } endgroup",
            "covergroup function declared in a cross body (IEEE 1800-2023 19.6, 19.6.1.4)",
        ),
        (
            "bit [1:0] x; covergroup cg; c: cross x, x; endgroup",
            "covergroup cross that lists an item twice (IEEE 1800-2023 19.6)",
        ),
        (
            "bit [1:0] x, y, z; covergroup cg; coverpoint z; c: cross x, y { bins b = binsof(z); } \
             endgroup",
            "covergroup cross selection of the bins of a coverpoint that the cross does not cross \
             (IEEE 1800-2023 19.6.1)",
        ),
    ];
    for (declaration, construct) in constructs {
        let construct = construct.split_whitespace().collect::<Vec<_>>().join(" ");
        for unused in ["", "cg c0;"] {
            let source = format!("module Top; {declaration} {unused} endmodule");
            analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
        }
        for used in [
            "cg c0 = new;",
            "cg c0; initial c0.sample();",
            "real r0; initial r0 = cg::get_coverage();",
        ] {
            let error = rejected("", &format!("{declaration} {used}"));
            assert_eq!(
                error,
                AnalyzerError::Unsupported(construct.clone()),
                "{declaration} {used}"
            );
            assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
        }
    }
    // `&&` and `||` that alternate through more than 32 levels of
    // parentheses.
    let mut select = "binsof(x)".to_string();
    for level in 0..33 {
        let op = if level % 2 == 0 { "&&" } else { "||" };
        select = format!("(binsof(y) {op} {select})");
    }
    let declaration =
        format!("bit [1:0] x, y; covergroup cg; c: cross x, y {{ bins b = {select}; }} endgroup");
    let error = rejected("", &format!("{declaration} cg c0 = new;"));
    assert_eq!(
        error,
        AnalyzerError::Unsupported(
            "covergroup cross selection nested deeper than 32 (IEEE 1800-2023 19.6.1)".into()
        )
    );
    assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    // 32 levels, and a chain of 10,000 terms, which is one node.
    let mut select = "binsof(x)".to_string();
    for level in 0..31 {
        let op = if level % 2 == 0 { "&&" } else { "||" };
        select = format!("(binsof(y) {op} {select})");
    }
    let chain = vec!["binsof(x) intersect {1}"; 10_000].join(" || ");
    analyze(&format!(
        "module Top; bit [1:0] x, y; covergroup cg; c: cross x, y {{ bins b = {select}; \
         bins d = {chain}; }} endgroup cg c0 = new; endmodule"
    ))
    .unwrap();
    // What the parser rejects is rejected where the covergroup is
    // declared, not where it is used: no `new` and no method of `cg` is
    // written. It names the issue of covergroups too.
    for (select, construct) in [
        (
            "!(binsof(x))",
            "covergroup cross selection that negates a parenthesized selection (IEEE 1800-2023 \
             19.6.1)",
        ),
        (
            "en ? binsof(x) : binsof(y)",
            "covergroup cross set expression with a conditional operator (IEEE 1800-2023 \
             19.6.1.4)",
        ),
    ] {
        let source = format!(
            "module Top; bit [1:0] x, y; bit en; covergroup cg; c: cross x, y {{ bins b = {select}; }} \
             endgroup endmodule"
        );
        let error = analyze(&source).unwrap_err().into_error();
        let AnalyzerError::Unsupported(text) = &error else {
            panic!("{select}: {error}");
        };
        let construct = construct.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.ends_with(&construct), "{select}: {text}");
        assert!(text.starts_with("covergroups.sv:"), "{select}: {text}");
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE, "{select}");
    }
}

#[test]
fn options_and_methods_bind_through_instances_and_types() {
    // 19.7, 19.7.1, 19.8, 19.10: the members of `option` through an
    // instance and of `type_option` through an instance or the type, at the
    // covergroup, a coverpoint and a cross; the methods of the items; the
    // two `ref int` arguments; `start`, `stop` and `set_inst_name`.
    let ir = analyze(
        "module Top; bit [1:0] a, b; int covered, total; real r; string s; bit on; \
         covergroup cg; ca: coverpoint a; coverpoint b; x: cross ca, b; endgroup \
         cg c; \
         initial begin \
           cg::type_option.comment = \"type\"; cg::ca::type_option.weight = 3; \
           cg::x::type_option.goal = 90; cg::type_option.merge_instances = 1; \
           cg::type_option.distribute_first = 0; \
           c = new; \
           c.option.weight = 2; c.option.name = \"first\"; c.ca.option.at_least = 2; \
           c.b.option.comment = s; c.x.option.cross_num_print_missing = 4; \
           c.type_option.weight = 4; c.ca.type_option.goal = 50; \
           covered = c.option.at_least; s = c.option.comment; on = c.option.per_instance; \
           covered = c.x.type_option.weight; on = cg::type_option.merge_instances; \
           s = cg::ca::type_option.comment; \
           r = c.ca.get_coverage() + c.ca.get_inst_coverage() + cg::x::get_coverage(); \
           r = c.get_coverage(covered, total) + c.get_inst_coverage(covered, total); \
           r = c.x.get_inst_coverage(covered, total) + cg::ca::get_coverage(covered, total); \
           void'(cg::get_coverage(covered, total)); \
           c.stop(); c.ca.stop(); c.sample(); c.ca.start(); c.x.stop(); c.start(); \
           c.set_inst_name(\"named\"); c.set_inst_name(s); \
         end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let names = top
        .subroutines()
        .iter()
        .map(|subroutine| subroutine.name.as_str())
        .filter(|name| name.starts_with("$covergroup::"))
        .collect::<Vec<_>>();
    assert!(
        names.iter().any(|name| name.ends_with("::$counts")),
        "{names:?}"
    );
    assert!(
        names.iter().any(|name| name.ends_with("::$type_counts")),
        "{names:?}"
    );
    // Without a call with two arguments a module makes neither.
    let ir = analyze(
        "module Top; bit a; real r; covergroup cg; coverpoint a; endgroup cg c = new; \
         initial begin c.option.weight = 2; r = c.a.get_coverage(); c.stop(); end endmodule",
    )
    .unwrap();
    assert!(
        !module(&ir, "Top")
            .subroutines()
            .iter()
            .any(|subroutine| subroutine.name.ends_with("counts")),
    );
    // Through a `null` handle and the members of an embedded covergroup
    // of the objects of a class.
    analyze(
        "class C; bit [1:0] v; covergroup cg; cv: coverpoint v; endgroup \
         function new(); cg = new; cg.option.weight = 2; cg.cv.option.at_least = 2; endfunction \
         function void off(); cg.stop(); cg.cv.start(); endfunction endclass \
         module Top; C o; int n, t; real r; \
         initial begin o = new; o.cg.option.comment = \"o\"; o.cg.cv.option.goal = 1; \
         r = o.cg.cv.get_inst_coverage(n, t); o.cg.set_inst_name(\"o\"); o.off(); \
         n = o.cg.type_option.weight; end endmodule",
    )
    .unwrap();
}

#[test]
fn procedural_options_the_standard_excludes_are_errors() {
    let covergroup = "bit [1:0] a, b; int n; string s; real r; \
        covergroup cg; ca: coverpoint a; x: cross a, b; endgroup cg c = new;";
    for (items, rule) in [
        // "The per_instance and get_inst_coverage options can only be set in
        // the covergroup definition. The auto_bin_max, detect_overlap, and
        // cross_retain_auto_bins options can only be set in the covergroup or
        // coverpoint definition" (19.7).
        (
            "initial c.option.per_instance = 1;",
            "`option.per_instance` of a covergroup of covergroup `Top.cg` is assigned in a procedure: it can be set only in the covergroup definition (IEEE 1800-2023 19.7)",
        ),
        (
            "initial c.option.get_inst_coverage = 1;",
            "`option.get_inst_coverage` of a covergroup",
        ),
        (
            "initial c.option.auto_bin_max = 4;",
            "(IEEE 1800-2023 19.7)",
        ),
        ("initial c.ca.option.auto_bin_max = 4;", "of a coverpoint"),
        (
            "initial c.ca.option.detect_overlap = 1;",
            "(IEEE 1800-2023 19.7)",
        ),
        (
            "initial c.x.option.cross_retain_auto_bins = 0;",
            "of a cross",
        ),
        // "The strobe and real_interval type options can only be set in the
        // covergroup definition" (19.7.1).
        (
            "initial cg::type_option.strobe = 1;",
            "`type_option.strobe` of a covergroup of covergroup `Top.cg` is assigned in a procedure: it can be set only in the covergroup definition (IEEE 1800-2023 19.7.1)",
        ),
        (
            "initial c.type_option.real_interval = 2.0;",
            "(IEEE 1800-2023 19.7.1)",
        ),
        // 19.10: the members of each level.
        (
            "initial n = c.option.size;",
            "`option` of covergroup `Top.cg` has no member `size` (IEEE 1800-2023 19.10)",
        ),
        (
            "initial c.ca.option.name = \"x\";",
            "`option.name` is used on a coverpoint of covergroup `Top.cg`, which has no such member (IEEE 1800-2023 19.10)",
        ),
        ("initial n = c.x.option.auto_bin_max;", "used on a cross"),
        (
            "initial c.ca.type_option.merge_instances = 1;",
            "(IEEE 1800-2023 19.10)",
        ),
        (
            "initial n = cg::x::type_option.real_interval;",
            "(IEEE 1800-2023 19.10)",
        ),
        // `option` is a member of an instance, `type_option` a static one.
        (
            "initial cg::option.weight = 2;",
            "named with `::`: `option` is a member of each instance",
        ),
        (
            "initial n = cg::ca::option.weight;",
            "(IEEE 1800-2023 19.7, 19.7.1)",
        ),
        // Items (19.5, 19.6), and the methods Table 19-5 gives them.
        (
            "initial r = c.nope.get_coverage();",
            "covergroup `Top.cg` has no coverpoint or cross named `nope` (IEEE 1800-2023 19.5, 19.6)",
        ),
        ("initial c.nope.option.weight = 2;", "named `nope`"),
        (
            "initial c.ca.sample();",
            "`sample` called on a coverpoint or cross",
        ),
        (
            "initial c.x.set_inst_name(\"x\");",
            "`set_inst_name` called on a coverpoint",
        ),
        (
            "initial cg::start();",
            "it is not a static method (IEEE 1800-2023 19.8)",
        ),
        (
            "initial r = cg::ca::get_inst_coverage();",
            "it is not a static method",
        ),
        ("initial cg::ca::start();", "it is not a static method"),
        ("initial cg::x::stop();", "it is not a static method"),
        (
            "initial n = c.start();",
            "the start method of covergroup `Top.cg` returns no value",
        ),
        ("initial s = c.ca.stop();", "the stop method"),
        ("initial n = c.set_inst_name(\"x\");", "returns no value"),
        (
            "initial n = c.option.weight();",
            "it is a member of a structure, not a method",
        ),
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{items}: {error}");
        };
        assert!(text.contains(rule), "{items}: {text}");
    }
    // The arity of the methods (19.8) is that of a call.
    for items in [
        "initial r = c.get_coverage(n);",
        "initial r = c.get_inst_coverage(n, n, n);",
        "initial r = cg::ca::get_coverage(n);",
        "initial c.start(1);",
        "initial c.set_inst_name();",
        "initial c.set_inst_name(s, s);",
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        assert!(
            matches!(error, AnalyzerError::InvalidCallArguments { .. }),
            "{items}: {error}"
        );
    }
    // The two arguments are variables passed by reference (13.5.2).
    for items in [
        "initial r = c.get_coverage(1, n);",
        "initial r = c.ca.get_inst_coverage(n, n + 1);",
        "logic [31:0] l; initial r = c.get_coverage(l, n);",
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        let AnalyzerError::InvalidRefArgument { subroutine, .. } = &error else {
            panic!("{items}: {error}");
        };
        // The error names the method, not a subroutine of the analysis.
        assert!(
            matches!(subroutine.as_str(), "get_coverage" | "get_inst_coverage"),
            "{items}: {error}"
        );
    }
    // Table 19-5 gives the formals of the methods no names; those of a
    // `with function sample` declaration have theirs (19.8.1).
    for items in [
        "initial r = c.get_coverage(.covered(n), .total(n));",
        "initial r = cg::ca::get_coverage(.a(n), .b(n));",
        "initial r = c.get_inst_coverage(n, .total(n));",
        "initial c.set_inst_name(.name(s));",
        "initial c.start(.x());",
        "initial c.sample(.v(1));",
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        let AnalyzerError::InvalidCallArguments { subroutine, detail } = &error else {
            panic!("{items}: {error}");
        };
        assert!(
            !subroutine.contains('$') && !detail.contains('$'),
            "{items}: {error}"
        );
    }
    analyze(
        "module Top; covergroup cg with function sample(int v, bit b = 0); coverpoint v; endgroup \
         cg c = new; initial c.sample(.b(1), .v(2)); endmodule",
    )
    .unwrap();
    // `$bits` of a member through the type, as through an instance (20.6.2).
    analyze(&format!(
        "module Top; {covergroup} initial begin n = $bits(cg::type_option.weight); \
         n = $bits(c.type_option.goal) + $bits(cg::ca::type_option.weight) + $bits(c.option.at_least); \
         end endmodule"
    ))
    .unwrap();
    // The value is checked as one assigned to a variable of the member's
    // type (19.10).
    for items in [
        "initial c.option.weight = s;",
        "bit [1:0] v[2]; initial c.option.weight = v;",
        "initial c.ca.option.at_least = c;",
    ] {
        assert!(
            analyze(&format!("module Top; {covergroup} {items} endmodule")).is_err(),
            "{items}"
        );
    }
    // What the analysis does not support of the members.
    for (items, construct) in [
        (
            "initial c.option.weight += 1;",
            "covergroup option member assigned with `+=` (`option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "initial c.option.weight <= 1;",
            "covergroup option member assigned with `<=` (`option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "initial c.option.weight++;",
            "covergroup option member written other than by an assignment with `=` (`option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "initial cg::type_option.weight++;",
            "covergroup option member written other than by an assignment with `=` (`type_option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "initial c.type_option.weight--;",
            "covergroup option member written other than by an assignment with `=` (`type_option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "task automatic t(output int o); o = 1; endtask initial t(c.option.weight);",
            "covergroup option member written other than by an assignment with `=` (`option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "function automatic int f(inout int o); o = 1; return 0; endfunction \
             initial n = f(c.ca.option.at_least);",
            "covergroup option member written other than by an assignment with `=` (`option.at_least` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "initial force c.type_option.weight = 1;",
            "covergroup option member written other than by an assignment with `=` (`type_option.weight` of covergroup `Top.cg`; IEEE 1800-2023 19.7)",
        ),
        (
            "initial n = cg::type_option.strobe;",
            "covergroup type_option.strobe read in a procedure (IEEE 1800-2023 19.7.1)",
        ),
        (
            "initial r = c.ca.type_option.real_interval;",
            "covergroup type_option.real_interval read in a procedure (IEEE 1800-2023 19.7.1)",
        ),
        (
            "initial $display(\"%p\", c.option);",
            "covergroup option structure used as a whole (`option` of `Top.cg`; IEEE 1800-2023 19.10)",
        ),
    ] {
        let error = rejected("", &format!("{covergroup} {items}"));
        assert_eq!(
            error,
            AnalyzerError::Unsupported(construct.into()),
            "{items}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
}

#[test]
fn type_options_of_the_definition_compile_to_the_descriptor() {
    // 19.7.1: `type_option` assignments of the definition, at the three
    // levels, are constants of the descriptor of the type.
    let ir = analyze(
        "module Top; bit [1:0] a, b; localparam int W = 3; \
         covergroup cg; \
         type_option.weight = W + 1; type_option.goal = 80; type_option.comment = \"of the type\"; \
         type_option.merge_instances = 1; type_option.distribute_first = 1; \
         type_option.strobe = 0; \
         ca: coverpoint a { type_option.weight = W; option.weight = 2; type_option.comment = \"a\"; \
         type_option.goal = 2.5; } \
         x: cross ca, b { type_option.goal = 70; type_option.weight = -0.4; } \
         option.detect_overlap = 1; option.get_inst_coverage = 1; \
         endgroup cg c = new; endmodule",
    )
    .unwrap();
    let descriptor_of = |ir: &Ir| descriptor(module(ir, "Top"), "Top.cg");
    let descriptor = descriptor_of(&ir);
    let group = &descriptor.type_options;
    assert_eq!(
        (group.weight, group.goal, group.comment.as_str()),
        (4, 80, "of the type")
    );
    assert!(group.merge_instances && group.distribute_first);
    let point = &descriptor.points[0].type_options;
    assert_eq!(
        (point.weight, point.goal, point.comment.as_str()),
        (3, 3, "a")
    );
    // A `real` constant is rounded, as an `int` variable takes it (6.12.1).
    let cross = &descriptor.crosses[0].type_options;
    assert_eq!((cross.weight, cross.goal), (0, 70));
    let rounded = analyze(
        "module Top; bit a; covergroup cg; type_option.goal = 2.5; type_option.weight = 2.0 + 0.6; \
         type_option.merge_instances = 0.6; coverpoint a; endgroup cg c = new; endmodule",
    )
    .unwrap();
    let group = &descriptor_of(&rounded).type_options;
    assert_eq!(
        (group.goal, group.weight, group.merge_instances),
        (3, 3, true)
    );
    use celox_heap::coverage::OptionId;
    assert!(
        descriptor
            .options
            .iter()
            .any(|option| option.option == OptionId::DetectOverlap)
    );
    assert!(
        descriptor
            .options
            .iter()
            .any(|option| option.option == OptionId::GetInstCoverage)
    );
    // What the standard excludes: a value that is no constant (19.7.1, "NOTE:
    // type_option.weight = w would cause syntax error"), a negative weight
    // (Table 19-3), a member at a level that has none (19.10).
    for (declaration, rule) in [
        (
            "bit a; covergroup cg(int w = 1); coverpoint a { type_option.weight = w; } endgroup",
            "`type_option.weight` of covergroup `Top.cg` is assigned a value that is not a constant expression (IEEE 1800-2023 19.7.1)",
        ),
        (
            "int v; covergroup cg; type_option.goal = v; endgroup",
            "is assigned a value that is not a constant expression",
        ),
        (
            "covergroup cg; type_option.weight = -1; endgroup",
            "`type_option.weight` of covergroup `Top.cg` is assigned the negative weight -1 (IEEE 1800-2023 19.7.1)",
        ),
        (
            "bit a; covergroup cg; coverpoint a { type_option.merge_instances = 1; } endgroup",
            "`type_option.merge_instances` is assigned in a coverpoint",
        ),
    ] {
        let error = rejected("", &format!("{declaration} cg c = new;"));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{declaration}: {error}");
        };
        assert!(text.contains(rule), "{declaration}: {text}");
    }
}

#[test]
fn strobe_follows_the_sampling_event() {
    // 19.3: strobe "only applies to the scheduling of samples triggered by
    // a clocking event. It shall have no effect on sampling due to a block
    // event expression or procedural calls to the sample() method." A type
    // without a clocking event accepts it; one with a clocking event that
    // sets it to a nonzero constant is rejected for it.
    for declaration in [
        "covergroup cg with function sample(bit v); type_option.strobe = 1; coverpoint v; endgroup",
        "covergroup cg; type_option.strobe = 1'b1; endgroup",
        "covergroup cg; type_option.strobe = 0; endgroup",
        "bit clk; covergroup cg @(posedge clk); type_option.strobe = 0; endgroup",
    ] {
        analyze(&format!("module Top; {declaration} cg c = new; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
    }
    for (strobe, construct) in [
        ("1", "covergroup strobe sampling (IEEE 1800-2023 19.7.1)"),
        (
            "2 - 1",
            "covergroup strobe sampling (IEEE 1800-2023 19.7.1)",
        ),
    ] {
        let error = rejected(
            "",
            &format!(
                "bit clk; covergroup cg @(posedge clk); type_option.strobe = {strobe}; endgroup \
                 cg c = new;"
            ),
        );
        assert_eq!(
            error,
            AnalyzerError::Unsupported(construct.into()),
            "{strobe}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
}

/// The sampler processes of `module`: what samples each covergroup instance
/// that a declaration constructs at the clocking event of its type, when the
/// event is not one a flip-flop shaped procedure waits for.
fn samplers(module: &crate::ir::Module) -> Vec<&crate::ir::InitialProcess> {
    module
        .initial_processes()
        .iter()
        .filter(|process| {
            process
                .site()
                .is_some_and(|site| site.construct == "covergroup sampling")
        })
        .collect()
}

/// The flip-flop shaped procedures of `module` that sample covergroup
/// instances at edges.
fn edge_samplers(module: &crate::ir::Module) -> Vec<&crate::ir::FfProcess> {
    module
        .ff_processes()
        .iter()
        .filter(|process| {
            process
                .site()
                .is_some_and(|site| site.construct == "covergroup sampling")
        })
        .collect()
}

/// The hidden handle variables of the sites of `module`.
fn site_handles(module: &crate::ir::Module) -> Vec<&str> {
    module
        .signals()
        .iter()
        .filter(|signal| signal.name().starts_with("%covergroup_site"))
        .map(|signal| {
            assert_eq!(signal.r#type().kind(), crate::ir::TypeKind::Object);
            signal.name()
        })
        .collect()
}

/// Whether `stmt` is the call of `N::$sample` with a hidden handle of
/// `handles`.
fn samples_a_site(stmt: &crate::ir::Stmt, handles: &[&str]) -> bool {
    use crate::ir::{Expr, Stmt};
    matches!(stmt, Stmt::Call { name, args, .. }
        if name.ends_with("::$sample")
            && matches!(args.as_slice(), [Some(Expr::Ident(handle))] if handles.contains(&handle.as_str())))
}

#[test]
fn clocked_covergroups_sample_the_instances_of_their_declarations() {
    use crate::ir::{Expr, FfEdge, Stmt};
    use crate::procedural::{EventEdge, LoopKind};
    // 19.3: an instance that a declaration of a module or a program
    // constructs is sampled at the clocking event of its type, one sampler
    // for each declaration (in a generate loop, one per iteration), with
    // the instance in a hidden variable. The event is bound where the type
    // is declared: the clock of the module, not that of the generate block.
    //
    // One edge of a one-bit variable is sampled by a flip-flop shaped
    // `always`, as `always @(posedge clk) c.sample();` is, so that a sample
    // runs with the flip-flops of the edge; one procedure samples every
    // site of the type, in the order of the declarations, and an `iff` is
    // checked in its body.
    let ir = analyze(
        "module Top; bit clk, en; bit [3:0] d; \
         covergroup cg @(posedge clk iff en); coverpoint d; endgroup \
         cg c = new; \
         for (genvar i = 0; i < 2; i++) begin : g bit clk; cg x = new(); end \
         covergroup fall @(negedge clk); endgroup fall f = new; \
         endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let handles = site_handles(top);
    assert_eq!(handles.len(), 4, "{handles:?}");
    assert!(samplers(top).is_empty());
    let procedures = edge_samplers(top);
    let [guarded, fall] = procedures.as_slice() else {
        panic!("{procedures:?}");
    };
    // The clock of the module, not that of the generate block.
    let events = |process: &crate::ir::FfProcess| {
        process
            .events()
            .iter()
            .map(|event| (event.edge(), event.signal().to_owned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(events(guarded), [(FfEdge::Pos, "clk".to_owned())]);
    assert_eq!(events(fall), [(FfEdge::Neg, "clk".to_owned())]);
    assert!(!guarded.is_always_ff() && !fall.is_always_ff());
    // Procedures of the analysis, which draw no seed (18.14.1).
    assert!(guarded.is_synthetic() && fall.is_synthetic());
    assert_eq!(guarded.body().len(), 3);
    for (stmt, handle) in guarded.body().iter().zip(&handles[..3]) {
        let Stmt::If {
            condition,
            then_body,
            else_body,
        } = stmt
        else {
            panic!("{stmt:?}");
        };
        assert_eq!(*condition, Expr::Ident("en".into()));
        assert!(else_body.is_empty());
        assert!(matches!(then_body.as_slice(), [call] if samples_a_site(call, &[handle])));
    }
    assert!(matches!(fall.body(), [call] if samples_a_site(call, &handles[3..])));
    // Any other event, and every event of a program, which runs in the
    // Reactive region set (24.3.1), is waited for by a process of each
    // site, `initial forever begin @(event); N::$sample(handle); end`,
    // which draws no seed and is no `always` (a level-sensitive `always`
    // would run once at time zero in two-state simulation).
    let ir = analyze(
        "module Top; bit clk, en, a, b; bit [1:0] bus; event e; \
         covergroup change @(clk); endgroup change c1 = new; \
         covergroup levels @(a or b); endgroup levels c2 = new; \
         covergroup named @e; endgroup named c3 = new; \
         covergroup guards @(posedge a iff en or posedge b); endgroup guards c4 = new; \
         covergroup mixed @(posedge a or b); endgroup mixed c5 = new; \
         covergroup both @(edge clk); endgroup both c6 = new; \
         covergroup list @(posedge a or negedge b); endgroup list c7 = new; \
         covergroup wide @(posedge bus); endgroup wide c8 = new; \
         endmodule \
         program prog(input bit clk); covergroup pc @(posedge clk); endgroup pc p = new; endprogram",
    )
    .unwrap();
    for (name, count) in [("Top", 8), ("prog", 1)] {
        let scope = module(&ir, name);
        let handles = site_handles(scope);
        assert_eq!(handles.len(), count, "{name}: {handles:?}");
        assert!(edge_samplers(scope).is_empty(), "{name}");
        let processes = samplers(scope);
        assert_eq!(processes.len(), count, "{name}");
        for process in processes {
            assert!(process.is_synthetic() && !process.repeats(), "{name}");
            assert_eq!(process.kind(), celox_design::ProcessKind::Regular);
            let [
                Stmt::Loop {
                    kind: LoopKind::Forever,
                    body,
                    ..
                },
            ] = process.body()
            else {
                panic!("{name}: {:?}", process.body());
            };
            let (wait, [call]) = body.split_at(body.len() - 1) else {
                panic!("{name}: {body:?}");
            };
            assert!(samples_a_site(call, &handles), "{call:?}");
            match wait {
                [Stmt::WaitEvent(items)] if name == "prog" => {
                    assert!(matches!(items.as_slice(), [item] if item.edge == EventEdge::Pos));
                }
                [Stmt::WaitEvent(items)] => assert!(
                    items.len() > 1
                        || items
                            .iter()
                            .any(|item| matches!(item.edge, EventEdge::Any | EventEdge::Both))
                        || matches!(&items[0].expr, Expr::Ident(name) if name == "bus"),
                    "{items:?}"
                ),
                [Stmt::Eval(_)] => {}
                wait => panic!("{name}: {wait:?}"),
            }
        }
    }
    // The declaration alone, a variable of the type, its methods and
    // `sample()` analyze (19.8: the methods "can be invoked procedurally at
    // any time").
    for items in [
        "",
        "cg c;",
        "cg c; real r; initial begin c.sample(); r = c.get_inst_coverage(); c.stop(); end",
        "real r; initial r = cg::get_coverage();",
        "cg c = new; real r; initial begin c.sample(); c.option.at_least = 2; c = null; end",
    ] {
        let source = format!(
            "module Top; bit clk; covergroup cg @(posedge clk); endgroup {items} endmodule"
        );
        let ir = analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
        assert_eq!(
            edge_samplers(module(&ir, "Top")).len(),
            usize::from(items.contains("= new;")),
            "{source}"
        );
    }
}

#[test]
fn clocking_events_the_analysis_does_not_support_are_rejected_where_used() {
    // What stays unsupported of clocking events is rejected where the type
    // is constructed or used, with a text that maps to the tracking issue.
    for (before, items, construct) in [
        // 19.3: "A clocking event may refer to arguments of the covergroup."
        (
            "",
            "bit clk; covergroup cg(bit x) @(posedge x); endgroup cg c = new(clk);",
            "covergroup sampling event that names an argument of the covergroup (IEEE 1800-2023 19.3)",
        ),
        (
            "",
            "bit clk, clk_en; covergroup cg(bit clk_en) @(posedge clk iff clk_en); endgroup \
             cg c = new(1);",
            "covergroup sampling event that names an argument of the covergroup (IEEE 1800-2023 19.3)",
        ),
        // An event the analysis does not support in any event control.
        (
            "",
            "bit clk; event e; covergroup cg @(posedge clk or e); endgroup cg c = new;",
            "covergroup sampling event (event control mixing named events with other events)",
        ),
    ] {
        let error = rejected(before, items);
        assert_eq!(
            error,
            AnalyzerError::Unsupported(construct.to_string()),
            "{before} {items}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE, "{items}");
    }
    // A declaration of a package or the compilation unit whose initializer
    // constructs a clocked instance, itself or through the constructor of a
    // class that embeds one, or through a function: its sampler would be a
    // process of the package.
    let package = "covergroup sampling event of an instance that a declaration of a package or the compilation unit constructs (IEEE 1800-2023 19.3)";
    for before in [
        "package p; bit clk; covergroup cg @(posedge clk); endgroup cg c = new; endpackage",
        "package p; event e; covergroup cg @e; endgroup cg c = new; endpackage",
        "package p; bit clk; covergroup cg @(posedge clk); endgroup \
         function automatic cg make(); cg c; c = new; return c; endfunction \
         cg c = make(); endpackage",
        "package p; bit clk; class K; covergroup cg @(posedge clk); endgroup \
         function new(); cg = new; endfunction endclass K k = new; endpackage",
        "bit clk; covergroup cg @(posedge clk); endgroup cg c = new;",
    ] {
        // The module uses the scope, which is elaborated for it.
        let import = if before.starts_with("package") {
            "import p::*;"
        } else {
            "initial c.sample();"
        };
        let source = format!("{before} module Top; {import} endmodule");
        let error = match analyze(&source) {
            Ok(_) => panic!("{source}: accepted"),
            Err(error) => error.into_error(),
        };
        assert_eq!(
            error,
            AnalyzerError::Unsupported(package.into()),
            "{source}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
    // The coverage event is one of a clocking event, a sample method and a
    // block event (Syntax 19-1): a covergroup declares at most one.
    for header in [
        "@(posedge clk) with function sample(bit v)",
        "with function sample(bit v) @(posedge clk)",
        "@(posedge clk) @@(begin t)",
    ] {
        let source = format!(
            "module Top; bit clk; task automatic t; endtask covergroup cg {header}; endgroup endmodule"
        );
        let error = analyze(&source).map(|_| ()).unwrap_err().into_error();
        assert!(
            matches!(&error, AnalyzerError::Parse(text) if text.contains("expected `;`")),
            "{source}: {error:?}"
        );
    }
    // A covergroup with a clocking event declared in an interface is
    // rejected for every use, also in an interface with ports that nothing
    // but its instance names, which is elaborated as a module is.
    let source = "interface ifc(input bit clk); covergroup ic @(posedge clk); endgroup \
                  ic i = new; endinterface \
                  module Top; bit clk; ifc f(.clk(clk)); endmodule";
    let error = analyze(source).map(|_| ()).unwrap_err().into_error();
    assert_eq!(
        error,
        AnalyzerError::Unsupported(
            "covergroup sampling event of a covergroup declared in an interface (IEEE 1800-2023 19.3, 25.3)"
                .into()
        ),
        "{source}"
    );
    assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
}

/// The subroutines of the modules of `ir` whose names end in `suffix`.
fn subroutines_ending<'a>(ir: &'a Ir, suffix: &str) -> Vec<&'a crate::ir::Subroutine> {
    ir.modules()
        .iter()
        .flat_map(|module| module.subroutines())
        .filter(|subroutine| {
            subroutine.name.starts_with("$covergroup::") && subroutine.name.ends_with(suffix)
        })
        .collect()
}

/// Whether `stmt` names a subroutine whose name ends in `callee`.
fn calls(stmt: &crate::ir::Stmt, callee: &str) -> bool {
    format!("{stmt:?}").contains(&format!("{callee}\""))
}

#[test]
fn constructed_instances_start_a_sampler_that_their_construction_returns() {
    use crate::ir::{Expr, Stmt};
    use crate::procedural::{EventEdge, LoopKind};
    // 19.3: any other `new` of a type with a clocking event hands the
    // instance to `N::$spawn`. Of a type of the module it adds the instance
    // to the list of the type, four handle variables, a count and a queue,
    // which the edge-sensitive procedure of the type walks after the
    // instance of the declaration; no process is started.
    let ir = analyze(
        "module Top; bit clk; bit [3:0] d; \
         covergroup cg @(posedge clk); coverpoint d; endgroup \
         covergroup ch @(d); coverpoint d; endgroup \
         cg c, e = new; ch h; \
         initial begin c = new; h = new; fork #1; join_none end \
         endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert_eq!(site_handles(top).len(), 1);
    let listed = |prefix: &str| {
        top.signals()
            .iter()
            .filter(|signal| signal.name().starts_with(prefix))
            .count()
    };
    assert_eq!(
        (
            listed("%covergroup_instance0_"),
            listed("%covergroup_instance_count"),
            listed("%covergroup_instances")
        ),
        (4, 2, 2)
    );
    let spawns = subroutines_ending(&ir, "::$spawn");
    assert_eq!(spawns.len(), 2);
    for spawn in &spawns {
        assert!(!spawn.is_task && spawn.automatic);
        let [handle] = spawn.params.as_slice() else {
            panic!("{:?}", spawn.params);
        };
        assert_eq!(handle.source_name, "$cg");
        let mut forks = 0;
        for stmt in &spawn.body {
            stmt.walk(&mut |stmt| forks += usize::from(matches!(stmt, Stmt::Fork { .. })));
        }
        assert_eq!(forks, 0, "{:?}", spawn.body);
        // It adds the handle and returns it.
        let [
            Stmt::If { .. },
            Stmt::Assign { .. },
            Stmt::Assign { rhs, .. },
        ] = spawn.body.as_slice()
        else {
            panic!("{:?}", spawn.body);
        };
        assert!(matches!(rhs, Expr::Ident(returned) if *returned == handle.name));
    }
    // The edge-sensitive procedure of `cg` samples the declaration's
    // instance, then the list; `ch` has a process for the variables of its
    // list and one for its queue, which first waits until the queue holds
    // an instance. All draw no seed.
    let [procedure] = edge_samplers(top)[..] else {
        panic!("one edge procedure");
    };
    assert!(procedure.is_synthetic());
    let body = procedure.body();
    assert_eq!(body.len(), 1 + 4 + 1, "{body:?}");
    assert!(samples_a_site(&body[0], &site_handles(top)));
    let processes = samplers(top);
    let [slots, queue] = processes.as_slice() else {
        panic!("{processes:?}");
    };
    assert!(slots.is_synthetic() && queue.is_synthetic());
    let [
        Stmt::Loop {
            kind: LoopKind::Forever,
            body,
            ..
        },
    ] = slots.body()
    else {
        panic!("{:?}", slots.body());
    };
    assert!(matches!(&body[0], Stmt::WaitEvent(items)
        if matches!(items.as_slice(), [item] if item.edge == EventEdge::Any)));
    assert_eq!(body.len(), 1 + 4, "{body:?}");
    let [Stmt::Wait(_), Stmt::Loop { body, .. }] = queue.body() else {
        panic!("{:?}", queue.body());
    };
    assert_eq!(body.len(), 2, "{body:?}");
    // The procedure constructs through it; its own fork is a fork of the
    // source.
    let procedure = top
        .initial_processes()
        .iter()
        .find(|process| process.body().iter().any(|stmt| calls(stmt, "::$spawn")))
        .expect("the procedure that constructs");
    let mut forks = Vec::new();
    for stmt in procedure.body() {
        stmt.walk(&mut |stmt| {
            if let Stmt::Fork { unseeded, .. } = stmt {
                forks.push(*unseeded);
            }
        });
    }
    assert_eq!(forks, [false]);
    // A type that only declarations construct has no sampler subroutine.
    let ir = analyze(
        "module Top; bit clk; covergroup cg @(posedge clk); endgroup cg e = new; endmodule",
    )
    .unwrap();
    assert!(subroutines_ending(&ir, "::$spawn").is_empty());
    // An embedded covergroup: the event is bound in its class, a property
    // read through `this`, and the sampler stops at the first event after
    // the property no longer holds the instance (19.4: a later `cg = new`).
    let ir = analyze(
        "module Top; class K; bit b; \
         covergroup cg @(posedge b); endgroup \
         function new(); cg = new; endfunction endclass \
         K k; initial k = new; endmodule",
    )
    .unwrap();
    let [spawn] = subroutines_ending(&ir, "::$spawn")[..] else {
        panic!("one sampler subroutine");
    };
    let [this, handle] = spawn.params.as_slice() else {
        panic!("{:?}", spawn.params);
    };
    assert_eq!(
        (this.source_name.as_str(), handle.source_name.as_str()),
        ("this", "$cg")
    );
    let Some(Stmt::Fork {
        unseeded: true,
        branches,
        ..
    }) = spawn.body.first()
    else {
        panic!("{:?}", spawn.body);
    };
    let [Stmt::Loop { body, .. }] = branches[0].as_slice() else {
        panic!("{branches:?}");
    };
    let [
        Stmt::WaitEvent(items),
        Stmt::If {
            condition,
            then_body,
            ..
        },
        Stmt::Call { args, .. },
    ] = body.as_slice()
    else {
        panic!("{body:?}");
    };
    assert!(
        matches!(items.as_slice(), [item] if item.edge == EventEdge::Pos
            && !matches!(item.expr, Expr::Ident(_))),
        "a property of `this`: {items:?}"
    );
    assert!(
        matches!(condition, Expr::Binary { op: crate::ir::BinaryOp::Ne, right, .. }
            if matches!(right.as_ref(), Expr::Ident(cg) if *cg == handle.name)),
        "{condition:?}"
    );
    assert_eq!(then_body, &[Stmt::Break]);
    assert!(matches!(args.as_slice(), [Some(Expr::Ident(object))] if *object == this.name));
    // The constructions that a declaration of a module makes through other
    // initializers, and those of a package type, are accepted.
    for (before, items) in [
        (
            "",
            "bit clk; covergroup cg @(posedge clk); endgroup cg a[2] = '{new, new};",
        ),
        (
            "",
            "bit clk; covergroup cg @(posedge clk); endgroup \
             class K; cg h; function new(); h = new; endfunction endclass K k = new;",
        ),
        (
            "",
            "class K; bit clk; covergroup cg @(posedge clk); endgroup \
             function new(); cg = new; endfunction endclass K k = new;",
        ),
        (
            "package p; bit clk; covergroup cg @(posedge clk); endgroup endpackage",
            "p::cg c = new;",
        ),
        (
            "package p; bit clk; covergroup cg @(posedge clk); endgroup endpackage",
            "real r; initial r = p::cg::get_coverage();",
        ),
        (
            "bit clk; covergroup cg @(posedge clk); endgroup",
            "cg c = new; cg d; initial d = new;",
        ),
    ] {
        let source = format!("{before} module Top; {items} endmodule");
        analyze(&source).unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

#[test]
fn constructions_that_would_start_a_sampler_where_no_process_may_start_are_rejected() {
    // The sampler of an instance of a covergroup embedded in a class, or of
    // a type of a package or the compilation unit, is a process its
    // construction starts (19.3): where a procedure may not start one, the
    // construction is rejected, directly and through the subroutines and
    // constructors it calls, with a covergroup text. The instances of a
    // type of the module join a list there (accepted below).
    let class = "bit clk, g, made; class K; covergroup kc @(posedge clk); endgroup \
                 function new(); kc = new; endfunction endclass K k; ";
    let package = "package p; bit pclk; covergroup pcg @(posedge pclk); endgroup endpackage";
    let comb = "covergroup sampling event of an instance constructed in an always_comb procedure (IEEE 1800-2023 9.2.2.2, 19.3)";
    let latch = "covergroup sampling event of an instance constructed in an always_latch procedure (IEEE 1800-2023 9.2.2.3, 19.3)";
    let assign = "covergroup sampling event of an instance constructed in a continuous assignment (IEEE 1800-2023 10.3, 19.3)";
    let ff = "covergroup sampling event of an instance constructed in an always_ff procedure (IEEE 1800-2023 9.2.2.4, 19.3)";
    let final_ = "covergroup sampling event of an instance constructed in a final procedure (IEEE 1800-2023 9.2.3, 19.3)";
    let mk = "function automatic void mk(); k = new; endfunction ";
    let mkb = "function automatic bit mkb(input bit v); if (v) k = new; return v; endfunction ";
    for (before, items, text) in [
        ("", format!("{class}always_comb if (g) k = new;"), comb),
        ("", format!("{class}{mk}always_comb if (g) mk();"), comb),
        ("", format!("{class}{mk}always_latch if (g) mk();"), latch),
        ("", format!("{class}{mkb}wire w = mkb(g);"), assign),
        ("", format!("{class}{mkb}assign made = mkb(g);"), assign),
        (
            "",
            format!("{class}always_ff @(posedge clk) if (!made) begin k = new; made <= 1; end"),
            ff,
        ),
        (
            "",
            format!("{class}{mk}always_ff @(posedge clk) if (!made) begin mk(); made <= 1; end"),
            ff,
        ),
        ("", format!("{class}final begin k = new; end"), final_),
        ("", format!("{class}{mk}final mk();"), final_),
        (
            "",
            format!(
                "{class}class W; function void mk(); k = new; endfunction endclass final begin W w; w = new; w.mk(); end"
            ),
            final_,
        ),
        (
            package,
            "bit g; p::pcg x; always_comb if (g) x = new;".to_owned(),
            comb,
        ),
        (package, "p::pcg x; final x = new;".to_owned(), final_),
    ] {
        let error = rejected(before, &items);
        assert_eq!(
            error,
            AnalyzerError::Unsupported(text.to_string()),
            "{before} {items}"
        );
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE, "{items}");
    }
    // A type of the module: its instances join the list of the type, in
    // every procedure, also a final one.
    let module = "bit clk, g, made; covergroup cg @(posedge clk); endgroup cg c; \
                  function automatic void mk(); c = new; endfunction ";
    for items in [
        "always_comb if (g) mk();",
        "function automatic bit mkb(input bit v); if (v) c = new; return v; endfunction wire w = mkb(g);",
        "always_ff @(posedge clk) if (!made) begin c = new; made <= 1; end",
        "final c = new;",
        "final mk();",
    ] {
        let source = format!("module Top; {module}{items} endmodule");
        analyze(&source).unwrap_or_else(|error| panic!("{items}: {error}"));
    }
    // 19.4's example shape with a value member, `@(m_obj.m_ev)` of an `int`
    // `m_ev`: an event on a member of an object a handle property holds,
    // which event expressions do not support. Through `iff` as well; a
    // named event of such an object is supported.
    let helper = "class Helper; int m_ev; bit on; event e; endclass ";
    let through = "covergroup sampling event on a member of an object that a handle property holds (IEEE 1800-2023 9.4.2, 19.3)";
    for event in ["@(m_obj.m_ev)", "@(posedge clk iff m_obj.on)"] {
        let items = format!(
            "bit clk; {helper}class MyClass; Helper m_obj; covergroup Cov {event}; endgroup \
             function new(); m_obj = new; Cov = new; endfunction endclass MyClass m; initial m = new;"
        );
        let error = rejected("", &items);
        assert_eq!(error, AnalyzerError::Unsupported(through.into()), "{event}");
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
    }
    analyze(&format!(
        "module Top; {helper}class MyClass; Helper m_obj; covergroup Cov @(m_obj.e); endgroup \
         function new(); m_obj = new; Cov = new; endfunction endclass MyClass m; initial m = new; endmodule"
    ))
    .unwrap();
}

/// The calls of `N::$sample` in `stmts`, each with its actuals.
fn sample_calls(stmts: &[crate::ir::Stmt]) -> Vec<Vec<Option<crate::ir::Expr>>> {
    let mut found = Vec::new();
    for stmt in stmts {
        stmt.walk(&mut |stmt| {
            if let crate::ir::Stmt::Call { name, args, .. } = stmt
                && name.ends_with("::$sample")
            {
                found.push(args.clone());
            }
        });
    }
    found
}

/// The directions of the formals of the subroutine of `module` whose name
/// ends in `suffix`.
fn directions(
    module: &crate::ir::Module,
    suffix: &str,
) -> Vec<(String, crate::procedural::ParamDirection)> {
    let subroutine = module
        .subroutines()
        .iter()
        .find(|subroutine| {
            subroutine.name.starts_with("$covergroup::") && subroutine.name.ends_with(suffix)
        })
        .unwrap_or_else(|| panic!("no subroutine `{suffix}`"));
    subroutine
        .params
        .iter()
        .map(|param| (param.source_name.clone(), param.direction))
        .collect()
}

#[test]
fn reference_formals_read_the_variables_of_their_declarations() {
    use crate::ir::Expr;
    use crate::procedural::ParamDirection::{ConstRef, Input};
    let ident = |name: &str| Some(Expr::Ident(name.into()));
    // 19.3: "A ref argument tracks the value of the actual argument as it
    // changes", and "will be treated the same as a read-only const ref
    // argument". `N::$new` and `N::$sample` take the `ref` formals as
    // `const ref` ones, `N::$sample` after the handle and before the
    // formals of `sample`; an input formal stays a value the instance
    // keeps. The sampler of each declaration passes its own actuals, an
    // unpacked array included, and the type has no `N::$spawn`, which no
    // other `new` could call.
    let ir = analyze(
        "module Top; bit clk; logic [3:0] bus_a, bus_b; logic [3:0] bus_event[0:7]; \
         assign bus_a = 4'd1; assign bus_b = 4'd2; \
         covergroup cg(ref logic [3:0] v, input int k, const ref logic [3:0] e[0:7]) @(posedge clk); \
         coverpoint v; coverpoint e[k]; endgroup \
         cg x = new(bus_a, 1, bus_event), y = new(bus_b, 2, bus_event); \
         covergroup change(ref logic [3:0] v) @(bus_a); coverpoint v; endgroup \
         change z = new(bus_b); \
         endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert_eq!(
        directions(top, "cg::$new"),
        [
            ("v".to_owned(), ConstRef),
            ("k".to_owned(), Input),
            ("e".to_owned(), ConstRef)
        ]
    );
    assert_eq!(
        directions(top, "cg::$sample"),
        [
            ("$cg".to_owned(), Input),
            ("v".to_owned(), ConstRef),
            ("e".to_owned(), ConstRef)
        ]
    );
    assert!(subroutines_ending(&ir, "::$spawn").is_empty());
    let handles = site_handles(top);
    assert_eq!(handles.len(), 3, "{handles:?}");
    let edge_procedures = edge_samplers(top);
    let [edges] = edge_procedures.as_slice() else {
        panic!("{edge_procedures:?}");
    };
    assert_eq!(
        sample_calls(edges.body()),
        [
            vec![ident(handles[0]), ident("bus_a"), ident("bus_event")],
            vec![ident(handles[1]), ident("bus_b"), ident("bus_event")],
        ]
    );
    let processes = samplers(top);
    let [process] = processes.as_slice() else {
        panic!("{processes:?}");
    };
    assert_eq!(
        sample_calls(process.body()),
        [vec![ident(handles[2]), ident("bus_b")]]
    );
    // A type without a clocking event, 19.8's example shape: `sample()`
    // through the variable of the declaration passes the actuals of the
    // declaration, from any procedure, task, function or method of a class
    // of the scope, also one bound before the declaration; a type with no
    // event needs no hidden variable.
    let ir = analyze(
        "module Top; int x, y; bit clk; \
         function automatic void early(); a.sample(); endfunction \
         covergroup cg(ref int v); coverpoint v; endgroup \
         cg a = new(x), b = new(y); \
         task automatic both(); a.sample(); b.sample(); endtask \
         class K; function void go(); b.sample(); endfunction endclass \
         always @(posedge clk) a.sample(); \
         always_comb b.sample(); \
         final a.sample(); \
         initial begin both(); early(); end \
         endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    assert!(site_handles(top).is_empty());
    assert!(samplers(top).is_empty() && edge_samplers(top).is_empty());
    let mut calls = top
        .subroutines()
        .iter()
        .filter(|subroutine| !subroutine.name.starts_with("$covergroup::"))
        .flat_map(|subroutine| sample_calls(&subroutine.body))
        .chain(
            top.ff_processes()
                .iter()
                .flat_map(|process| sample_calls(process.body())),
        )
        .chain(
            top.comb_processes()
                .iter()
                .flat_map(|process| sample_calls(process.body())),
        )
        .chain(
            top.initial_processes()
                .iter()
                .flat_map(|process| sample_calls(process.body())),
        )
        .collect::<Vec<_>>();
    calls.sort_by_key(|args| format!("{args:?}"));
    calls.dedup();
    assert_eq!(
        calls,
        [vec![ident("a"), ident("x")], vec![ident("b"), ident("y")]]
    );
    // A declaration of a generate block, whose actual is an element of an
    // unpacked array, or a variable of the block.
    let ir = analyze(
        "module Top; bit clk; logic [3:0] bus_event[0:7]; \
         covergroup cg(ref logic [3:0] v) @(posedge clk); coverpoint v; endgroup \
         for (genvar i = 0; i < 2; i++) begin : g cg c = new(bus_event[i]); end \
         if (1) begin : h logic [3:0] w; cg c = new(w); end \
         endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let edge_procedures = edge_samplers(top);
    let [edges] = edge_procedures.as_slice() else {
        panic!("{edge_procedures:?}");
    };
    let calls = sample_calls(edges.body());
    assert_eq!(calls.len(), 3, "{calls:?}");
    for (call, index) in calls.iter().zip(["0", "1"]) {
        let [_, Some(Expr::Select { expr, .. })] = call.as_slice() else {
            panic!("{call:?}");
        };
        assert_eq!(**expr, Expr::Ident("bus_event".into()), "{index}: {call:?}");
    }
    assert!(
        matches!(&calls[2][1], Some(Expr::Ident(name)) if name.starts_with('h') && name.ends_with('w')),
        "{:?}",
        calls[2]
    );
    // A `ref` formal of an overridden `sample` method is a `const ref` one,
    // whose actual each call passes, through any handle.
    let ir = analyze(
        "module Top; int x; \
         covergroup sf with function sample(ref int s); coverpoint s; endgroup \
         sf e = new; sf h; initial begin h = e; e.sample(x); h.sample(x); end endmodule",
    )
    .unwrap();
    assert_eq!(
        directions(module(&ir, "Top"), "sf::$sample"),
        [("$cg".to_owned(), Input), ("s".to_owned(), ConstRef)]
    );
}

#[test]
fn reference_formals_outside_declarations_are_rejected() {
    // Everything else with a `ref` formal is rejected where it is used,
    // with a text that maps to the tracking issue: a `new` that is not the
    // initializer of a declaration of a module or a program, `sample()`
    // through anything but the variable of such a declaration, and a write
    // of that variable but for its initializer. The methods and options
    // other than `sample` stay legal through any handle.
    let types = "bit clk; int x, y; \
                 covergroup cg(ref int v); coverpoint v; endgroup \
                 covergroup clocked(ref int v) @(posedge clk); coverpoint v; endgroup ";
    let outside =
        "covergroup ref argument outside a declaration with an initializer (IEEE 1800-2023 19.3)";
    let elsewhere = "covergroup ref argument of an instance sampled through a variable other than its declaration (IEEE 1800-2023 19.3, 19.8)";
    let written = |variable: &str| {
        format!(
            "covergroup ref argument of an instance whose variable `{variable}` is written outside its declaration (IEEE 1800-2023 19.3)"
        )
    };
    for (before, items, text) in [
        (
            "",
            "cg c; initial c = new(x);".to_owned(),
            outside.to_owned(),
        ),
        (
            "",
            "clocked c; initial c = new(x);".to_owned(),
            outside.to_owned(),
        ),
        (
            "",
            "cg c = new(x); initial c = new(y);".to_owned(),
            outside.to_owned(),
        ),
        (
            "",
            "function automatic cg make(); cg c = new(x); return c; endfunction".to_owned(),
            outside.to_owned(),
        ),
        (
            "",
            "cg a[2] = '{new(x), new(y)};".to_owned(),
            outside.to_owned(),
        ),
        (
            "",
            "class K; cg c; function new(); c = new(x); endfunction endclass K k = new;".to_owned(),
            outside.to_owned(),
        ),
        // Embedded in a class (19.4): constructed by `new` of the class
        // alone.
        (
            "",
            "class K; int m; covergroup e(ref int v); coverpoint v; endgroup \
             function new(); e = new(m); endfunction endclass K k = new;"
                .to_owned(),
            outside.to_owned(),
        ),
        // A type of a package, which a declaration of a module constructs:
        // a site is a declaration of the scope of its type.
        (
            "package p; covergroup pcg(ref int v); coverpoint v; endgroup endpackage",
            "p::pcg c = new(x);".to_owned(),
            outside.to_owned(),
        ),
        (
            "",
            "cg c = new(x); cg d; initial begin d = c; d.sample(); end".to_owned(),
            elsewhere.to_owned(),
        ),
        (
            "",
            "cg c = new(x); cg a[2]; initial a[0].sample();".to_owned(),
            elsewhere.to_owned(),
        ),
        (
            "",
            "cg c = new(x); function automatic void f(); cg c; c.sample(); endfunction".to_owned(),
            elsewhere.to_owned(),
        ),
        (
            "",
            "cg c = new(x); task automatic t(cg c); c.sample(); endtask".to_owned(),
            elsewhere.to_owned(),
        ),
        (
            "",
            "cg c; initial c.sample();".to_owned(),
            elsewhere.to_owned(),
        ),
        (
            "",
            "for (genvar i = 0; i < 1; i++) begin : g cg c = new(x); end initial g[0].c.sample();"
                .to_owned(),
            elsewhere.to_owned(),
        ),
        (
            "",
            "covergroup cd(ref int v = x); coverpoint v; endgroup cd c = new;".to_owned(),
            "covergroup ref argument `v` that takes its default (IEEE 1800-2023 13.5.3, 19.3)"
                .to_owned(),
        ),
        (
            "",
            "cg c = new(x); initial c = null;".to_owned(),
            written("c"),
        ),
        (
            "",
            "cg c = new(x), d = new(y); initial c = d;".to_owned(),
            written("c"),
        ),
        (
            "",
            "clocked c = new(x); final c = null;".to_owned(),
            written("c"),
        ),
        (
            "",
            "cg c = new(x); task automatic t(output cg h); h = null; endtask initial t(c);"
                .to_owned(),
            written("c"),
        ),
        (
            "",
            "cg c = new(x); function automatic void f(); c = null; endfunction".to_owned(),
            written("c"),
        ),
    ] {
        let error = rejected(before, &format!("{types}{items}"));
        assert_eq!(error, AnalyzerError::Unsupported(text), "{before} {items}");
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE, "{items}");
    }
    for items in [
        "cg c = new(x); cg d; real r; initial begin d = c; r = d.get_inst_coverage(); \
         r = cg::get_coverage(); d.stop(); d.start(); d.option.weight = 2; \
         d.set_inst_name(\"n\"); end",
        "cg c = new(x); cg a[2]; real r; initial begin a[0] = c; r = a[0].get_coverage(); end",
        "cg c; real r; initial r = c.get_inst_coverage();",
        // A member of an unpacked structure is a variable (13.5.2).
        "typedef struct { int m; } s_t; s_t s; cg c = new(s.m); initial c.sample();",
    ] {
        let source = format!("module Top; {types}{items} endmodule");
        analyze(&source).unwrap_or_else(|error| panic!("{items}: {error}"));
    }
    // What 13.5.2 does not allow to pass by reference is an invalid
    // covergroup: a net, a select of one, an expression, a constant, an
    // actual of another type.
    for (items, detail) in [
        ("wire [3:0] w; lg c = new(w);", "is a net"),
        ("wire [3:0] w[2]; lg c = new(w[0]);", "is a net"),
        ("logic [3:0] w; lg c = new(w);", ""),
        ("cg c = new(x + 1);", "only a variable"),
        ("cg c = new(3);", "only a variable"),
        (
            "localparam int P = 1; cg c = new(P);",
            "a constant is not a variable",
        ),
        ("shortint s; cg c = new(s);", "not equivalent"),
        ("bit [31:0] b; cg c = new(b[3:0]);", "only a variable"),
    ] {
        let lg = "covergroup lg(ref logic [3:0] v); coverpoint v; endgroup ";
        if detail.is_empty() {
            let source = format!("module Top; {types}{lg}{items} endmodule");
            analyze(&source).unwrap_or_else(|error| panic!("{items}: {error}"));
            continue;
        }
        let error = rejected("", &format!("{types}{lg}{items}"));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{items}: {error:?}");
        };
        let display = if items.contains("lg c") {
            "Top.lg"
        } else {
            "Top.cg"
        };
        assert!(
            text.contains(&format!("ref argument `v` of covergroup `{display}`"))
                && text.contains(detail),
            "{items}: {text}"
        );
    }
    // What the analysis does not pass by reference anywhere.
    for (before, items, construct) in [
        (
            "module child; int sig; endmodule",
            "child u(); cg c = new(u.sig);",
            "covergroup ref argument bound to a structure member or hierarchical name",
        ),
        (
            "",
            "int q[$]; cg c = new(q[0]);",
            "covergroup ref argument bound to an element of a dynamic array",
        ),
        // A sample reads the actual as the declaration bound it: a
        // property through the handle, an element at the index, of the
        // time of the sample, where the reference is the one of `new`.
        (
            "",
            "class K; int m; endclass K k = new; cg c = new(k.m);",
            "covergroup ref argument bound to a class property or to an element at an index that is not constant",
        ),
        (
            "",
            "int arr[4]; int i; cg c = new(arr[i]);",
            "covergroup ref argument bound to a class property or to an element at an index that is not constant",
        ),
        // Formals of other types than integral ones and fixed-size arrays
        // of them.
        (
            "",
            "covergroup sr(ref string s); endgroup string t; sr c = new(t);",
            "covergroup ref argument of a type that is neither integral nor a fixed-size array",
        ),
        (
            "",
            "covergroup sq(ref int s[$]); endgroup int t[$]; sq c = new(t);",
            "covergroup ref argument of a type that is neither integral nor a fixed-size array",
        ),
        (
            "",
            "typedef struct { int m; } s_t; covergroup ss(ref s_t s); endgroup s_t t; ss c = new(t);",
            "covergroup ref argument of a type that is neither integral nor a fixed-size array",
        ),
    ] {
        let error = rejected(before, &format!("{types}{items}"));
        let AnalyzerError::Unsupported(text) = &error else {
            panic!("{items}: {error:?}");
        };
        assert!(text.starts_with(construct), "{items}: {text}");
        assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE, "{items}");
    }
    // Caliptra's actuals: `logic` variables that continuous assignments
    // drive, and an unpacked array of them, are variables (6.5).
    analyze(
        "module Top; logic [3:0] bus_x; logic [3:0] bus_event[0:7]; bit d; \
         assign bus_x = {4{d}}; for (genvar i = 0; i < 8; i++) assign bus_event[i] = 4'(i); \
         covergroup cg(ref logic [3:0] x, ref logic [3:0] e[0:7]); coverpoint x; coverpoint e[3]; endgroup \
         cg c = new(bus_x, bus_event); initial c.sample(); endmodule",
    )
    .unwrap();
}

#[test]
fn covergroup_expressions_do_not_read_reference_arguments() {
    // 19.5: "Only constant expressions (see 11.2.1), global and instance
    // constants ..., or non-ref arguments to the covergroup are allowed to be
    // used as variables in a covergroup_expression": a bin value, a bin
    // array size, a `with` filter over any range (19.5.1.1 evaluates it "at
    // the time the covergroup instance is constructed"), a set expression
    // and an `intersect` list. The same with an input argument analyzes;
    // an option (`expression` in Syntax 19-1) and a guard may read either.
    let declaration = |direction: &str, items: &str| {
        format!(
            "bit clk; int x = 5; int y = 1; int w = 0; int q[4]; \
             covergroup cg({direction} int r) @(posedge clk); {items} endgroup cg c = new(w);"
        )
    };
    for (items, place) in [
        (
            "big: coverpoint x { bins near = {[0:$]} with (item >= r); }",
            "bin `near` of coverpoint `big`",
        ),
        (
            "sm: coverpoint x { bins near = {[0:15]} with (item >= r); }",
            "bin `near` of coverpoint `sm`",
        ),
        (
            "big: coverpoint x { bins near = big with (item >= r); }",
            "bin `near` of coverpoint `big`",
        ),
        (
            "cx: coverpoint x { bins b = {r}; }",
            "bin `b` of coverpoint `cx`",
        ),
        (
            "cx: coverpoint x { bins b[r] = {[0:7]}; }",
            "bin `b` of coverpoint `cx`",
        ),
        (
            "cx: coverpoint x { bins b = {[0:7]}; } cy: coverpoint y { bins b = {[0:7]}; } \
             xy: cross cx, cy { bins s = binsof(cx) intersect {r}; }",
            "bin `s` of cross `xy`",
        ),
    ] {
        let error = rejected("", &declaration("ref", items));
        let AnalyzerError::InvalidCovergroup(text) = &error else {
            panic!("{items}: {error:?}");
        };
        assert!(
            text.contains(&format!("`r` in {place}"))
                && text.contains(
                    "is a ref argument of the covergroup, which a covergroup_expression cannot read (IEEE 1800-2023 19.5)"
                ),
            "{items}: {text}"
        );
        let source = format!("module Top; {} endmodule", declaration("input", items));
        analyze(&source).unwrap_or_else(|error| panic!("{items}: {error}"));
    }
    // A set expression of a `ref` array argument.
    let error = rejected(
        "",
        "bit clk; int x; int q[4]; covergroup cg(ref int s[4]) @(posedge clk); \
         cx: coverpoint x { bins b[] = s; } endgroup cg c = new(q);",
    );
    assert!(
        matches!(&error, AnalyzerError::InvalidCovergroup(text) if text.contains("`s` in bin `b`")),
        "{error:?}"
    );
    for items in [
        "option.weight = r; cx: coverpoint x;",
        "cx: coverpoint x iff (r > 0) { bins b = {[0:7]} iff (r > 1); }",
    ] {
        let source = format!("module Top; {} endmodule", declaration("ref", items));
        analyze(&source).unwrap_or_else(|error| panic!("{items}: {error}"));
    }
}

#[test]
fn errors_of_covergroup_arguments_name_the_covergroup() {
    // The subroutines of a covergroup are no names of the source: what their
    // calls and bodies are rejected for names the method and the covergroup.
    let leaks = |error: &AnalyzerError| error.to_string().contains("$covergroup");
    // The arguments of `new` and of `sample` (13.5.3, 13.5.4).
    let types = "int x; covergroup cg(input int k, ref int v) with function sample(int s); \
                 coverpoint v; coverpoint s; endgroup covergroup pg(input int k); endgroup ";
    for (items, method, detail) in [
        ("cg c = new;", "new", "no actual for argument `k`"),
        (
            "pg c; initial c = new;",
            "new",
            "no actual for argument `k`",
        ),
        ("cg c = new(1, x, 2);", "new", "3 actuals for 2 formals"),
        ("cg c = new(.k(1), .z(x));", "new", "no formal named `z`"),
        ("cg c = new(.k(1), .k(2), .v(x));", "new", "`k`"),
        (
            "cg c = new(1, x); initial c.sample(1, 2);",
            "sample",
            "2 actuals for 1 formals",
        ),
        (
            "cg c = new(1, x); initial c.sample();",
            "sample",
            "no actual for argument `s`",
        ),
    ] {
        let error = rejected("", &format!("{types}{items}"));
        let AnalyzerError::InvalidCallArguments {
            subroutine,
            detail: found,
        } = &error
        else {
            panic!("{items}: {error:?}");
        };
        let display = if items.starts_with("pg") {
            "Top.pg"
        } else {
            "Top.cg"
        };
        assert_eq!(
            *subroutine,
            format!("{method}` of covergroup `{display}"),
            "{items}"
        );
        assert!(found.contains(detail), "{items}: {found}");
        assert!(
            error.to_string().starts_with(&format!(
                "arguments of `{method}` of covergroup `{display}`: "
            )),
            "{items}: {error}"
        );
        assert!(!leaks(&error), "{items}: {error}");
    }
    // A net passed to a `ref` argument of `sample` (13.5.2), of a covergroup
    // of the module and of one embedded in a class.
    for (items, display) in [
        (
            "wire [3:0] w; covergroup sf with function sample(ref logic [3:0] s); coverpoint s; \
             endgroup sf e = new; initial e.sample(w);",
            "Top.sf",
        ),
        (
            "wire [3:0] w; class K; covergroup kc with function sample(ref logic [3:0] s); \
             coverpoint s; endgroup function new(); kc = new; endfunction endclass \
             K k = new; initial k.kc.sample(w);",
            "K::kc",
        ),
    ] {
        let error = rejected("", items);
        assert_eq!(
            error,
            AnalyzerError::InvalidRefArgument {
                subroutine: format!("sample` of covergroup `{display}"),
                argument: "s".into(),
                detail: "a net cannot be passed by reference (IEEE 1800-2023 13.5.2)".into(),
            },
            "{items}"
        );
        assert!(!leaks(&error), "{error}");
    }
    // An expression of the covergroup that writes a `ref` argument of `new`
    // through a function: "a covergroup cannot modify any argument to the new
    // operator, a ref argument will be treated the same as a read-only const
    // ref argument" (19.3). One of `sample`, which the analysis passes as a
    // `const ref` argument too, is not supported written.
    let bump = "function automatic logic [3:0] bump(ref logic [3:0] r); r = r + 1; return r; \
                endfunction logic [3:0] a; ";
    let error = rejected(
        "",
        &format!(
            "{bump}covergroup cg(ref logic [3:0] v); coverpoint bump(v); endgroup cg c = new(a);"
        ),
    );
    assert_eq!(
        error,
        AnalyzerError::InvalidCovergroup(
            "ref argument `v` of covergroup `Top.cg` is written by an expression of the covergroup, which cannot modify an argument of `new`: a ref argument is read-only, as a const ref argument is (IEEE 1800-2023 19.3)"
                .into()
        )
    );
    let error = rejected(
        "",
        &format!(
            "{bump}covergroup sf with function sample(ref logic [3:0] s); coverpoint bump(s); \
             endgroup sf e = new; initial e.sample(a);"
        ),
    );
    assert_eq!(
        error,
        AnalyzerError::Unsupported(
            "covergroup ref argument `s` of the sample method of `Top.sf` written by an expression of the covergroup (IEEE 1800-2023 19.8.1)"
                .into()
        )
    );
    assert_eq!(error.tracking_issue(), COVERGROUPS_ISSUE);
}

/// Operand `operand` of a bin as `constant_ranges` shows it.
fn operand_text(operand: &celox_heap::coverage::Operand) -> String {
    use celox_heap::coverage::Operand;
    match operand {
        Operand::Constant(constant) if constant.mask.bits() != 0 => {
            format!("{:b}/{:b}", constant.value, constant.mask)
        }
        Operand::Constant(constant)
            if constant.ty.signed && constant.value.bit(u64::from(constant.ty.width) - 1) =>
        {
            let modulus = num_bigint::BigInt::from(1) << constant.ty.width;
            (num_bigint::BigInt::from(constant.value.clone()) - modulus).to_string()
        }
        Operand::Constant(constant) => constant.value.to_string(),
        Operand::Input(index) => format!("input{index}"),
    }
}

/// The transitions of `bin` as 19.5.2 writes them, with the counts of each
/// repetition as a range: `(1,5 => 6[*2:2]), (3[->1:1])`.
fn transition_text(bin: &celox_heap::coverage::BinDescriptor) -> String {
    use celox_heap::coverage::{BinValues, Bound, RangeDescriptor, Repeat};
    let BinValues::Transitions(sequences) = &bin.values else {
        panic!("bin `{}` is no transition bin: {:?}", bin.name, bin.values);
    };
    let bound = |bound: &Bound| match bound {
        Bound::Open => "$".to_string(),
        Bound::Operand(value) => operand_text(value),
    };
    sequences
        .iter()
        .map(|steps| {
            let steps: Vec<String> = steps
                .iter()
                .map(|step| {
                    let values: Vec<String> = step
                        .values
                        .iter()
                        .map(|value| match value {
                            RangeDescriptor::Value(value) => operand_text(value),
                            RangeDescriptor::Range { low, high } => {
                                format!("[{}:{}]", bound(low), bound(high))
                            }
                        })
                        .collect();
                    let repeat = |kind: &str, low, high| {
                        format!("[{kind}{}:{}]", operand_text(low), operand_text(high))
                    };
                    let repeat = match &step.repeat {
                        Repeat::Once => String::new(),
                        Repeat::Consecutive(low, high) => repeat("*", low, high),
                        Repeat::Goto(low, high) => repeat("->", low, high),
                        Repeat::NonConsecutive(low, high) => repeat("=", low, high),
                    };
                    format!("{}{repeat}", values.join(","))
                })
                .collect();
            format!("({})", steps.join(" => "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[test]
fn transition_bins_compile_to_the_descriptor_of_the_type() {
    use celox_heap::coverage::{BinArray, BinKind, BinValues, InputDescriptor, Operand, ValueType};
    // 19.5.2, 19.5.5, 19.5.6: each form of a transition bin, with values
    // and counts that are constants, parameters, enumerators, arguments of
    // the covergroup and calls of functions of these, which `new`
    // evaluates.
    let ir = analyze(
        "module Top; bit [3:0] v; bit en; \
         typedef enum bit [3:0] { IDLE, RD = 5, WR = 9 } st_t; st_t s; \
         localparam int P = 4; localparam int S [3] = '{1, 2, 3}; \
         function automatic int twice(int x); return 2 * x; endfunction \
         covergroup cg(int a, int n); \
         coverpoint v { \
           bins one = (1 => 2); \
           bins sets = (1, 5 => 6, 7); \
           bins lists = ([7:9], 10 => 11), (4 => 5 => 6); \
           bins rep = (3 [* 5]); \
           bins reps = (3 [* 3:5]); \
           bins go = (1 => 3 [-> 3] => 5); \
           bins non = (1 => 3 [= 2:4] => 6); \
           bins arg = (a => P => twice(a) [* n]); \
           bins arr[] = (1, 2 => 3 [* 1:2]); \
           wildcard bins w = (4'b11?? => 4'b00??); \
           ignore_bins ig = (2 => 3); \
           illegal_bins bad = (4 => 5 => 6) iff (en); \
           bins other = default sequence; \
           illegal_bins never = default sequence; \
           bins set = (S); \
           bins each[] = (S); \
           bins two[2] = (S); \
         } \
         coverpoint s { bins t = (IDLE => RD, WR); } \
         endgroup \
         cg c; initial begin c = new(1, 2); c.sample(); end endmodule",
    )
    .unwrap();
    let top = module(&ir, "Top");
    let descriptor = descriptor(top, "Top.cg");
    let [v, s] = descriptor.points.as_slice() else {
        panic!("{:?}", descriptor.points);
    };
    let text = |name| transition_text(bin(v, name));
    assert_eq!(text("one"), "(1 => 2)");
    assert_eq!(text("sets"), "(1,5 => 6,7)");
    assert_eq!(text("lists"), "([7:9],10 => 11), (4 => 5 => 6)");
    assert_eq!(text("rep"), "(3[*5:5])");
    assert_eq!(text("reps"), "(3[*3:5])");
    assert_eq!(text("go"), "(1 => 3[->3:3] => 5)");
    assert_eq!(text("non"), "(1 => 3[=2:4] => 6)");
    // `a`, `twice(a)` and `n` are evaluated by `new`, each an input.
    assert_eq!(text("arg"), "(input0 => 4 => input1[*input2:input2])");
    assert_eq!(text("arr"), "(1,2 => 3[*1:2])");
    // `?` is z: the bits of the mask.
    assert_eq!(text("w"), "(1100/11 => 0/11)");
    assert_eq!(transition_text(bin(s, "t")), "(0 => 5,9)");
    let shape = |name| {
        let bin = bin(v, name);
        (bin.kind, bin.array.clone(), bin.wildcard, bin.iff)
    };
    assert_eq!(shape("arr"), (BinKind::Bins, BinArray::Open, false, None));
    assert_eq!(shape("w"), (BinKind::Bins, BinArray::Scalar, true, None));
    assert_eq!(
        shape("ig"),
        (BinKind::Ignore, BinArray::Scalar, false, None)
    );
    assert_eq!(
        shape("bad"),
        (BinKind::Illegal, BinArray::Scalar, false, Some(0))
    );
    assert_eq!(bin(v, "other").values, BinValues::DefaultSequence);
    assert_eq!(bin(v, "other").kind, BinKind::Bins);
    assert_eq!(bin(v, "never").values, BinValues::DefaultSequence);
    assert_eq!(bin(v, "never").kind, BinKind::Illegal);
    assert_eq!(v.predicates, 1);
    // `(S)` of an array is a parenthesized set expression (19.5.1.2): the
    // elements of the parameter are the values of the bin.
    assert_eq!(
        constant_ranges(bin(v, "set")),
        (vec!["1".to_string(), "2".into(), "3".into()], None)
    );
    assert_eq!(constant_ranges(bin(v, "each")).0, ["1", "2", "3"]);
    assert_eq!(bin(v, "each").array, BinArray::Open);
    assert_eq!(constant_ranges(bin(v, "two")).0, ["1", "2", "3"]);
    assert!(
        matches!(&bin(v, "two").array, BinArray::Fixed(Operand::Constant(size)) if size.value == 2u8.into())
    );
    let int = ValueType {
        width: 32,
        signed: true,
    };
    assert_eq!(
        descriptor.inputs,
        [
            InputDescriptor::Integral(int),
            InputDescriptor::Integral(int),
            InputDescriptor::Integral(int)
        ]
    );
}

#[test]
fn transition_bins_the_standard_excludes_are_errors() {
    // What the declaration alone shows is an error where the covergroup is
    // declared, whether or not something uses it.
    for (declaration, rule) in [
        // 19.5.2: "Transition bin specifications of length 0 shall be
        // illegal": a trans_set of one step without a repetition.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1, 2); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = ([0:1]); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2), (3); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        // 19.5.2: "Transitions that specify sequences of unbounded or
        // undetermined varying length cannot be used with the multiple
        // bins construct".
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[] = (1 => 3 [-> 2]); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` is an array of transition bins with \
             a goto or nonconsecutive repetition, whose sequences are of unbounded length (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[] = (3 [= 2]); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` is an array of transition bins with \
             a goto or nonconsecutive repetition, whose sequences are of unbounded length (IEEE \
             1800-2023 19.5.2)",
        ),
        // 19.5.5, 19.5.6: "Ignored transition bins cannot specify a
        // sequence of unbounded or undetermined varying length", nor
        // illegal ones.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { ignore_bins b = (1 => 3 [-> 2]); } \
             endgroup",
            "`ignore_bins b` of coverpoint `x` of covergroup `Top.cg` is a transition with a goto \
             or nonconsecutive repetition, a sequence of unbounded length (IEEE 1800-2023 19.5.5)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { illegal_bins b = (3 [= 1:2] => 1); } \
             endgroup",
            "`illegal_bins b` of coverpoint `x` of covergroup `Top.cg` is a transition with a goto \
             or nonconsecutive repetition, a sequence of unbounded length (IEEE 1800-2023 19.5.6)",
        ),
        // 19.5: "A default bin cannot be explicitly ignored".
        (
            "bit [3:0] x; covergroup cg; coverpoint x { ignore_bins b = default sequence; } \
             endgroup",
            "`ignore_bins b` of coverpoint `x` of covergroup `Top.cg` is a default bin, which \
             cannot be ignored (IEEE 1800-2023 19.5)",
        ),
        // 19.8.1: an argument of the sample method is no operand of a bin.
        (
            "covergroup cg with function sample(bit [3:0] x, int n); \
             coverpoint x { bins b = (1 => 2 [* n]); } endgroup",
            "argument `n` of the sample method of covergroup `Top.cg` is used in a bin: it is \
             visible in coverpoint expressions and their guards only (IEEE 1800-2023 19.8.1)",
        ),
    ] {
        let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        for used in ["", "cg c; initial c = new;"] {
            let error = rejected("", &format!("{declaration} {used}"));
            assert_eq!(
                error,
                AnalyzerError::InvalidCovergroup(rule.clone()),
                "{declaration} {used}"
            );
        }
    }
    // What needs the values of the operands or their types is an error
    // where the type is used.
    for (declaration, rule) in [
        // One value alone is a parenthesized set expression where it is an
        // array (19.5.1.2), else a transition of length 0 (19.5.2).
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[2] = (1); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg(int a); coverpoint x { bins b = (a); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        // "a single covergroup_value_range with a repeat_range evaluating
        // to 1": also the minimum of a range of repetitions, which stands
        // for each of its counts.
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (0 [* 1]); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; localparam int ONE = 1; \
             covergroup cg; coverpoint x { bins b = ([0:1] [* ONE:3]); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (2 [-> 1]); } endgroup",
            "bin `b` of coverpoint `x` of covergroup `Top.cg` has a transition of length 0 (IEEE \
             1800-2023 19.5.2)",
        ),
        // 16.9.2: a count is a non-negative integer, and the minimum of a
        // range is at most its maximum.
        (
            "bit [3:0] x; localparam int M = -1; \
             covergroup cg; coverpoint x { bins b = (1 => 2 [-> 1:M]); } endgroup",
            "a repetition count of bin `b` of coverpoint `x` of covergroup `Top.cg` is not a \
             non-negative integer (IEEE 1800-2023 16.9.2, 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2 [= 4'bx1]); } endgroup",
            "a repetition count of bin `b` of coverpoint `x` of covergroup `Top.cg` is not a \
             non-negative integer (IEEE 1800-2023 16.9.2, 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2 [* 3:2]); } endgroup",
            "the repetition range 3:2 of bin `b` of coverpoint `x` of covergroup `Top.cg` has a \
             maximum below its minimum (IEEE 1800-2023 16.9.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2 [* 1.5]); } endgroup",
            "a repetition count of bin `b` of coverpoint `x` of covergroup `Top.cg` is not an \
             integral expression (IEEE 1800-2023 19.5.2)",
        ),
        // 19.5: what a covergroup_expression may read, in step values and
        // counts.
        (
            "bit [3:0] x; int lim; \
             covergroup cg; coverpoint x { bins b = (1 => 2 [* lim]); } endgroup",
            "`lim` in bin `b` of coverpoint `x` of covergroup `Top.cg` is neither a constant nor \
             an argument of the covergroup (IEEE 1800-2023 19.5)",
        ),
        (
            "bit [3:0] x; covergroup cg(ref bit [3:0] r); \
             coverpoint r { bins b = (r => 1); } endgroup",
            "`r` in bin `b` of coverpoint `r` of covergroup `Top.cg` is a ref argument of the \
             covergroup, which a covergroup_expression cannot read (IEEE 1800-2023 19.5)",
        ),
        (
            "bit [3:0] x; \
             function automatic int f(output int o); o = 1; return 2; endfunction \
             int k; covergroup cg; coverpoint x { bins b = (1 => 2 [* f(k)]); } endgroup",
            "`k` in bin `b` of coverpoint `x` of covergroup `Top.cg` is neither a constant nor \
             an argument of the covergroup (IEEE 1800-2023 19.5)",
        ),
        (
            "bit [3:0] x; \
             function automatic int f(output int o); o = 1; return 2; endfunction \
             covergroup cg(int k); coverpoint x { bins b = (f(k) => 2); } endgroup",
            "the function called in bin `b` of coverpoint `x` of covergroup `Top.cg` has an \
             output, inout or ref argument (IEEE 1800-2023 19.5)",
        ),
    ] {
        analyze(&format!("module Top; {declaration} cg c; endmodule"))
            .unwrap_or_else(|error| panic!("{declaration}: {error}"));
        // A `ref` argument is that of a declaration (19.3).
        let used = if declaration.contains("cg(ref") {
            "cg c = new(x);"
        } else if declaration.contains("cg(int") {
            "cg c; initial c = new(1);"
        } else {
            "cg c; initial c = new;"
        };
        let error = rejected("", &format!("{declaration} {used}"));
        let rule = rule.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            error,
            AnalyzerError::InvalidCovergroup(rule),
            "{declaration}"
        );
    }
    // The forms A.2.11 excludes are errors of the parser.
    for declaration in [
        "bins b[2] = (1 => 2);",
        "bins b[] = default sequence;",
        "wildcard bins b = default sequence;",
        "bins b = (1 => 2 [* 1:$]);",
    ] {
        let source = format!(
            "module Top; bit [3:0] x; covergroup cg; coverpoint x {{ {declaration} }} endgroup \
             endmodule"
        );
        let error = analyze(&source).expect_err(&source);
        assert!(
            !matches!(
                error,
                AnalyzerError::InvalidCovergroup(_) | AnalyzerError::Unsupported(_)
            ),
            "{declaration}: {error}"
        );
    }
    // A bounded repetition range in a bin array is legal (19.5.2: only the
    // goto and the nonconsecutive repetitions are unbounded), as are a
    // repetition range from 1 after another step and, in an ignored bin,
    // a consecutive one.
    analyze(
        "module Top; bit [3:0] x; covergroup cg; coverpoint x { \
         bins b[] = (3 [* 3:5]); bins c = (1 => 2 [-> 1:2]); ignore_bins i = (1 => 2 [* 1:2]); \
         } endgroup cg c; initial begin c = new; c.sample(); end endmodule",
    )
    .unwrap();
}
