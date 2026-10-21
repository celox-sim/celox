//! Native procedure metadata selects actual process kinds and body seeds.
use super::*;
use crate::ast::{CombProcessKind, Expr, Source, Stmt};
use celox_sv_syntax::{ProcedureKind, StatementId, StatementKind};
fn parsed(process: &str) -> SourceTree {
    parse_source(
        &format!("module M(input logic clk,a,output logic[7:0]y);{process} endmodule"),
        Path::new("native-process-root.sv"),
    )
    .unwrap()
}
fn selected_body(tree: &SourceTree) -> StatementId {
    let mut body = tree.declarations.procedures[0].body;
    if let StatementKind::Timing { body: inner, .. } = tree.declarations.statement(body).kind {
        body = inner;
    }
    let StatementKind::Block { items, .. } = &tree.declarations.statement(body).kind else {
        panic!()
    };
    items[1]
}
fn only_second(statements: &[Stmt]) {
    assert!(
        matches!(statements,[Stmt::Assign{rhs:Expr::Literal(value),..}] if value=="2"),
        "{statements:?}"
    );
}
#[test]
fn native_procedure_kind_drives_actual_comb_collection_and_metadata() {
    let mut tree = parsed("always y=a;");
    tree.declarations.procedures[0].kind = ProcedureKind::AlwaysComb;
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    assert!(module.initial_processes().is_empty());
    assert_eq!(
        module.comb_processes()[0].kind(),
        CombProcessKind::AlwaysComb
    );
    let mut tree = parsed("always_comb y=a;");
    tree.declarations.procedures[0].kind = ProcedureKind::AlwaysLatch;
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(
        source.modules()[0].comb_processes()[0].kind(),
        CombProcessKind::AlwaysLatch
    );
}
#[test]
fn native_procedure_body_ids_seed_initial_comb_and_repeating_processes() {
    for keyword in ["initial", "always_comb", "always #1"] {
        let mut tree = parsed(&format!("{keyword} begin y=1;y=2;end"));
        let body = selected_body(&tree);
        tree.declarations.procedures[0].body = body;
        let source = Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        match keyword {
            "always_comb" => only_second(module.comb_processes()[0].body()),
            "initial" => only_second(module.initial_processes()[0].body()),
            _ => {
                let [Stmt::Loop { body, .. }] = module.initial_processes()[0].body() else {
                    panic!()
                };
                only_second(body);
            }
        }
    }
}
#[test]
fn native_timing_child_body_ids_seed_ff_and_wildcard_comb_bodies() {
    for keyword in ["always_ff @(posedge clk)", "always @*"] {
        let mut tree = parsed(&format!("{keyword} begin y=1;y=2;end"));
        let body = selected_body(&tree);
        let root = tree.declarations.procedures[0].body;
        let tokens = tree.declarations.statement(root).tokens;
        for statement in &mut tree.declarations.statements {
            if statement.tokens == tokens
                && let StatementKind::Timing { body: inner, .. } = &mut statement.kind
            {
                *inner = body;
            }
        }
        let source = Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        if keyword.starts_with("always_ff") {
            only_second(module.ff_processes()[0].body());
        } else {
            only_second(module.comb_processes()[0].body());
        }
    }
}
#[test]
fn native_child_timing_traversal_keeps_plain_always_as_a_suspending_process() {
    for body in [
        "wait(a) y=1;",
        "if(a) begin #1 y=1;end else y=2;",
        "assert(a) wait(a) y=1; else y=2;",
    ] {
        let tree = parsed(&format!("always @(posedge clk) begin {body} end"));
        let source = Source::from_syntax(&tree).unwrap();
        let module = &source.modules()[0];
        assert!(module.ff_processes().is_empty(), "{body}");
        assert!(module.comb_processes().is_empty(), "{body}");
        assert!(
            matches!(module.initial_processes()[0].body(),[Stmt::Loop{body,..}] if matches!(body.first(),Some(Stmt::WaitEvent(_)))),
            "{body}"
        );
    }
}
#[test]
fn native_root_membership_and_procedure_kinds_select_native_bodies() {
    for process in ["initial;", "always_comb y=a;"] {
        for remove in [true, false] {
            let mut tree = parsed(process);
            if remove {
                for item in &mut tree.declarations.body_items {
                    if matches!(item.kind, celox_sv_syntax::BodyItemKind::Procedure(_)) {
                        item.kind = celox_sv_syntax::BodyItemKind::Unconverted {
                            category: "missing process".into(),
                            items: Vec::new(),
                        };
                    }
                }
            } else {
                tree.declarations.procedures[0].kind = if process.starts_with("initial") {
                    ProcedureKind::AlwaysComb
                } else {
                    ProcedureKind::Initial
                };
            }
            let source = Source::from_syntax(&tree).unwrap();
            let module = &source.modules()[0];
            if !remove && process.starts_with("initial") {
                assert_eq!(module.comb_processes().len(), 1);
                assert!(module.comb_processes()[0].body().is_empty());
            } else {
                assert!(module.comb_processes().is_empty());
            }
            if !remove && !process.starts_with("initial") {
                assert_eq!(module.initial_processes().len(), 1);
                assert!(!module.initial_processes()[0].body().is_empty());
            } else {
                assert!(module.initial_processes().is_empty());
            }
        }
    }
}
#[test]
fn native_null_initial_roots_and_empty_wildcard_comb_keep_contracts() {
    let tree = parsed("initial;initial;always @*;");
    assert_eq!(tree.declarations.procedures.len(), 3);
    let source = Source::from_syntax(&tree).unwrap();
    let module = &source.modules()[0];
    assert!(module.comb_processes().is_empty());
    assert!(
        module
            .initial_processes()
            .iter()
            .all(|process| process.body().is_empty())
    );
}

