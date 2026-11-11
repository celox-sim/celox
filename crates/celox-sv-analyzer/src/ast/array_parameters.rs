//! Parameters of unpacked array type (IEEE 1800-2023 6.20.2), such as the
//! table `localparam logic [2:0] T [4] = '{...};`, and parameters whose value
//! is an assignment pattern, such as a packed structure `S'{m: 1, k: 2}`.
//! Such a parameter becomes a module variable whose initial value is the
//! pattern; nothing else writes it, so it holds that value throughout the
//! simulation.

use super::*;

/// A parameter of unpacked array type, as the variable that holds it.
pub(super) struct ArrayParameter {
    pub signal: Signal,
    native_pattern: Option<celox_sv_syntax::ExprId>,
    /// A default that is not an assignment pattern: an expression of the
    /// array type, such as another array parameter or a conditional of
    /// array parameters and patterns.
    native_value: Option<celox_sv_syntax::ExprId>,
    bound: Option<BoundPattern>,
}

/// The elements of an unpacked array parameter whose value is a constant.
pub(super) struct ConstantElements {
    /// The bounds of the array's one dimension.
    pub left: i128,
    pub right: i128,
    /// The width and sign of an element, and whether its type is two-state.
    pub width: usize,
    pub signed: bool,
    pub two_state: bool,
    /// The bounds of the packed dimensions of an element, outermost first,
    /// which its constant selects take (one `[width-1:0]` for an integer
    /// atom); none when they are not known, or for an element of a named
    /// signed type, whose select is signed (IEEE 1800-2023 7.4.1).
    pub packed: Vec<(i128, i128)>,
    /// The whole value, as later array parameters of the scope read it.
    pub value: Expr,
    /// The elements in storage order, the left one first.
    pub values: Vec<i128>,
}

/// The most elements of an array parameter that are published as constants
/// of its scope.
const MAX_CONSTANT_ELEMENTS: usize = 4096;

