//! Compilation-unit data and visibility consume native source syntax.
use super::*;
use crate::ast::Source;
use celox_sv_syntax::{DataDeclaration, DataTypeKind, ExprKind, SourceItemKind};
fn subtract(tree: &mut SourceTree) {
    for value in &mut tree.declarations.expressions {
        if let ExprKind::Binary { operator, .. } = &mut value.kind
            && operator == "+"
        {
            *operator = "-".into();
        }
    }
}
fn has_subtraction(value: &crate::ir::Expr) -> bool {
    match value {
        crate::ir::Expr::Binary {
            op: crate::ir::BinaryOp::Sub,
            ..
        } => true,
        crate::ir::Expr::Resize { expr, .. } | crate::ir::Expr::Unary { expr, .. } => {
            has_subtraction(expr)
        }
        _ => false,
    }
}
#[test]
fn native_global_data_types_bounds_and_initializers_reach_the_unit_state_module() {
    let path = Path::new("native-global-data.sv");
    let mut tree = parse_source(
        "logic[(10+2):0]g=8+2;module M(output logic[(10+2):0]y);assign y=g;endmodule",
        path,
    )
    .unwrap();
    subtract(&mut tree);
    let DataDeclaration::Variables(global) = &tree.declarations.data_declarations[0] else {
        panic!()
    };
    let tokens = tree.declarations.data_type(global.data_type).tokens;
    tree.declarations
        .types
        .iter_mut()
        .find(|value| value.tokens == tokens)
        .unwrap()
        .kind = DataTypeKind::Builtin("bit".into());
    let packages = crate::Packages::analyze(&[(&tree, path)]).unwrap();
    let state = packages
        .state_modules()
        .find(|module| module.name() == "$unit")
        .unwrap();
    let global = state
        .signals()
        .iter()
        .find(|signal| signal.name() == "g")
        .unwrap();
    assert_eq!(global.r#type().kind(), crate::ir::TypeKind::Bit);
    assert_eq!(global.r#type().resolved_width(), Some(9));
    assert!(state.initial_processes().iter().flat_map(|process|process.body()).any(|statement|matches!(statement,crate::ir::Stmt::Assign{rhs,..} if has_subtraction(rhs))));
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(
        ir.modules()[0].ports()[0].r#type().resolved_width(),
        Some(9)
    );
}
#[test]
fn native_source_positions_determine_compilation_unit_visibility() {
    let mut tree = parse_source(
        "parameter int U=2;module M;parameter int N=$unit::U;endmodule parameter int Later=0;",
        Path::new("native-unit-visibility.sv"),
    )
    .unwrap();
    Source::from_syntax(&tree).unwrap();
    // Move only the native declaration position past the module. The original
    // legacy declaration offset remains earlier and must not determine lookup.
    let later = tree.declarations.source_items.last().unwrap().tokens;
    tree.declarations.source_items[0].tokens = later;
    assert!(
        matches!(Source::from_syntax(&tree),Err(AnalyzerError::UnknownPackageItem{package,name}) if package=="$unit" && name=="U")
    );
}
#[test]
fn compilation_unit_source_membership_controls_objects_and_rejects_duplicates() {
    for duplicate in [false, true] {
        let mut tree = parse_source(
            "int g=1;module M(output int y);assign y=g;endmodule",
            Path::new("native-unit-membership.sv"),
        )
        .unwrap();
        if duplicate {
            let item = tree.declarations.source_items[0].clone();
            tree.declarations.source_items.push(item);
        } else {
            tree.declarations.source_items.remove(0);
        }
        if duplicate {
            assert!(matches!(
                Source::from_syntax(&tree),
                Err(AnalyzerError::Parse(_))
            ));
        } else {
            let packages =
                crate::Packages::analyze(&[(&tree, Path::new("native-unit-membership.sv"))])
                    .unwrap();
            assert!(
                packages
                    .state_modules()
                    .all(|module| module.signals().iter().all(|signal| signal.name() != "g"))
            );
            Source::from_syntax(&tree).unwrap();
        }
    }
}
#[test]
fn native_global_functions_remain_visible_after_the_using_module() {
    let mut tree=parse_source("module M;parameter int N=f(8+2);endmodule function automatic int f(input int x);return x;endfunction",Path::new("native-unit-forward-function.sv")).unwrap();
    subtract(&mut tree);
    assert!(matches!(
        tree.declarations.source_items[0].kind,
        SourceItemKind::Unit(_)
    ));
    assert!(matches!(
        tree.declarations.source_items[1].kind,
        SourceItemKind::Declaration(_)
    ));
    let ir = crate::analyze::analyze_source(Source::from_syntax(&tree).unwrap()).unwrap();
    assert_eq!(ir.modules()[0].parameters()[0].resolved_value(), Some(6));
}