#[test]
fn native_selected_process_graph_owns_nonblocking_and_iff_restrictions() {
    let mut tree = parsed("always_comb y=a;");
    for statement in &mut tree.declarations.statements {
        if let StatementKind::Assignment { nonblocking, .. } = &mut statement.kind {
            *nonblocking = true;
        }
    }
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("nonblocking assignment inside always_comb"),
        "{error}"
    );
    let mut tree = parsed("always_comb begin y<=a;y=2;end");
    tree.declarations.procedures[0].body = selected_body(&tree);
    let source = Source::from_syntax(&tree).unwrap();
    only_second(source.modules()[0].comb_processes()[0].body());
    let mut tree = parsed("always_ff @(posedge clk) y<=a;");
    for control in &mut tree.declarations.timing_controls {
        if let celox_sv_syntax::TimingControlKind::Event(celox_sv_syntax::EventControl::Items(
            items,
        )) = &mut control.kind
        {
            items[0].iff = Some(items[0].expression);
        }
    }
    let error = Source::from_syntax(&tree).unwrap_err();
    assert!(
        error.to_string().contains("iff-qualified always_ff event"),
        "{error}"
    );
}
#[test]
fn native_process_restrictions_apply_only_to_active_generate_items() {
    let tree = parsed(
        "if(0)begin:unused always_latch y=a;always_comb y<=a;always_ff @(posedge clk iff a) y<=a;end else begin:used initial y=2;end",
    );
    let source = Source::from_syntax(&tree).unwrap();
    only_second(source.modules()[0].initial_processes()[0].body());
}

