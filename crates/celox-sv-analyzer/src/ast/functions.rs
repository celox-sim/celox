//! Function declaration validation and function-body expression construction.

use super::*;

mod symbolic;

pub(super) fn functions_from_scope(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    dimensions: &PackedDimensions,
) -> Converted<HashMap<String, Function>> {
    let aliases = type_aliases_from_scope_with_env(tree, env)?;
    let items = generate::items(tree, env, &aliases)?;
    native_functions(
        &items,
        tree,
        dimensions,
        lifetimes::automatic_by_default(tree),
    )
}

pub(super) fn native_functions(
    items: &[generate::Item],
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    automatic: bool,
) -> Converted<HashMap<String, Function>> {
    let mut owners = Vec::new();
    let mut names = HashMap::default();
    let mut directions = HashMap::default();
    for item in items {
        for id in subroutine_owners::item(item.id, tree)? {
            let native = tree.declarations().subroutine(id);
            let mut arguments = Vec::new();
            let mut modes = Vec::new();
            let mut previous = crate::procedural::ParamDirection::Input;
            for port in native
                .ports
                .iter()
                .flatten()
                .chain(native.port_declarations.iter().flat_map(|decl| &decl.ports))
            {
                if let Some(direction) = port.direction {
                    previous = match direction {
                        celox_sv_syntax::Direction::Input => {
                            crate::procedural::ParamDirection::Input
                        }
                        celox_sv_syntax::Direction::Output => {
                            crate::procedural::ParamDirection::Output
                        }
                        celox_sv_syntax::Direction::Inout => {
                            crate::procedural::ParamDirection::Inout
                        }
                        _ => continue,
                    };
                }
                if let Some(name) = &port.name {
                    arguments.push(instances::normalize_identifier(&name.spelling));
                    modes.push(previous);
                }
            }
            let name = item.name(&instances::normalize_identifier(&native.name.spelling));
            names.insert(name.clone(), arguments);
            directions.insert(name, modes);
            owners.push((item, id));
        }
    }
    let names = Arc::new(names);
    let mut functions = HashMap::default();
    for (item, id) in owners {
        let aliases = item.type_aliases(&dimensions.type_aliases);
        let _scope = item.interface_scope();
        let _constants = item.enter();
        let mut signature_context = item.dimensions(dimensions);
        signature_context.const_env = item.env.clone();
        signature_context.type_aliases = aliases.as_ref().clone();
        let mut function =
            native_signature(tree.declarations().subroutine(id), tree, &signature_context)?;
        let mut locals = Vec::new();
        let mut counter = 0;
        let mut state = procedural::BodyState {
            locals: &mut locals,
            counter: &mut counter,
            subroutine_params: &names,
            subroutine_directions: &directions,
            automatic,
            statics: Vec::new(),
            static_initializers: Vec::new(),
            constant_evaluation: false,
        };
        let mut dims = item.dimensions(dimensions);
        dims.subroutine_param_names = names.clone();
        match procedural::lower_subroutine(
            id,
            tree,
            &item.env,
            &aliases,
            &dims,
            &item.literals,
            &mut state,
            item,
            None,
            None,
        ) {
            Ok(subroutine) => {
                let has_real = subroutine.return_type.as_ref().is_some_and(|ty| {
                    matches!(
                        ty.kind(),
                        crate::ir::TypeKind::Real | crate::ir::TypeKind::Shortreal
                    )
                }) || subroutine.params.iter().any(|param| {
                    matches!(
                        param.r#type.kind(),
                        crate::ir::TypeKind::Real | crate::ir::TypeKind::Shortreal
                    )
                });
                let body = if has_real {
                    None
                } else {
                    symbolic::body(&subroutine, &locals)
                };
                if let Some((value, outputs)) = body {
                    function.body = Some(value);
                    function.outputs = outputs;
                }
                function.name = subroutine.name;
            }
            Err(error @ AnalyzerError::Parse(_)) => return Err(error),
            // Signature-only functions are the existing representation for
            // bodies that cannot be inlined; execution still validates/lowers
            // their native subroutine body through the ordinary path.
            Err(_) => function.name = item.name(&function.name),
        }
        let name = function.name.clone();
        if functions.insert(name.clone(), function).is_some() {
            return Err(unsupported(format!(
                "duplicate function declaration `{name}`"
            )));
        }
    }
    Ok(functions)
}

