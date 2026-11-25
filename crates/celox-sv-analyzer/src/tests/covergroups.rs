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
    let elaborated = |width: i128| {
        let ir = analyze_source_module_with_packages(
            parameterized,
            Path::new("covergroups.sv"),
            "M",
            &HashMap::from_iter([("W".to_string(), width)]),
            &packages,
            &mut packages.design(),
        )
        .unwrap();
        constructors(module(&ir, "M"))
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(elaborated(2), elaborated(2));
    assert_ne!(elaborated(2), elaborated(3));
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
            "bit x; covergroup cg; coverpoint x; endgroup",
            "covergroup coverpoint (IEEE 1800-2023 19.5)",
        ),
        (
            "bit x, y; covergroup cg; cross x, y; endgroup",
            "covergroup cross (IEEE 1800-2023 19.6)",
        ),
        (
            "covergroup cg; option.per_instance = 1; endgroup",
            "covergroup option (IEEE 1800-2023 19.7)",
        ),
        (
            "covergroup cg; type_option.weight = 2; endgroup",
            "covergroup option (IEEE 1800-2023 19.7)",
        ),
        (
            "bit clk; covergroup cg @(posedge clk); endgroup",
            "covergroup sampling event (IEEE 1800-2023 19.3)",
        ),
        (
            "task automatic t; endtask covergroup cg @@(begin t); endgroup",
            "covergroup block event (IEEE 1800-2023 19.3)",
        ),
        (
            "covergroup cg(ref int v); endgroup",
            "covergroup ref argument (IEEE 1800-2023 19.3)",
        ),
        (
            "covergroup cg(int v[2]); endgroup",
            "covergroup argument of unpacked type (IEEE 1800-2023 19.3)",
        ),
        (
            "covergroup cg with function sample(ref int v); endgroup",
            "covergroup ref argument of the sample method (IEEE 1800-2023 19.8.1)",
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
        ("initial c.start();", "covergroup method `start`"),
        ("initial c.stop();", "covergroup method `stop`"),
        (
            "initial c.set_inst_name(\"x\");",
            "covergroup method `set_inst_name`",
        ),
        (
            "initial r = c.get_coverage(a, b);",
            "covergroup method `get_coverage` with arguments",
        ),
        (
            "initial r = c.get_inst_coverage(a, b);",
            "covergroup method `get_inst_coverage` with arguments",
        ),
        ("initial c.option.weight = 2;", "covergroup option"),
        ("initial a = c.option.weight;", "covergroup option"),
        ("initial cg::type_option.weight = 2;", "covergroup option"),
        ("initial a = c.type_option.goal;", "covergroup option"),
        (
            "initial r = c.point.get_coverage();",
            "covergroup coverpoint or cross method",
        ),
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
