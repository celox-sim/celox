//! Actual arguments of `ref` and `const ref` formals (IEEE 1800-2023
//! 13.5.2): only a variable, a class property, a member of an unpacked
//! structure, or an element of an unpacked array, of a type equivalent to the
//! formal's (6.22.2).
use super::*;

/// Check actual `actual` of formal `position` of subroutine `name` when that
/// formal is passed by reference.
pub(super) fn check_actual(
    name: &str,
    position: usize,
    actual: ExprId,
    native: &Declarations,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    if !dimensions
        .subroutine_directions
        .get(name)
        .and_then(|directions| directions.get(position))
        .is_some_and(|direction| direction.is_reference())
    {
        return Ok(());
    }
    let invalid = |detail: &str| AnalyzerError::InvalidRefArgument {
        subroutine: name.to_owned(),
        argument: dimensions
            .subroutine_param_names
            .get(name)
            .and_then(|names| names.get(position))
            .cloned()
            .unwrap_or_else(|| (position + 1).to_string()),
        detail: detail.to_owned(),
    };
    let actual = ungroup(actual, native);
    let shape = match &native.expression(actual).kind {
        ExprKind::Name(variable) => {
            let variable = instances::normalize_identifier(&variable.spelling);
            match dimensions.get(&variable) {
                Some(shape)
                    if !dimensions.const_env.contains_key(&variable)
                        && !dimensions.parameter_values.contains_key(&variable) =>
                {
                    Some(shape.clone())
                }
                Some(_) => {
                    return Err(invalid(
                        "a constant is not a variable (IEEE 1800-2023 13.5.2)",
                    ));
                }
                None => property_shape(tree, actual, native, dimensions)?,
            }
        }
        ExprKind::Member { .. } => match property_shape(tree, actual, native, dimensions)? {
            Some(shape) => Some(shape),
            // A member of a variable of unpacked structure type is a
            // variable of its own (see `unpacked_structs`).
            None if let (parts, None) = direct::path(actual, native, tree, dimensions)?
                && let [variable] = parts.as_slice()
                && let Some(shape) = dimensions.get(variable)
                && !dimensions.const_env.contains_key(variable) =>
            {
                Some(shape.clone())
            }
            // A static property is a variable of its class's scope.
            None if let Some(variable) =
                objects::static_property(tree, actual, native, dimensions)? =>
            {
                match dimensions.get(&variable) {
                    Some(shape) => Some(shape.clone()),
                    None => return Ok(()),
                }
            }
            None => {
                return Err(unsupported(format!(
                    "ref argument of `{name}` bound to a structure member or hierarchical name"
                )));
            }
        },
        ExprKind::Select {
            expression,
            dimension,
        } if matches!(dimension.kind, DimensionKind::Index(_)) => {
            let mut indices = 1;
            let mut base = ungroup(*expression, native);
            while let ExprKind::Select {
                expression,
                dimension,
            } = &native.expression(base).kind
                && matches!(dimension.kind, DimensionKind::Index(_))
            {
                indices += 1;
                base = ungroup(*expression, native);
            }
            let ExprKind::Name(variable) = &native.expression(base).kind else {
                return Err(unsupported(format!(
                    "ref argument of `{name}` bound to a member or element of an expression"
                )));
            };
            let variable = instances::normalize_identifier(&variable.spelling);
            let Some(array) = dimensions.get(&variable) else {
                return Err(unsupported(format!(
                    "ref argument of `{name}` bound to an element of a class property"
                )));
            };
            if array.container.is_some() {
                if indices == 1 {
                    // Elements of dynamically sized arrays: their reference
                    // must outlive a resize or a deletion of the element.
                    return Err(unsupported(format!(
                        "ref argument of `{name}` bound to an element of a dynamic array, queue, or associative array"
                    )));
                }
                return Err(invalid("a select of a container element is not a variable"));
            }
            if indices > array.unpacked.len() {
                return Err(invalid(
                    "a select of packed bits is not a variable (IEEE 1800-2023 13.5.2)",
                ));
            }
            array_compatibility::selected_unpacked_shape(&variable, indices, dimensions).or_else(
                || {
                    (indices == array.unpacked.len()).then(|| VariableDimensions {
                        declared: type_names::Declared::default(),
                        unpacked: Vec::new(),
                        ..array.clone()
                    })
                },
            )
        }
        _ => {
            return Err(invalid(
                "only a variable, a class property, a member of an unpacked structure, or an element of an unpacked array can be passed by reference (IEEE 1800-2023 13.5.2)",
            ));
        }
    };
    let Some(shape) = shape else {
        return Err(invalid(
            "only a variable, a class property, a member of an unpacked structure, or an element of an unpacked array can be passed by reference (IEEE 1800-2023 13.5.2)",
        ));
    };
    let formal = dimensions
        .subroutine_param_shapes
        .get(name)
        .and_then(|shapes| shapes.get(position));
    // Bounded and unbounded queues are equivalent (6.22.2), but a formal's
    // operations would apply its own bound to the actual.
    if let (Some(formal), Some(actual)) = (
        formal.and_then(|formal| formal.container.as_deref()),
        shape.container.as_deref(),
    ) && let (
        system_functions::ContainerKind::Queue { capacity: bound },
        system_functions::ContainerKind::Queue { capacity },
    ) = (formal.layout.kind, actual.layout.kind)
        && bound != capacity
    {
        return Err(unsupported(format!(
            "ref argument of `{name}` binding queues with different bounds"
        )));
    }
    if formal.is_some_and(|formal| !equivalent(&tree.state, formal, &shape, &dimensions.const_env))
    {
        return Err(invalid(
            "the actual's type is not equivalent to the formal's (IEEE 1800-2023 6.22.2)",
        ));
    }
    Ok(())
}