#[test]
fn native_direct_nested_unit_restrictions_are_preserved_for_selected_parent() {
    for (process, expected) in [
        ("final begin end", "final construct"),
        (
            "always_latch begin int y;y<=1;end",
            "nonblocking assignment inside always_latch",
        ),
        (
            "always_comb begin int y;y<=1;end",
            "nonblocking assignment inside always_comb",
        ),
    ] {
        let tree = parse_source(
            &format!("module M;module N;{process} endmodule endmodule"),
            Path::new("native-nested-process-validation.sv"),
        )
        .unwrap();
        let error =
            Source::from_syntax_module_with_parameter_overrides(&tree, "M", &HashMap::default())
                .unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn unconverted_checker_processes_retain_existing_restriction_diagnostics() {
    for (process, expected) in [
        ("final begin end", "final construct"),
        (
            "always_latch begin int y;y<=1;end",
            "nonblocking assignment inside always_latch",
        ),
    ] {
        let tree = parse_source(
            &format!("module M;checker C;{process} endchecker endmodule"),
            Path::new("native-checker-process-validation.sv"),
        )
        .unwrap();
        let error = Source::from_syntax(&tree).unwrap_err();
        assert!(error.to_string().contains(expected), "{error}");
    }
}

#[test]
fn checker_and_nested_unit_validation_keep_the_legacy_root_and_active_leaf_contract() {
    for (body, rejected) in [
        ("checker C;final begin end endchecker", true),
        ("checker C;if(0)final begin end endchecker", true),
        (
            "if(0)begin:g checker C;final begin end endchecker end",
            false,
        ),
        (
            "if(1)begin:g checker C;final begin end endchecker end",
            true,
        ),
        (
            "module N;checker C;final begin end endchecker endmodule",
            true,
        ),
        (
            "module N;checker C;if(0)final begin end endchecker endmodule",
            false,
        ),
        (
            "module N;checker C;if(1)final begin end endchecker endmodule",
            false,
        ),
    ] {
        let tree = parse_source(
            &format!("module M;{body} endmodule"),
            Path::new("native-checker-scope-contract.sv"),
        )
        .unwrap();
        let result =
            Source::from_syntax_module_with_parameter_overrides(&tree, "M", &HashMap::default());
        if rejected {
            let error = result.unwrap_err();
            assert!(
                error.to_string().contains("final construct"),
                "{body}: {error}"
            );
        } else {
            assert!(result.is_ok(), "{body}: {result:?}");
        }
    }
}

#[test]
fn captured_checker_headers_preserve_compiler_directives_and_following_items() {
    let source = "module M;checker\n`line 10 \"checker-body.sv\" 0\nC(input logic value\n`line 20 \"checker-body.sv\" 0\n);initial;endchecker wire w=1'b0;endmodule";
    let tree = parse_source(source, Path::new("native-checker-header-directives.sv")).unwrap();
    assert_eq!(tree.declarations.directives.len(), 2);
    let source = Source::from_syntax(&tree).unwrap();
    assert_eq!(source.modules()[0].signals()[0].name(), "w");
}

#[test]
fn opaque_checker_process_payloads_retain_nonblocking_and_event_restrictions() {
    for (process, expected) in [
        (
            "always_comb randsequence(main) main:{x<=1;};endsequence",
            "nonblocking assignment inside always_comb",
        ),
        (
            "always_ff @(posedge clk) x<=@(posedge clk iff a)1;",
            "iff-qualified always_ff event",
        ),
        (
            "always_ff @(posedge clk) x<=repeat(2)@(posedge clk iff a)1;",
            "iff-qualified always_ff event",
        ),
        (
            "always @(posedge clk) randsequence(main) main:{@(posedge clk iff a)x=1;};endsequence",
            "iff-qualified always_ff event",
        ),
    ] {
        let source = format!("module M;checker C;logic x,clk,a;{process} endchecker endmodule");
        let tree = parse_source(&source, Path::new("native-checker-opaque-process.sv")).unwrap();
        let error = Source::from_syntax(&tree).unwrap_err();
        assert!(error.to_string().contains(expected), "{process}: {error}");
    }
    for process in [
        "always_comb randsequence(main) main:{x=1;};endsequence",
        "always_ff @(posedge clk) x<=@(posedge clk)1;",
    ] {
        let source = format!("module M;checker C;logic x,clk;{process} endchecker endmodule");
        let tree = parse_source(&source, Path::new("native-checker-opaque-valid.sv")).unwrap();
        assert!(Source::from_syntax(&tree).is_ok(), "{process}");
    }
}
