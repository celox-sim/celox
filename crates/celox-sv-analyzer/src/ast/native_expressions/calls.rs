use super::*;
pub(super) fn callee_name(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<String>> {
    Ok(direct::bound_callable_name(tree, id, native, dimensions)?
        .filter(|name| !name.starts_with('$') || name.contains("::")))
}

pub(in crate::ast) fn statement_root(
    root: ExprId,
    discarded: bool,
    native: &celox_sv_syntax::Declarations,
) -> Converted<ExprId> {
    Ok(if discarded {
        let ExprKind::Cast { target, expression } = native.expression(root).kind else {
            return Err(disagreement());
        };
        let ExprKind::Type(data_type) = native.expression(target).kind else {
            return Err(disagreement());
        };
        let data_type = native.data_type(data_type);
        if !matches!(&data_type.kind, celox_sv_syntax::DataTypeKind::Builtin(name) if name == "void")
            || data_type.signed.is_some()
            || !data_type.packed_dimensions.is_empty()
        {
            return Err(disagreement());
        }
        expression
    } else {
        root
    })
}

pub(in crate::ast) fn statement(
    root: ExprId,
    discarded: bool,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    signatures: &HashMap<String, Vec<String>>,
) -> Converted<Option<Expr>> {
    let root = statement_root(root, discarded, native)?;
    let callee = match native.expression(root).kind {
        ExprKind::Call { expression, .. } => expression,
        _ => root,
    };
    if callee_name(tree, callee, native, dimensions)?.is_none() {
        return match &native.expression(callee).kind {
            ExprKind::Name(name) if name.spelling.starts_with('$') => Ok(None),
            ExprKind::Member { separator, .. } if separator == "." => Ok(None),
            _ => Err(unsupported("subroutine callee")),
        };
    }
    let mut context = dimensions.clone();
    context.subroutine_param_names = std::sync::Arc::new(signatures.clone());
    if matches!(
        native.expression(root).kind,
        ExprKind::Name(_) | ExprKind::Member { .. }
    ) {
        let name = callee_name(tree, root, native, dimensions)?.ok_or_else(disagreement)?;
        return Ok(Some(Expr::Call {
            callee: Callee::Subroutine(name),
            args: Vec::new(),
            arg_order: Vec::new(),
        }));
    }
    super::bind_value(root, native, tree, &context, None).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_interface_statement_calls_keep_the_selected_element() {
        let tree = crate::migration::parse_source(
            "interface I; function int value(); return 7; endfunction endinterface
             module M; I lanes[2]();
             initial lanes[1].value();
             initial lanes[0].value;
             for (genvar i=0; i<2; i++) begin:g
                 initial void'(lanes[i].value());
             end endmodule",
            Path::new("indexed-interface-statement-calls.sv"),
        )
        .unwrap();
        let source = Source::from_syntax_module_with_parameter_expr_overrides(
            &tree,
            "M",
            &HashMap::default(),
            &crate::ModuleInterfaces::default(),
        )
        .unwrap();
        let module = source
            .modules()
            .iter()
            .find(|module| module.name() == "M")
            .unwrap();
        let calls = module
            .initial_processes()
            .iter()
            .flat_map(|process| process.body())
            .map(|statement| match statement {
                Stmt::Call { name, .. } => name.as_str(),
                other => panic!("expected interface function call: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 4);
        assert_ne!(calls[0], calls[1]);
        assert_eq!(calls[0], calls[3]);
        assert_eq!(calls[1], calls[2]);
    }
}
