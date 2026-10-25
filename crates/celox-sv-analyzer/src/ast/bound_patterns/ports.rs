//! Port patterns bind values in the caller and sizes in the specialized callee.
use super::*;

/// Keep the operand's type independently of its lowered bit expression. A
/// conditional retains matching branch types; calls resolve after caller
/// signatures are complete, before inlining erases their declared return type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in crate::ast) enum ValueType {
    Known(Option<Type>),
    Call(Expr),
    Conditional(Box<Self>, Box<Self>),
}

impl ValueType {
    pub(super) fn known(&self) -> Option<&Type> {
        match self {
            Self::Known(ty) => ty.as_ref(),
            _ => None,
        }
    }

    fn resolve(&self, dims: &PackedDimensions) -> Option<Type> {
        match self {
            Self::Known(ty) => ty.clone(),
            Self::Call(Expr::Call {
                callee: Callee::Subroutine(name),
                ..
            }) => dims
                .function_return_types
                .get(name)
                .and_then(|metadata| metadata.pattern_type.clone()),
            Self::Call(_) => None,
            Self::Conditional(left, right) => {
                let left = left.resolve(dims)?;
                (right.resolve(dims).as_ref() == Some(&left)).then_some(left)
            }
        }
    }

    fn map_expressions(&mut self, map: &mut impl FnMut(Expr) -> Expr) {
        match self {
            Self::Known(_) => {}
            Self::Call(expr) => *expr = map(expr.clone()),
            Self::Conditional(left, right) => {
                left.map_expressions(map);
                right.map_expressions(map);
            }
        }
    }
}

