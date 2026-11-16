use std::path::Path;

use celox_sv_syntax::{DataTypeKind, Declarations, ExprKind};

use crate::source_tree::parse_source;

fn expand(source: &str) -> Result<Declarations, String> {
    parse_source(source, Path::new("lets.sv"))
        .map(|tree| tree.into_declarations())
        .map_err(|error| error.to_string())
}

fn expanded(source: &str) -> Declarations {
    expand(source).unwrap_or_else(|error| panic!("{source}: {error}"))
}

/// The calls of `f` and the names `f` left in the expressions of `tree`.
fn calls_of_f(tree: &Declarations) -> usize {
    let callees = tree
        .expression_nodes()
        .filter_map(|(_, node)| match &node.kind {
            ExprKind::Call { expression, .. } => Some(*expression),
            _ => None,
        })
        .collect::<Vec<_>>();
    callees
        .into_iter()
        .filter(|callee| matches!(&tree.expression(*callee).kind, ExprKind::Name(name) if name.value() == "f"))
        .count()
}

/// A `let` of a module is expanded where it is called, with positional,
/// named, empty and default actuals; one called before its declaration, one
/// that calls itself, a call with an unknown or repeated formal or too many
/// actuals, and a `let` outside the items of a module, interface or program
/// are rejected (IEEE 1800-2023 11.12, 13.5.4).
#[test]
fn let_calls_expand_and_bind_their_actuals() {
    for source in [
        "module M; logic a; let f(x, y = 1) = x + y; initial a = f(.x(a)); endmodule",
        "module M; logic a; let f(x, y = 1) = x + y; initial a = f(a, ); endmodule",
        "module M; logic a; let f(x = 0) = x; initial a = f(.x()); endmodule",
        "module M; logic a; let f = a; initial a = f; endmodule",
        "module M; (* keep *) let f(x) = x; initial $display(f(1)); endmodule",
    ] {
        assert_eq!(calls_of_f(&expanded(source)), 0, "{source}");
    }
    for (source, message) in [
        (
            "module M; initial $display(f(1)); let f(x) = x; endmodule",
            "used before its declaration",
        ),
        (
            "module M; let f(x) = f(x) + 1; initial $display(f(1)); endmodule",
            "calls itself",
        ),
        (
            "module M; let f(x) = x; initial $display(f(.y(1))); endmodule",
            "no formal named `y`",
        ),
        (
            "module M; let f(x) = x; initial $display(f(1, 2)); endmodule",
            "2 actuals for 1 formals",
        ),
        (
            "module M; let f(x = 1) = x; initial $display(f(.x(), .x(2))); endmodule",
            "formal `x` is bound twice",
        ),
        (
            "module M; let f(x) = x; initial $display(f()); endmodule",
            "no actual for formal `x`",
        ),
        (
            "package P; let f(x) = x; endpackage",
            "declared other than among the items",
        ),
        (
            "module M; if (1) begin : g let f(x) = x; end endmodule",
            "declared other than among the items",
        ),
    ] {
        let error = expand(source)
            .err()
            .unwrap_or_else(|| panic!("accepted {source}"));
        assert!(error.contains(message), "{source}: {error}");
    }
}

/// The body's names resolve where the `let` is declared: a call in a scope
/// that declares one of them again, a block or the blocks of a generate
/// loop whose index has that name, is rejected, while a declaration of
/// another scope, such as another block or a nested module, hides none. A
/// name that a scope around the call declares again is no call of the
/// `let`; a nested module's own function of that name is its own.
#[test]
fn let_bodies_resolve_their_names_where_they_are_declared() {
    for source in [
        "module M; logic a; let f = a; initial begin int a; end initial $display(f); endmodule",
        "module M; logic a; let f = a; module N; logic a; endmodule initial $display(f); endmodule",
        "module M; localparam int i = 5; let f = i; for (genvar i = 0; i < 2; i++) begin : g end \
         initial $display(f); endmodule",
    ] {
        assert_eq!(calls_of_f(&expanded(source)), 0, "{source}");
    }
    for source in [
        "module M; logic a; let f = a; initial begin int a; $display(f); end endmodule",
        "module M; localparam int i = 5; let f = i; for (genvar i = 0; i < 2; i++) begin : g \
         initial $display(f); end endmodule",
        "module M; logic a; let f(x) = x + a; function int g(int a); return f(1); endfunction endmodule",
    ] {
        let error = expand(source)
            .err()
            .unwrap_or_else(|| panic!("accepted {source}"));
        assert!(error.contains("declares again"), "{source}: {error}");
    }
    let tree = expanded(
        "module M; let f(x) = x + 1; initial begin int f; f = 2; end \
         module N; function int f(int x); return x; endfunction initial $display(f(1)); endmodule \
         endmodule",
    );
    assert_eq!(calls_of_f(&tree), 1);
}

/// A formal used in a type operator is replaced there too: the expansion's
/// `type(...)` operand is the actual (11.12).
#[test]
fn let_formals_are_replaced_inside_type_operators() {
    let tree = expanded(
        "module M; logic [7:0] a; let same(x) = type(x) == type(logic [7:0]); \
         initial $display(same(a)); endmodule",
    );
    let operands = tree
        .types
        .iter()
        .filter_map(|data_type| match data_type.kind {
            DataTypeKind::TypeOf(operand) => Some(&tree.expression(operand).kind),
            _ => None,
        })
        .collect::<Vec<_>>();
    // The declaration's own operand, and the call's: the parenthesized `a`.
    assert!(
        operands
            .iter()
            .any(|kind| matches!(kind, ExprKind::Name(name) if name.value() == "x"))
    );
    assert!(operands.iter().any(|kind| matches!(kind,
        ExprKind::Group(inner) if matches!(&tree.expression(*inner).kind, ExprKind::Name(name) if name.value() == "a"))));
}

/// A deep body expands without the thread's stack.
#[test]
fn deep_let_bodies_expand_iteratively() {
    let depth = 50_000;
    let source = format!(
        "module M; logic a; let f(x) = {}x{}; initial $display(f(a)); endmodule",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    let expanded = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || parse_source(&source, Path::new("deep_let.sv")).is_ok())
        .unwrap()
        .join()
        .unwrap();
    assert!(expanded);
}