/// The statements of a function body.
fn coerce_function_local_assignment(expr: Expr, r#type: FunctionLocalType) -> Expr {
    let expr = Expr::Resize {
        expr: Box::new(expr),
        width: r#type.width,
        signed: r#type.signed,
    };
    if r#type.is_2state {
        Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(expr),
        }
    } else {
        expr
    }
}

pub(super) fn procedural_truth_condition(condition: Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::RedOr,
        expr: Box::new(Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(condition),
        }),
    }
}

pub(super) fn case_item_condition(case_expr: Expr, item_expr: Expr, wildcard: bool) -> Expr {
    Expr::Binary {
        left: Box::new(case_expr),
        op: if wildcard {
            BinaryOp::EqWildcard
        } else {
            BinaryOp::EqCase
        },
        right: Box::new(item_expr),
    }
}

fn native_signature(
    native: &celox_sv_syntax::Subroutine,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Function> {
    let mut params = Vec::new();
    let mut previous: Option<Type> = None;
    let mut direction = ParamDirection::Input;
    for port in native.ports.iter().flatten().chain(
        native
            .port_declarations
            .iter()
            .flat_map(|group| &group.ports),
    ) {
        if let Some(value) = port.direction {
            direction = match value {
                celox_sv_syntax::Direction::Input => ParamDirection::Input,
                celox_sv_syntax::Direction::Output => ParamDirection::Output,
                celox_sv_syntax::Direction::Inout => ParamDirection::Inout,
                _ => direction,
            };
        }
        let Some(name) = &port.name else { continue };
        let data = tree.declarations().data_type(port.data_type);
        let omitted = matches!(data.kind, celox_sv_syntax::DataTypeKind::Implicit)
            && data.signed.is_none()
            && data.packed_dimensions.is_empty();
        let ty = if port.direction.is_none() && omitted {
            previous
                .clone()
                .or(type_binding::bind(port.data_type, tree, context)?)
        } else {
            type_binding::bind(port.data_type, tree, context)?
        };
        previous = ty.clone();
        let expr_type = ty
            .as_ref()
            .and_then(|ty| expr_type_from_type(&tree.state, ty, &context.const_env));
        params.push(FunctionParam {
            name: instances::normalize_identifier(&name.spelling),
            has_default: port.default.is_some(),
            direction,
            width: expr_type.map(|ty| ty.width),
            signed: expr_type.is_some_and(|ty| ty.signed),
            is_2state: ty.as_ref().is_some_and(|ty| ty.kind() == TypeKind::Bit),
            packed_dimensions: ty
                .as_ref()
                .map(|ty| dimensions::function_packed_dimension_widths(ty.packed_ranges()))
                .unwrap_or_default(),
            signed_element_depth: ty.as_ref().and_then(|ty| ty.signed_element_depth),
        });
    }
    let ty = match native.return_type {
        Some(id) => type_binding::bind(id, tree, context)?,
        None => None,
    };
    let result = ty
        .as_ref()
        .and_then(|ty| expr_type_from_type(&tree.state, ty, &context.const_env));
    let first = match ty.as_ref().and_then(|ty| ty.packed_ranges().first()) {
        Some(range) => (|| {
            let left = eval_ast_const_expr(&tree.state, &range.left, &context.const_env)?;
            let right = eval_ast_const_expr(&tree.state, &range.right, &context.const_env)?;
            usize::try_from(left.abs_diff(right).checked_add(1)?).ok()
        })(),
        None => result.map(|ty| ty.width),
    };
    Ok(Function {
        return_real: ty
            .as_ref()
            .is_some_and(|ty| matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal)),
        return_string: ty.as_ref().is_some_and(|ty| ty.kind() == TypeKind::String),
        name: instances::normalize_identifier(&native.name.spelling),
        params,
        body: None,
        outputs: Vec::new(),
        return_width: result.map(|ty| ty.width),
        return_first_packed_dimension_width: first,
        return_dimensions: ty.as_ref().map(|ty| {
            ty.unpacked_ranges().len()
                + if matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal) {
                    0
                } else {
                    ty.packed_ranges().len()
                }
        }),
        return_signed: result.is_some_and(|ty| ty.signed),
        return_is_2state: ty.as_ref().is_some_and(|ty| ty.kind() == TypeKind::Bit),
    })
}