pub(in crate::ast) fn from_port(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<BoundPattern> {
    match binding::value_mode(root, tree, dims, false)? {
        Value::Pattern(pattern) => Ok(pattern),
        _ => Err(unsupported("untyped port assignment pattern")),
    }
}

impl BoundPattern {
    pub(in crate::ast) fn resolve_types(&mut self, dims: &PackedDimensions) -> Converted<()> {
        fn value(value: &mut Value, dims: &PackedDimensions) -> Converted<()> {
            match value {
                Value::Pattern(pattern) => pattern.resolve_types(dims)?,
                Value::Expression {
                    expr,
                    signed,
                    value_type,
                } => {
                    if signed.is_none() {
                        *signed = Some(
                            dimensions::expression_signedness(expr, dims, &dims.const_env)
                                .ok_or_else(|| {
                                    unsupported("assignment pattern operand signedness")
                                })?,
                        );
                    }
                    *value_type = ValueType::Known(value_type.resolve(dims));
                }
            }
            Ok(())
        }
        match &mut self.items {
            Items::List(items) | Items::Repeat(_, items) => {
                for item in items {
                    value(item, dims)?;
                }
            }
            Items::Keyed(items) => {
                for (_, item) in items {
                    value(item, dims)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(in crate::ast) fn map_expressions(&mut self, map: &mut impl FnMut(Expr) -> Expr) {
        fn value(value: &mut Value, map: &mut impl FnMut(Expr) -> Expr) {
            match value {
                Value::Pattern(pattern) => pattern.map_expressions(map),
                Value::Expression {
                    expr, value_type, ..
                } => {
                    *expr = map(expr.clone());
                    value_type.map_expressions(map);
                }
            }
        }
        match &mut self.items {
            Items::Value(expr) | Items::ArrayValue { value: expr, .. } => {
                *expr = map(expr.clone());
            }
            Items::List(items) | Items::Repeat(_, items) => {
                for item in items {
                    value(item, map);
                }
            }
            Items::Keyed(items) => {
                for (_, item) in items {
                    value(item, map);
                }
            }
        }
    }

    pub(crate) fn bind_port(&self, target: &Type) -> Converted<crate::ir::Expr> {
        let state = AnalysisState::default();
        let shape = procedural::dimensions_from_type(target);
        self.lower(&state, &shape, &PackedDimensions::default())
            .map(Into::into)
    }
}

impl Type {
    /// Freeze declaration bounds while constant functions and child parameters
    /// are available. Caller identifiers in pattern values never enter this scope.
    pub(crate) fn pattern_target(
        &self,
        constants: &dyn typecheck::ConstantEnvironment,
    ) -> Option<Self> {
        let literal = |expr: &ConstExpr| {
            typecheck::eval_const_expr_in_env(&expr.clone().into(), constants)
                .map(|value| ConstExpr::Literal(value.to_string()))
        };
        let mut ty = self.clone();
        for range in &mut ty.packed_ranges {
            range.left = literal(&range.left)?;
            range.right = literal(&range.right)?;
        }
        for range in &mut ty.unpacked_ranges {
            range.left = literal(&range.left)?;
            range.right = literal(&range.right)?;
            range.size = match range.size.as_ref() {
                Some(size) => Some(literal(size)?),
                None => None,
            };
        }
        ty.members = ty
            .members
            .into_iter()
            .map(|member| {
                let ty = member.r#type().pattern_target(constants)?;
                Some(member.with_type(|_| ty))
            })
            .collect::<Option<_>>()?;
        Some(ty)
    }
}

/// Capture aggregate identity before caller names and dimensions are lowered.
pub(in crate::ast) fn native_value_type(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<Option<Type>> {
    native_value_type_plan(root, tree, dims).map(|ty| ty.resolve(dims))
}

pub(super) fn native_value_type_plan(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    dims: &PackedDimensions,
) -> Converted<ValueType> {
    use celox_sv_syntax::ExprKind;
    enum Job {
        Enter(celox_sv_syntax::ExprId),
        Conditional,
    }
    let native = tree.declarations();
    let mut jobs = vec![Job::Enter(root)];
    let mut values = Vec::new();
    while let Some(job) = jobs.pop() {
        match job {
            Job::Enter(root) => {
                let root = expression_binding::ungroup(root, native);
                match native.expression(root).kind {
                    ExprKind::Conditional {
                        then_expression,
                        else_expression,
                        ..
                    } => {
                        jobs.push(Job::Conditional);
                        jobs.push(Job::Enter(else_expression));
                        jobs.push(Job::Enter(then_expression));
                    }
                    ExprKind::Call { .. } => {
                        values.push(ValueType::Call(expression_binding::bind(root, tree, dims)?))
                    }
                    _ => {
                        let shape =
                            if matches!(native.expression(root).kind, ExprKind::Pattern { .. }) {
                                patterns::typed_shape(root, tree, dims)?
                            } else {
                                dimensions::operand_shape(root, tree, dims)?
                            };
                        values.push(ValueType::Known(
                            shape
                                .filter(|shape| {
                                    !shape.members.is_empty()
                                        || !shape.unpacked.is_empty()
                                        || shape.packed.len() > 1
                                })
                                .and_then(|shape| shape.pattern_type(&tree.state, dims)),
                        ));
                    }
                }
            }
            Job::Conditional => {
                let right = values
                    .pop()
                    .ok_or_else(|| unsupported("pattern operand type"))?;
                let left = values
                    .pop()
                    .ok_or_else(|| unsupported("pattern operand type"))?;
                values.push(ValueType::Conditional(Box::new(left), Box::new(right)));
            }
        }
    }
    values
        .pop()
        .ok_or_else(|| unsupported("pattern operand type"))
}

impl VariableDimensions {
    pub(in crate::ast) fn pattern_type(
        &self,
        state: &AnalysisState,
        dims: &PackedDimensions,
    ) -> Option<Type> {
        let mut ty = Type::new(if self.is_2state {
            TypeKind::Bit
        } else {
            TypeKind::Logic
        });
        ty.is_signed = self.signed;
        ty.packed_ranges = self
            .packed
            .iter()
            .map(|dim| PackedRange::new(dim.left.clone(), dim.right.clone()))
            .collect();
        ty.unpacked_ranges = self
            .unpacked
            .iter()
            .map(|dim| UnpackedRange::new(dim.left.clone(), dim.right.clone()))
            .collect();
        ty.members = self.members.clone();
        ty.signed_element_depth = self.signed_element_depth;
        ty.pattern_target(&const_functions::Environment {
            constants: &*dims.const_env,
            functions: &state.const_functions,
        })
    }
}
