//! Comparisons of type references (IEEE 1800-2023 6.23): constant
//! expressions that are true if and only if the referenced types match
//! (6.22.1).
use super::*;
use celox_sv_syntax::{ExprId, ExprKind};

/// The data type that `id` refers to when it is a type reference: `type(e)`,
/// `type(data_type)` or a data type used as an operand.
pub(in crate::ast) fn reference(
    id: ExprId,
    native: &Declarations,
) -> Option<celox_sv_syntax::TypeId> {
    match native.expression(ungroup(id, native)).kind {
        ExprKind::Type(ty) => Some(ty),
        _ => None,
    }
}

/// Whether type references `left` and `right` refer to matching types. A
/// type reference is compared with another type reference only.
pub(in crate::ast) fn matching(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    left: ExprId,
    right: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<bool> {
    let (Some(left), Some(right)) = (reference(left, native), reference(right, native)) else {
        return Err(unsupported(
            "comparison of a type reference with a value (IEEE 1800-2023 6.23)",
        ));
    };
    let mut semantic = |ty| -> Converted<semantic_types::TyId> {
        let bound = type_binding::bind(ty, tree, session, dimensions)?
            .ok_or_else(|| unsupported("type reference of this data type"))?;
        semantic_types::of_type(
            &mut session.types,
            &tree.functions,
            &bound,
            &dimensions.const_env,
        )
        .ok_or_else(|| unsupported("type reference of this data type"))
    };
    let (left, right) = (semantic(left)?, semantic(right)?);
    Ok(session.types.matches(left, right))
}

/// Whether `id` compares type references with an equality operator.
pub(in crate::ast) fn compares(id: ExprId, native: &Declarations) -> bool {
    matches!(
        &native.expression(id).kind,
        ExprKind::Binary { left, operator, right }
            if matches!(operator.as_str(), "==" | "!=" | "===" | "!==")
                && (reference(*left, native).is_some() || reference(*right, native).is_some())
    )
}

/// The constant value of comparison `id`, which [`compares`] type references.
pub(in crate::ast) fn comparison(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let ExprKind::Binary {
        left,
        operator,
        right,
    } = &native.expression(id).kind
    else {
        unreachable!("a comparison of type references")
    };
    let matching = matching(tree, session, *left, *right, native, dimensions)?;
    let equal = matches!(operator.as_str(), "==" | "===");
    Ok(Expr::Literal(
        if matching == equal { "1'b1" } else { "1'b0" }.into(),
    ))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// A type operator in a generate block cannot read the block's own
    /// variables; it is rejected with that reason.
    #[test]
    fn type_operators_on_generate_block_variables_are_rejected() {
        let error = crate::analyze_source(
            "module Top; if (1) begin : g int a; var type(a) b; end endmodule",
            Path::new("generate-type-operator.sv"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("any expression in a generate block"),
            "{error}"
        );
    }

    /// A variable declared with a type reference needs `var` (IEEE
    /// 1800-2023 6.23).
    #[test]
    fn type_reference_variables_need_var() {
        let error = crate::analyze_source(
            "module Top; real a; type(a) b; endmodule",
            Path::new("type-reference-var.sv"),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("without `var`"), "{error}");
    }
}
