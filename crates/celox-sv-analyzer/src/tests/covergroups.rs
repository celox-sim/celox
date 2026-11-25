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
            "bit x, y; covergroup cg; cross x, y; endgroup",
            "covergroup cross (IEEE 1800-2023 19.6)",
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
    let BinValues::Ranges { ranges } = &descriptor.points[0].bins[0].values;
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
    let BinValues::Ranges { ranges } = &descriptor.points[0].bins[1].values;
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
            let BinValues::Ranges { ranges } = &point.bins[bin].values;
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
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[] = {[0:3]}; } endgroup",
            "covergroup bin array (IEEE 1800-2023 19.5.1)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b[2] = {[0:3]}; } endgroup",
            "covergroup bin array (IEEE 1800-2023 19.5.1)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {1}; bins others = default; } \
             endgroup",
            "covergroup default bins (IEEE 1800-2023 19.5)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { ignore_bins b = {1}; } endgroup",
            "covergroup ignore_bins (IEEE 1800-2023 19.5.5)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { illegal_bins b = {1}; } endgroup",
            "covergroup illegal_bins (IEEE 1800-2023 19.5.6)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { wildcard bins b = {4'b1??0}; } endgroup",
            "covergroup wildcard bins (IEEE 1800-2023 19.5.4)",
        ),
        (
            "bit [3:0] x; bit en; covergroup cg; coverpoint x { bins b = {1} iff (en); } endgroup",
            "covergroup bins with a guard (IEEE 1800-2023 19.5.1)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {[0:7]} with (item % 2 == 0); } \
             endgroup",
            "covergroup bins with a filter (IEEE 1800-2023 19.5.1.1)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = x with (item > 2); } endgroup",
            "covergroup bins with a filter (IEEE 1800-2023 19.5.1.1)",
        ),
        (
            "bit [3:0] x; localparam int S [2] = '{1, 2}; \
             covergroup cg; coverpoint x { bins b = S; } endgroup",
            "covergroup bins of a set expression (IEEE 1800-2023 19.5.1.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = {[4 +/- 1]}; } endgroup",
            "covergroup bin range with a tolerance (IEEE 1800-2023 19.5.1, 11.4.13)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = (1 => 2); } endgroup",
            "covergroup transition bins (IEEE 1800-2023 19.5.2)",
        ),
        (
            "bit [3:0] x; covergroup cg; coverpoint x { bins b = default sequence; } endgroup",
            "covergroup transition bins (IEEE 1800-2023 19.5.2)",
        ),
        (
            "bit x, y; covergroup cg; coverpoint x; coverpoint y; cross x, y; endgroup",
            "covergroup cross (IEEE 1800-2023 19.6)",
        ),
        (
            "covergroup cg; type_option.weight = 2; endgroup",
            "covergroup type_option (IEEE 1800-2023 19.7.1)",
        ),
        (
            "bit x; covergroup cg; coverpoint x { type_option.comment = \"c\"; } endgroup",
            "covergroup type_option (IEEE 1800-2023 19.7.1)",
        ),
        (
            "covergroup cg; option.detect_overlap = 1; endgroup",
            "covergroup option `detect_overlap` (IEEE 1800-2023 19.7)",
        ),
        (
            "covergroup cg; option.get_inst_coverage = 1; endgroup",
            "covergroup option `get_inst_coverage` (IEEE 1800-2023 19.7)",
        ),
        (
            "covergroup cg; option.cross_num_print_missing = 1; endgroup",
            "covergroup option `cross_num_print_missing` (IEEE 1800-2023 19.7)",
        ),
        (
            "bit x; covergroup cg; coverpoint x { option.detect_overlap = 1; } endgroup",
            "covergroup option `detect_overlap` (IEEE 1800-2023 19.7)",
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
    // 19.5 allows a signal of a clocking block as a coverpoint. Clocking
    // blocks are not supported, so such a design does not analyze.
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
