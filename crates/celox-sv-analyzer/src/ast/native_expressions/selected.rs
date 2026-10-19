//! Native index binding regressions.
use super::*;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_index_operators_and_selected_names_are_authoritative() {
        let tree = crate::migration::parse_source(
            "module M;assign y=a[i+1];endmodule",
            Path::new("native-index.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let mut native = tree.declarations().clone();
        for expression in &mut native.expressions {
            if let ExprKind::Binary { operator, .. } = &mut expression.kind {
                *operator = "-".into();
            }
        }
        let root = native.assign_statements[0].assignments[0].right;
        let Expr::Select { msb, lsb, .. } =
            value(root, &native, &tree, &PackedDimensions::default()).unwrap()
        else {
            panic!()
        };
        assert_eq!(msb, lsb);
        assert_eq!(
            lsb,
            ConstExpr::Binary {
                left: Box::new(ConstExpr::Ident("i".into())),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".into()))
            }
        );
        for expression in &mut native.expressions {
            if let ExprKind::Name(name) = &mut expression.kind
                && name.spelling == "a"
            {
                name.spelling = "other".into();
            }
        }
        let Expr::Select { expr, .. } =
            value(root, &native, &tree, &PackedDimensions::default()).unwrap()
        else {
            panic!()
        };
        assert!(matches!(*expr,Expr::Ident(ref name) if name=="other"));
    }
    #[test]
    fn nested_native_indices_keep_typed_selection_values() {
        let tree = crate::migration::parse_source(
            "module M;assign y=a[b[i+1]];endmodule",
            Path::new("nested-index.sv"),
        )
        .unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let native = tree.declarations();
        let root = native.assign_statements[0].assignments[0].right;
        let Expr::Select { lsb, .. } =
            value(root, native, &tree, &PackedDimensions::default()).unwrap()
        else {
            panic!()
        };
        // A single-bit nested select remains self-determined before it is
        // widened into the outer selection's offset arithmetic.
        let ConstExpr::Binary {
            left,
            op: BinaryOp::BitAnd,
            right,
        } = lsb
        else {
            panic!("{lsb:?}")
        };
        assert_eq!(*right, ConstExpr::Literal("1".into()));
        let ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } = *left
        else {
            panic!("{left:?}")
        };
        assert_eq!(*expr, ConstExpr::Ident("b".into()));
        assert_eq!(msb, lsb);
        assert!(!signed);
        assert!(matches!(
            *lsb,
            ConstExpr::Binary {
                op: BinaryOp::Add,
                ..
            }
        ));
    }
    #[test]
    fn deep_native_index_groups_use_the_explicit_worklists() {
        let path = Path::new("native-index-groups.sv");
        let tree =
            crate::migration::parse_source("module M;assign y=a[7];endmodule", path).unwrap();
        let tree = AnalysisContext::unspecialized(&tree);
        let source = format!(
            "module M;assign y=a[{}7{}];endmodule",
            "(".repeat(10_000),
            ")".repeat(10_000)
        );
        let native = celox_sv_syntax::Declarations::parse(&source, path).unwrap();
        let root = native.assign_statements[0].assignments[0].right;
        let Expr::Select { lsb, .. } =
            value(root, &native, &tree, &PackedDimensions::default()).unwrap()
        else {
            panic!()
        };
        assert_eq!(lsb, ConstExpr::Literal("7".into()));
    }
}

#[cfg(test)]
mod range_tests {
    use super::*;
    #[test]
    fn deep_native_constant_and_indexed_range_groups_use_worklists() {
        for (shallow, deep, high, low) in [
            (
                "a[7:0]".to_string(),
                format!(
                    "a[{}7{}:{}0{}]",
                    "(".repeat(10_000),
                    ")".repeat(10_000),
                    "(".repeat(10_000),
                    ")".repeat(10_000)
                ),
                7,
                0,
            ),
            (
                "a[1+:2]".to_string(),
                format!(
                    "a[{}1{}+:{}2{}]",
                    "(".repeat(10_000),
                    ")".repeat(10_000),
                    "(".repeat(10_000),
                    ")".repeat(10_000)
                ),
                2,
                1,
            ),
        ] {
            let path = Path::new("native-range-groups.sv");
            let tree = crate::migration::parse_source(
                &format!("module M;assign y={shallow};endmodule"),
                path,
            )
            .unwrap();
            let tree = AnalysisContext::unspecialized(&tree);
            let native = celox_sv_syntax::Declarations::parse(
                &format!("module M;assign y={deep};endmodule"),
                path,
            )
            .unwrap();
            let root = native.assign_statements[0].assignments[0].right;
            let Expr::Select { msb, lsb, .. } =
                value(root, &native, &tree, &PackedDimensions::default()).unwrap()
            else {
                panic!()
            };
            assert_eq!(
                eval_ast_const_expr(&tree.state, &msb, &HashMap::default()),
                Some(high)
            );
            assert_eq!(
                eval_ast_const_expr(&tree.state, &lsb, &HashMap::default()),
                Some(low)
            );
        }
    }
}