/// The shape of class property `actual`, also of unpacked structure type.
fn property_shape(
    tree: &AnalysisContext<'_>,
    actual: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<VariableDimensions>> {
    if let Some((property, _)) = objects::property(tree, actual, native, dimensions)? {
        return Ok(Some(procedural::dimensions_from_type(&property.r#type)));
    }
    // `objects::aggregate_load` rejects a receiver that calls a subroutine,
    // which each member would evaluate again.
    let Some((aggregate, _, _)) = objects::aggregate_property(tree, actual, native, dimensions)?
    else {
        return Ok(None);
    };
    Ok(Some(procedural::dimensions_from_type(&aggregate.r#type)))
}

/// Whether variables of shapes `a` and `b` have equivalent types (IEEE
/// 1800-2023 6.22.2). Integral types compare by total width, signedness and
/// state count; an enumeration only by its members; an unpacked structure
/// only with the same structure type.
fn equivalent(
    state: &AnalysisState,
    a: &VariableDimensions,
    b: &VariableDimensions,
    env: &HashMap<String, i128>,
) -> bool {
    let count = |width: &ConstExpr| eval_ast_const_expr(state, width, env);
    match (&a.container, &b.container) {
        (None, None) => {}
        (Some(a), Some(b)) => {
            use system_functions::ContainerKind;
            let same_kind = match (a.layout.kind, b.layout.kind) {
                (ContainerKind::Dynamic, ContainerKind::Dynamic)
                | (ContainerKind::Queue { .. }, ContainerKind::Queue { .. }) => true,
                (ContainerKind::Associative { .. }, ContainerKind::Associative { .. }) => {
                    match (&a.key, &b.key) {
                        (Some(x), Some(y)) => equivalent(
                            state,
                            &procedural::dimensions_from_type(x),
                            &procedural::dimensions_from_type(y),
                            env,
                        ),
                        (None, None) => true,
                        _ => false,
                    }
                }
                _ => false,
            };
            return same_kind
                && equivalent(
                    state,
                    &procedural::dimensions_from_type(&a.element),
                    &procedural::dimensions_from_type(&b.element),
                    env,
                );
        }
        _ => return false,
    }
    if a.unpacked.len() != b.unpacked.len()
        || a.unpacked
            .iter()
            .zip(&b.unpacked)
            .any(|(a, b)| count(&a.width).is_none() || count(&a.width) != count(&b.width))
    {
        return false;
    }
    if a.real != b.real || a.string != b.string || a.class != b.class {
        return false;
    }
    match (&a.unpacked_struct, &b.unpacked_struct) {
        (None, None) => {}
        (Some(a), Some(b)) if a.same_type(b) => {}
        _ => return false,
    }
    if (a.enum_domain.is_some() || b.enum_domain.is_some()) && a.enum_domain != b.enum_domain {
        return false;
    }
    let width = |shape: &VariableDimensions| {
        shape.packed.iter().try_fold(1i128, |width, dimension| {
            Some(width * count(&dimension.width)?)
        })
    };
    width(a).is_some() && width(a) == width(b) && a.signed == b.signed && a.is_2state == b.is_2state
}
