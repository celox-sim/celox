//! Array locator methods (IEEE 1800-2023 7.12.1) and iterator index queries
//! (7.12.4) bind with their iterator as a hidden local of the body.
use super::*;
use crate::ast::{Expr, Source, Stmt};
use crate::system_functions::{
    Callee, ContainerKind, LocatorKey, LocatorMethod, LocatorSource, ObjectOp,
};

fn source(declarations: &str, body: &str) -> Result<Source, AnalyzerError> {
    let tree = parse_source(
        &format!("module M;{declarations} initial begin {body} end endmodule"),
        Path::new("array-locators.sv"),
    )
    .unwrap();
    Source::from_syntax(&tree)
}

/// The locator calls in `expr`, outermost first, with their arguments.
fn locators<'e>(
    expr: &'e Expr,
    found: &mut Vec<(&'e crate::system_functions::Locator, &'e [Option<Expr>])>,
) {
    match expr {
        Expr::Call { callee, args, .. } => {
            if let Callee::Object(op) = callee
                && let ObjectOp::Locator(locator) = &**op
            {
                found.push((locator, args));
            }
            for arg in args.iter().flatten() {
                locators(arg, found);
            }
        }
        Expr::Binary { left, right, .. } => {
            locators(left, found);
            locators(right, found);
        }
        Expr::Unary { expr, .. } | Expr::Resize { expr, .. } | Expr::Select { expr, .. } => {
            locators(expr, found)
        }
        _ => {}
    }
}

fn assigned(source: &Source) -> Vec<&Expr> {
    source.modules()[0].initial_processes()[0]
        .body()
        .iter()
        .filter_map(|stmt| match stmt {
            Stmt::Assign { rhs, .. } => Some(rhs),
            _ => None,
        })
        .collect()
}

fn ident(expr: &Option<Expr>) -> &str {
    let Some(Expr::Ident(name)) = expr else {
        panic!("{expr:?}")
    };
    name
}

#[test]
fn locator_iterators_are_hidden_locals_read_by_the_predicate() {
    let source = source(
        "int q[$], item;",
        "item = q.find with (item > 1).size() + item;",
    )
    .unwrap();
    let rhs = assigned(&source)[0];
    let mut found = Vec::new();
    locators(rhs, &mut found);
    let [(locator, args)] = found.as_slice() else {
        panic!("{rhs:?}")
    };
    assert_eq!(locator.method, LocatorMethod::Find);
    assert!(matches!(
        locator.source,
        LocatorSource::Container(layout) if layout.kind == ContainerKind::Queue { capacity: 0 }
    ));
    let (iterator, index) = (ident(&args[2]), ident(&args[3]));
    assert!(
        iterator.starts_with("item@") && index.starts_with("index@"),
        "{iterator} {index}"
    );
    // The iterator hides the module variable `item` inside the predicate only.
    let predicate = format!("{:?}", args[1]);
    assert!(predicate.contains(&format!("{iterator:?}")) && !predicate.contains("\"item\""));
    assert!(format!("{rhs:?}").contains("Ident(\"item\")"));
    let locals = source.modules()[0].locals();
    assert!(locals.iter().any(|local| local.name == iterator));
    assert!(locals.iter().any(|local| local.name == index));
}

#[test]
fn nested_locators_declare_distinct_iterators() {
    let source = source(
        "int q[$], r[$];",
        "r = q.find(item) with (item > q.find with (item > 5).size());",
    )
    .unwrap();
    let mut found = Vec::new();
    locators(assigned(&source)[0], &mut found);
    let [(_, outer), (_, inner)] = found.as_slice() else {
        panic!("{found:?}")
    };
    assert_ne!(ident(&outer[2]), ident(&inner[2]));
    // The inner predicate reads the inner iterator, not the outer one.
    assert!(format!("{:?}", inner[1]).contains(ident(&inner[2])));
    assert!(!format!("{:?}", inner[1]).contains(ident(&outer[2])));
}

#[test]
fn index_queries_read_the_index_local() {
    let source = source(
        "int map[string]; string keys[$];",
        "keys = map.find_index(v, at) with (v.at != \"x\" && v == 1);",
    )
    .unwrap();
    let mut found = Vec::new();
    locators(assigned(&source)[0], &mut found);
    let [(locator, args)] = found.as_slice() else {
        panic!("{found:?}")
    };
    // find_index of a string-indexed array returns strings (7.12.1).
    assert_eq!(
        locator.result.handle,
        Some(crate::system_functions::HandleProperty::String)
    );
    let index = ident(&args[3]);
    assert!(index.starts_with("at@"), "{index}");
    assert!(format!("{:?}", args[1]).contains(index));
    let local = source.modules()[0]
        .locals()
        .iter()
        .find(|local| local.name == index)
        .unwrap();
    assert_eq!(local.r#type.kind(), crate::ir::TypeKind::String);
}

#[test]
fn compared_values_take_the_type_of_the_with_expression() {
    let source = source(
        "int q[$], r[$]; string s[$], t[$];",
        "r = q.min with (item[3:0]); t = s.unique; r = q.max;",
    )
    .unwrap();
    let keys = assigned(&source)
        .into_iter()
        .map(|rhs| {
            let mut found = Vec::new();
            locators(rhs, &mut found);
            found[0].0.key
        })
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            Some(LocatorKey::Integral {
                width: 4,
                signed: false
            }),
            Some(LocatorKey::String),
            Some(LocatorKey::Integral {
                width: 32,
                signed: true
            }),
        ]
    );
}

#[test]
fn fixed_arrays_iterate_their_declared_range() {
    let source = source(
        "byte b[2:5]; int r[$];",
        "r = b.find_index with (item < 0);",
    )
    .unwrap();
    let mut found = Vec::new();
    locators(assigned(&source)[0], &mut found);
    assert_eq!(
        found[0].0.source,
        LocatorSource::Fixed { left: 2, right: 5 }
    );
}

#[test]
fn locator_methods_require_their_with_clause() {
    for body in ["r = q.find;", "r = q.find_index();", "r = q.min(x);"] {
        assert!(
            matches!(
                source("int q[$], r[$];", body),
                Err(AnalyzerError::Parse(_))
            ),
            "{body}"
        );
    }
}

#[test]
fn class_methods_named_like_locators_stay_method_calls() {
    let source = source(
        "class C; function int find(int x); return x; endfunction endclass C c; int r;",
        "c = new; r = c.find(2);",
    )
    .unwrap();
    let mut found = Vec::new();
    for rhs in assigned(&source) {
        locators(rhs, &mut found);
    }
    assert!(found.is_empty());
}

#[test]
fn structure_elements_are_read_through_their_records() {
    let source = source(
        "typedef struct { string name; int id; } entry_t; entry_t q[$], r[$];",
        "r = q.find(e) with (e.id > 1 && e.name != \"x\");",
    )
    .unwrap();
    let mut found = Vec::new();
    locators(assigned(&source)[0], &mut found);
    let [(_, args)] = found.as_slice() else {
        panic!("{found:?}")
    };
    let iterator = ident(&args[2]);
    // The iterator holds the record handle, whose fields the predicate loads.
    let predicate = format!("{:?}", args[1]);
    assert!(predicate.contains("Load"), "{predicate}");
    assert!(
        predicate.contains(&format!("Ident({iterator:?})")),
        "{predicate}"
    );
    let local = source.modules()[0]
        .locals()
        .iter()
        .find(|local| local.name == iterator)
        .unwrap();
    assert_eq!(local.r#type.kind(), crate::ir::TypeKind::Object);
}