/// The elements of the parameter `native` of a one-dimensional unpacked array
/// type with packed integral elements, when its value (its own, or the one
/// its instantiation binds) is a constant in `dims`. Elements wider than the
/// numeric environment holds, or with unknown bits, leave the parameter to
/// the variable that holds it.
/// Also the shape of the variable that holds it, which a later array
/// parameter that takes its value names.
pub(super) fn constant_elements_and_shape(
    native: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Option<(Option<ConstantElements>, VariableDimensions)> {
    let parameter = array_parameters_from_declaration(
        &[native],
        tree,
        session,
        &dims.const_env,
        &dims.type_aliases,
        &mut |_| {},
    )
    .ok()?
    .pop()?;
    let shape = procedural::dimensions_from_type(parameter.signal.r#type());
    Some((parameter.constant_elements(tree, session, dims), shape))
}

impl ArrayParameter {
    /// The elements of the parameter when it is a one-dimensional array of
    /// packed integral elements whose value is a constant in `dims` (see
    /// [`constant_elements`]).
    pub(super) fn constant_elements(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        dims: &PackedDimensions,
    ) -> Option<ConstantElements> {
        let parameter = self;
        let shape = procedural::dimensions_from_type(parameter.signal.r#type());
        let [dimension] = shape.unpacked.as_slice() else {
            return None;
        };
        if shape.real
            || shape.string
            || shape.chandle
            || shape.class.is_some()
            || shape.container.is_some()
            || shape.virtual_interface.is_some()
            || shape.unpacked_struct.is_some()
        {
            return None;
        }
        let bound =
            |bound: &ConstExpr| eval_ast_const_expr(&tree.functions, bound, &dims.const_env);
        let (left, right) = (bound(&dimension.left)?, bound(&dimension.right)?);
        let count = usize::try_from(left.abs_diff(right))
            .ok()?
            .checked_add(1)
            .filter(|count| *count <= MAX_CONSTANT_ELEMENTS)?;
        let process = parameter.initial_process(tree, session, dims).ok()?;
        let Some(Stmt::Assign { rhs, .. }) = process.body.into_iter().next() else {
            return None;
        };
        let literals = dims
            .parameter_values
            .iter()
            .filter(|(name, _)| !dims.const_env.contains_key(*name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let value = substitute_expr_constants_with_parameter_literals(
            tree,
            session,
            rhs,
            &dims.const_env,
            &literals,
        );
        let Expr::Literal(text) =
            fold_const_integral_expr_preserving_mask(tree, session, value, &dims.const_env)
        else {
            return None;
        };
        let literal = typecheck::parse_integral_literal(&text)?;
        if literal.mask != num_bigint::BigUint::default() || literal.width % count != 0 {
            return None;
        }
        let width = literal.width / count;
        if width == 0 || width > 127 {
            return None;
        }
        let window = (num_bigint::BigUint::from(1u8) << width) - 1u8;
        let values = (0..count)
            .map(|position| {
                u128::try_from((&literal.value >> (position * width)) & &window)
                    .ok()
                    .map(|value| value as i128)
            })
            .collect::<Option<Vec<_>>>()?;
        let packed = if shape.signed_element_depth.is_some() {
            Vec::new()
        } else {
            let mut packed = shape
                .packed
                .iter()
                .map(|dimension| Some((bound(&dimension.left)?, bound(&dimension.right)?)))
                .collect::<Option<Vec<_>>>()
                .unwrap_or_default();
            if packed.is_empty() {
                packed.push((width as i128 - 1, 0));
            }
            if parameters::packed_width(&packed) == Some(width) {
                packed
            } else {
                Vec::new()
            }
        };
        Some(ConstantElements {
            left,
            right,
            width,
            packed,
            signed: shape.signed,
            two_state: shape.is_2state,
            value: Expr::Literal(text),
            values,
        })
    }
}

/// The value of the expression `root` as the value of the array parameter
/// `name` of shape `shape`: a pattern, an expression that names an unpacked
/// array, or a conditional of these whose condition is a constant. Every
/// operand of a conditional is a value of the parameter, also the one that
/// is not selected (IEEE 1800-2023 11.4.11).
fn array_value(
    root: celox_sv_syntax::ExprId,
    shape: &VariableDimensions,
    name: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    let native = tree.declarations();
    let id = expression_binding::ungroup(root, native);
    match native.expression(id).kind {
        celox_sv_syntax::ExprKind::Conditional {
            condition,
            then_expression,
            else_expression,
        } => {
            let condition = constant_binding::bind(condition, tree, session, dims)?
                .and_then(|value| eval_ast_const_expr(&tree.functions, &value, &dims.const_env))
                .ok_or_else(|| {
                    unsupported("nonconstant or ambiguous condition in array parameter argument")
                })?;
            let then_value = array_value(then_expression, shape, name, tree, session, dims)?;
            let else_value = array_value(else_expression, shape, name, tree, session, dims)?;
            Ok(if condition != 0 {
                then_value
            } else {
                else_value
            })
        }
        celox_sv_syntax::ExprKind::Pattern { .. } => {
            patterns::native_expression(id, shape, tree, session, dims)?
                .ok_or_else(|| unsupported("array parameter pattern target"))
        }
        _ => {
            let actual = instances::array_argument_shape(id, tree, session, dims)
                .ok_or_else(|| unsupported(format!("value of array parameter `{name}`")))?;
            bound_patterns::from_array_parameter(id, tree, session, dims, &actual)?
                .lower(tree, session, shape, dims)
        }
    }
}

/// The operand a conditional expression of an unpacked array type selects.
/// Aggregate conditional expressions require equivalent branch types (IEEE
/// 1800-2023 11.4.11). A known condition selects a whole array before element
/// binding; treating it as a packed mux would use the wrong X merge rule.
pub(super) fn selected_branch(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Converted<celox_sv_syntax::ExprId> {
    let native = tree.declarations();
    let mut selected = expression_binding::ungroup(root, native);
    while let celox_sv_syntax::ExprKind::Conditional {
        condition,
        then_expression,
        else_expression,
    } = native.expression(selected).kind
    {
        let condition = constant_binding::bind(condition, tree, session, dims)?
            .and_then(|value| eval_ast_const_expr(&tree.functions, &value, &dims.const_env))
            .ok_or_else(|| {
                unsupported("nonconstant or ambiguous condition in array parameter argument")
            })?;
        selected = expression_binding::ungroup(
            if condition != 0 {
                then_expression
            } else {
                else_expression
            },
            native,
        );
    }
    Ok(selected)
}

/// An authored unpacked parameter or an assignment-pattern value.
pub(super) fn is_array_parameter(
    parameter: &celox_sv_syntax::Parameter,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    let celox_sv_syntax::ParameterKind::Value {
        dimensions,
        default,
        ..
    } = &parameter.kind
    else {
        return false;
    };
    !dimensions.is_empty()
        || default.is_some_and(|id| {
            matches!(
                native
                    .expression(expression_binding::ungroup(id, native))
                    .kind,
                celox_sv_syntax::ExprKind::Pattern { .. }
            )
        })
}

fn declared_type(
    native: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    type_aliases: &TypeAliases,
) -> Converted<Option<Type>> {
    let celox_sv_syntax::ParameterKind::Value { data_type, .. } = native.kind else {
        return Err(unsupported(
            "type parameter used as an array value parameter",
        ));
    };
    type_binding::bind(
        data_type,
        tree,
        session,
        &PackedDimensions::new(HashMap::default(), const_env, type_aliases),
    )
}

/// A scalar value override replaces a packed pattern default. Keep unpacked
/// values and caller-bound patterns on the aggregate binding path.
pub(super) fn uses_signal_representation(
    parameter: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<bool> {
    if !is_array_parameter(parameter, tree.declarations()) {
        return Ok(false);
    }
    let name = instances::normalize_identifier(&parameter.name.spelling);
    if !parameter.local
        && tree.parameter_overrides.contains_key(&name)
        && matches!(&parameter.kind, celox_sv_syntax::ParameterKind::Value { dimensions, .. } if dimensions.is_empty())
        && declared_type(parameter, tree, session, const_env, aliases)?
            .is_some_and(|ty| ty.unpacked_ranges.is_empty())
    {
        return Ok(false);
    }
    Ok(true)
}

/// The value of a parameter of packed type that is given by an assignment
/// pattern, its own or one its instantiation binds, when the pattern is a
/// constant in `dims`: the literal of the pattern's value in the declared
/// type (IEEE 1800-2023 6.20.2, 10.9). `None` leaves the parameter to the
/// constant variable that holds a pattern which is not one here.
pub(super) fn packed_pattern_value(
    native: &celox_sv_syntax::Parameter,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> Option<ConstExpr> {
    let celox_sv_syntax::ParameterKind::Value { dimensions, .. } = &native.kind else {
        return None;
    };
    if !dimensions.is_empty() {
        return None;
    }
    let ty = declared_type(native, tree, session, &dims.const_env, &dims.type_aliases)
        .ok()
        .flatten()?;
    let shape = procedural::dimensions_from_type(&ty);
    if !shape.unpacked.is_empty()
        || shape.packed.is_empty()
        || shape.real
        || shape.string
        || shape.chandle
        || shape.class.is_some()
        || shape.container.is_some()
        || shape.virtual_interface.is_some()
        || shape.unpacked_struct.is_some()
    {
        return None;
    }
    let bound = scope::imported(tree)
        .pattern_parameter_bindings
        .get(&native.id)
        .cloned();
    let value = if let Some(bound) = bound {
        bound.lower(tree, session, &shape, dims).ok()?
    } else {
        let root = parameters::ownership::default(native).filter(|id| {
            matches!(
                tree.declarations()
                    .expression(expression_binding::ungroup(*id, tree.declarations()))
                    .kind,
                celox_sv_syntax::ExprKind::Pattern { .. }
            )
        })?;
        patterns::native_expression(root, &shape, tree, session, dims)
            .ok()
            .flatten()?
    };
    let width = patterns::shape_width(&tree.functions, &shape, dims)?;
    // The pattern's concatenation is self-determined in the declared type
    // before any conversion at a receiving declaration (6.20.2).
    let mut value = Expr::Resize {
        expr: Box::new(value),
        width,
        signed: shape.signed,
    };
    if shape.is_2state {
        value = Expr::Unary {
            op: UnaryOp::ToTwoState,
            expr: Box::new(value),
        };
    }
    let literals = dims
        .parameter_values
        .iter()
        .filter(|(name, _)| !dims.const_env.contains_key(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let value = substitute_expr_constants_with_parameter_literals(
        tree,
        session,
        value,
        &dims.const_env,
        &literals,
    );
    match fold_const_integral_expr_preserving_mask(tree, session, value, &dims.const_env) {
        Expr::Literal(text) => Some(ConstExpr::Literal(text)),
        _ => None,
    }
}

/// Array parameters declared directly in the selected native unit/scope.
/// The names in `values` are value parameters already.
pub(super) fn array_parameters_from_scope(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    type_aliases: &TypeAliases,
    values: &[Parameter],
) -> Converted<Vec<ArrayParameter>> {
    let values: HashSet<&str> = values.iter().map(Parameter::name).collect();
    let is_array = |parameter: &&celox_sv_syntax::Parameter| {
        matches!(parameter.kind, celox_sv_syntax::ParameterKind::Value { .. })
            && !values.contains(instances::normalize_identifier(&parameter.name.spelling).as_str())
    };
    let mut parameters = Vec::new();
    if let Ok(unit) = tree.root_unit() {
        let authored = unit
            .parameters
            .iter()
            .flatten()
            .filter(is_array)
            .collect::<Vec<_>>();
        parameters.extend(array_parameters_from_declaration(
            &authored,
            tree,
            session,
            const_env,
            type_aliases,
            &mut |_| {},
        )?);
    }
    for id in tree.body_membership()? {
        if let celox_sv_syntax::BodyItemKind::Parameter(id) = tree.declarations().body_item(id).kind
        {
            let authored = tree
                .declarations()
                .parameter_declaration(id)
                .parameters
                .iter()
                .filter(is_array)
                .collect::<Vec<_>>();
            parameters.extend(array_parameters_from_declaration(
                &authored,
                tree,
                session,
                const_env,
                type_aliases,
                &mut |_| {},
            )?);
        }
    }
    Ok(parameters)
}

/// A scope scan installs each array parameter declaration in source order.
fn array_parameters_from_declaration(
    authored: &[&celox_sv_syntax::Parameter],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    type_aliases: &TypeAliases,
    publish: &mut impl FnMut(&ArrayParameter),
) -> Result<Vec<ArrayParameter>, AnalyzerError> {
    let mut parameters = Vec::new();
    for &native in authored {
        let bound = scope::imported(tree)
            .pattern_parameter_bindings
            .get(&native.id)
            .cloned();
        if !uses_signal_representation(native, tree, session, const_env, type_aliases)?
            && bound.is_none()
        {
            continue;
        }
        let name = instances::normalize_identifier(&native.name.spelling);
        let element =
            declared_type(native, tree, session, const_env, type_aliases)?.ok_or_else(|| {
                AnalyzerError::Unsupported(format!("type of array parameter `{name}`"))
            })?;
        let celox_sv_syntax::ParameterKind::Value { dimensions, .. } = &native.kind else {
            return Err(AnalyzerError::Parse(
                "native array parameter kind disagrees".into(),
            ));
        };
        let ranges = dimensions
            .iter()
            .map(|dimension| {
                constant_binding::unpacked_dimension(
                    dimension,
                    tree,
                    session,
                    const_env,
                    type_aliases,
                )
            })
            .collect::<Converted<Vec<_>>>()?;
        let default = parameters::ownership::default(native);
        let pattern = default.filter(|id| {
            matches!(
                tree.declarations()
                    .expression(expression_binding::ungroup(*id, tree.declarations()))
                    .kind,
                celox_sv_syntax::ExprKind::Pattern { .. }
            )
        });
        if default.is_none() && bound.is_none() {
            return Err(AnalyzerError::Unsupported(format!(
                "value of array parameter `{name}`"
            )));
        }
        // A supplied caller-bound pattern takes precedence over the authored
        // default. Only defaults actually consumed here require their native ID.
        let native_pattern = if bound.is_none() { pattern } else { None };
        let native_value = if bound.is_none() && pattern.is_none() {
            default
        } else {
            None
        };
        let parameter = ArrayParameter {
            signal: Signal::new(name, type_with_unpacked_ranges(element, ranges)?),
            native_pattern,
            native_value,
            bound,
        };
        // Publish before the next declarator can query this array's shape.
        publish(&parameter);
        parameters.push(parameter);
    }
    Ok(parameters)
}

impl ArrayParameter {
    /// The `initial` block that gives the variable its value.
    pub(super) fn initial_process(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        dims: &PackedDimensions,
    ) -> Result<InitialProcess, AnalyzerError> {
        let name = self.signal.name().to_string();
        let shape = procedural::dimensions_from_type(self.signal.r#type());
        let value = if let Some(bound) = &self.bound {
            bound.lower(tree, session, &shape, dims)?
        } else {
            match self.native_pattern {
                Some(root) => match patterns::native_expression(root, &shape, tree, session, dims)?
                {
                    Some(value) => value,
                    None => return Err(unsupported("array parameter pattern target")),
                },
                None => {
                    let root = self.native_value.ok_or_else(|| {
                        AnalyzerError::Parse("native array parameter default is missing".into())
                    })?;
                    array_value(root, &shape, &name, tree, session, dims)?
                }
            }
        };
        Ok(InitialProcess {
            creation_order: 0,
            site: None,
            condition: None,
            body: vec![Stmt::Assign {
                lhs: LValue::Ident(name),
                rhs: value,
                nonblocking: false,
            }],
            kind: celox_design::ProcessKind::Regular,
            validation_body: Vec::new(),
            synthetic: true,
            repeats: false,
        })
    }
}
