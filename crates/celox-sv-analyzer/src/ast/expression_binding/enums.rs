//! Enumerated type methods (IEEE 1800-2023 6.19.5).
//!
//! The methods are values of the members of the receiver's type. They bind
//! to [`Callee::Lookup`] calls, which evaluate the receiver once and compare
//! it with the members by case equality, so a value that is no member of
//! the type, including one with unknown bits, matches none of them.

use std::sync::Arc;

use super::direct::call_name;
use super::*;
use crate::ast::enums::EnumDomain;

/// An enumerated type method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    First,
    Last,
    Next,
    Prev,
    Num,
    Name,
}

impl Method {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "first" => Self::First,
            "last" => Self::Last,
            "next" => Self::Next,
            "prev" => Self::Prev,
            "num" => Self::Num,
            "name" => Self::Name,
            _ => return None,
        })
    }

    /// Whether the result is a string.
    pub(super) fn returns_string(self) -> bool {
        self == Self::Name
    }

    /// Whether the result is a value of the enumerated type.
    fn returns_member(self) -> bool {
        matches!(self, Self::First | Self::Last | Self::Next | Self::Prev)
    }

    /// The number of arguments the method takes at most: `next` and `prev`
    /// take a count, which defaults to 1.
    pub(super) fn formals(self) -> usize {
        usize::from(matches!(self, Self::Next | Self::Prev))
    }
}

/// The enumerated type method that native callee expression `id` names,
/// with the expression it is called on and that expression's type.
pub(super) fn method(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(Method, ExprId, Arc<EnumDomain>)>> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(ungroup(id, native)).kind
    else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let Some(method) = Method::parse(&instances::normalize_identifier(&name.spelling)) else {
        return Ok(None);
    };
    Ok(domain(tree, *expression, native, dimensions)?.map(|domain| (method, *expression, domain)))
}

/// The enumerated type of native expression `id`, judged before it is
/// bound: a variable, an element of an array, a class property, a call of
/// a function, method or enumerated type method, a cast, or the status of a
/// process.
pub(super) fn domain(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Arc<EnumDomain>>> {
    // `process::state` (9.7).
    if let Some(domain) = processes::state(tree, id, native, dimensions) {
        return Ok(Some(domain));
    }
    Ok(match &native.expression(ungroup(id, native)).kind {
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            match dimensions.get(&name) {
                Some(shape) => variable(shape),
                None => match objects::property(tree, id, native, dimensions)? {
                    Some((property, _)) => scalar(&property.r#type),
                    None => constant(&tree.state, &name, dimensions),
                },
            }
        }
        ExprKind::Member { .. } => match method(tree, id, native, dimensions)? {
            // A method called without parentheses (6.19.5.7).
            Some((method, _, domain)) => method.returns_member().then_some(domain),
            None => match objects::static_property(tree, id, native, dimensions)?
                .or_else(|| call_name(&tree.state, id, native))
                .and_then(|name| dimensions.get(&name))
            {
                // A static property or a package variable.
                Some(shape) => variable(shape),
                // A package-qualified enum constant.
                None if let Some(domain) = call_name(&tree.state, id, native)
                    .and_then(|name| constant(&tree.state, &name, dimensions)) =>
                {
                    Some(domain)
                }
                None => match objects::property(tree, id, native, dimensions)? {
                    Some((property, _)) => scalar(&property.r#type),
                    // A member of a container element of unpacked structure
                    // type.
                    None if let Some((ty, _)) = containers::element_member(
                        ungroup(id, native),
                        native,
                        tree,
                        dimensions,
                    )? =>
                    {
                        scalar(&ty)
                    }
                    // A member of a packed struct.
                    None => dimensions::operand_shape(id, tree, dimensions)
                        .ok()
                        .flatten()
                        .and_then(|shape| variable(&shape)),
                },
            },
        },
        ExprKind::Select { .. } => element(tree, id, native, dimensions)?,
        ExprKind::Call { expression, .. } => {
            if let Some((method, _, domain)) = method(tree, *expression, native, dimensions)? {
                method.returns_member().then_some(domain)
            } else if let ExprKind::Member {
                expression: base,
                name,
                ..
            } = &native.expression(ungroup(*expression, native)).kind
                && matches!(name.spelling.as_str(), "pop_front" | "pop_back")
                && let Some(container) = containers::native_type(*base, native, tree, dimensions)?
            {
                // An element popped from a queue (7.10.2.4, 7.10.2.5).
                scalar(&container.element)
            } else {
                let name = match objects::method(tree, *expression, native, dimensions)? {
                    Some((name, _, _)) => Some(name),
                    None => call_name(&tree.state, *expression, native),
                };
                name.and_then(|name| {
                    dimensions
                        .function_return_types
                        .get(&name)
                        .and_then(|metadata| metadata.enum_domain.clone())
                        .or_else(|| {
                            dimensions
                                .functions
                                .get(&name)
                                .and_then(|function| function.return_enum_domain.clone())
                        })
                })
            }
        }
        ExprKind::Cast { target, .. } => {
            let target = ungroup(*target, native);
            let ty = if let Some(name) = call_name(&tree.state, target, native) {
                dimensions.type_aliases.get(&name).cloned()
            } else if let ExprKind::Type(id) = native.expression(target).kind {
                type_binding::binding::bind_in(id, native, tree, dimensions)?
            } else {
                None
            };
            ty.as_ref().and_then(scalar)
        }
        ExprKind::Conditional {
            then_expression,
            else_expression,
            ..
        } => {
            let then_domain = domain(tree, *then_expression, native, dimensions)?;
            let else_domain = domain(tree, *else_expression, native, dimensions)?;
            then_domain.filter(|domain| else_domain.as_ref() == Some(domain))
        }
        _ => None,
    })
}

/// The enumerated type of a value of type `ty`.
fn scalar(ty: &Type) -> Option<Arc<EnumDomain>> {
    (ty.unpacked_ranges.is_empty() && ty.container.is_none())
        .then(|| ty.enum_domain.clone())
        .flatten()
}

/// The enumerated type of a value of a variable of shape `shape`.
fn variable(shape: &VariableDimensions) -> Option<Arc<EnumDomain>> {
    (shape.unpacked.is_empty() && shape.container.is_none())
        .then(|| shape.enum_domain.clone())
        .flatten()
}

/// The enumerated type of enum constant `name`: the named type of its scope
/// declaring a member of that name and value. A qualified constant is one
/// of a type of the scope that qualifies it, and an unqualified one that no
/// local type declares is the package item an import makes visible; other
/// scopes may declare members of the same name and value.
fn constant(
    state: &AnalysisState,
    name: &str,
    dimensions: &PackedDimensions,
) -> Option<Arc<EnumDomain>> {
    let value = *dimensions.const_env.get(name)?;
    let find = |scope: Option<&str>, member: &str| {
        let mut candidates = dimensions
            .type_aliases
            .iter()
            .filter(|(alias, _)| match scope {
                Some(scope) => alias
                    .strip_prefix(scope)
                    .is_some_and(|rest| rest.starts_with("::")),
                None => !alias.contains("::"),
            })
            .filter_map(|(_, ty)| ty.enum_domain.as_ref())
            .filter(|domain| {
                domain
                    .members
                    .iter()
                    .any(|candidate| candidate.name == member && candidate.value == value)
            });
        let domain = candidates.next()?;
        candidates
            .all(|other| other == domain)
            .then(|| domain.clone())
    };
    match name.rsplit_once("::") {
        Some((scope, member)) => find(Some(scope), member),
        None => find(None, name).or_else(|| {
            let qualified = scope::imported(state).aliases.get(name)?.clone();
            let (scope, member) = qualified.rsplit_once("::")?;
            find(Some(scope), member)
        }),
    }
}

/// The enumerated type of an element select `id` of an unpacked array or
/// container whose elements are of one.
fn element(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Arc<EnumDomain>>> {
    let mut root = ungroup(id, native);
    let mut selects = 0;
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(root).kind
    {
        if !matches!(dimension.kind, DimensionKind::Index(_)) {
            return Ok(None);
        }
        if selects == 0
            && let Some(container) = containers::native_type(*expression, native, tree, dimensions)?
        {
            return Ok(scalar(&container.element));
        }
        selects += 1;
        root = ungroup(*expression, native);
    }
    let (unpacked, domain, packed_enum) = match &native.expression(root).kind {
        ExprKind::Name(name)
            if let Some(shape) =
                dimensions.get(&instances::normalize_identifier(&name.spelling)) =>
        {
            if shape.container.is_some() {
                return Ok(None);
            }
            (
                shape.unpacked.len(),
                shape.enum_domain.clone(),
                shape.packed_enum,
            )
        }
        // A static property or a package variable.
        ExprKind::Member { .. }
            if let Some(shape) = objects::static_property(tree, root, native, dimensions)?
                .or_else(|| call_name(&tree.state, root, native))
                .and_then(|name| dimensions.get(&name)) =>
        {
            if shape.container.is_some() {
                return Ok(None);
            }
            (
                shape.unpacked.len(),
                shape.enum_domain.clone(),
                shape.packed_enum,
            )
        }
        ExprKind::Name(_) | ExprKind::Member { .. } => {
            match objects::property(tree, root, native, dimensions)? {
                Some((property, _)) if property.r#type.container.is_none() => (
                    property.r#type.unpacked_ranges.len(),
                    property.r#type.enum_domain.clone(),
                    property.r#type.packed_enum,
                ),
                _ => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    if packed_enum && selects > unpacked {
        return Err(unsupported(
            "method of an element of a packed array of enums",
        ));
    }
    Ok(domain.filter(|_| unpacked == selects))
}

/// The value of `method` of `domain` called on `receiver` with `count`.
pub(super) fn call(
    state: &AnalysisState,
    method: Method,
    domain: &EnumDomain,
    receiver: Option<Expr>,
    count: Option<Expr>,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let members = &domain.members;
    let member = |index: usize| {
        Expr::Literal(format_typed_parameter_literal(
            members[index].value,
            domain.width,
            domain.signed,
        ))
    };
    let int = |value: usize| Expr::Literal(format_typed_parameter_literal(value as i128, 32, true));
    let wide =
        |value: usize| Expr::Literal(format_typed_parameter_literal(value as i128, 64, false));
    let lookup = |width, signed, selector, pairs: Vec<(Expr, Expr)>, default| Expr::Call {
        callee: Callee::Lookup { width, signed },
        args: std::iter::once(Some(selector))
            .chain(
                pairs
                    .into_iter()
                    .flat_map(|(key, value)| [Some(key), Some(value)]),
            )
            .chain(std::iter::once(Some(default)))
            .collect(),
        arg_order: Vec::new(),
    };
    let count_members = members.len();
    let receiver = receiver.ok_or_else(disagreement)?;
    // A result that does not depend on the receiver still evaluates it once,
    // for the effects of the calls it makes.
    let evaluated = |value: Expr, width, signed| {
        let mut effects = false;
        lifetimes::visit_calls(&receiver, &mut |callee, _| effects |= callee.is_effectful());
        if effects {
            lookup(width, signed, receiver.clone(), Vec::new(), value)
        } else {
            value
        }
    };
    Ok(match method {
        Method::First => evaluated(member(0), domain.width, domain.signed),
        Method::Last => evaluated(member(count_members - 1), domain.width, domain.signed),
        Method::Num => evaluated(int(count_members), 32, true),
        Method::Next | Method::Prev => {
            // A value that is no member gives the default initial value of
            // the base type (6.19.5.3, 6.19.5.4, Table 6-7).
            let default = Expr::Literal(if domain.four_state {
                format!("{}'bx", domain.width)
            } else {
                format!("{}'b0", domain.width)
            });
            // `int unsigned N`: only N modulo the number of members matters.
            // The count converts to the formal's type as in an assignment,
            // extended by its own signedness; the type is 2-state, so unknown
            // bits become 0.
            let count = casts::runtime_cast_target_expr(
                state,
                casts::RuntimeCastTarget::Integral {
                    target: ExprType {
                        width: 32,
                        signed: false,
                    },
                    numeric_size: false,
                    two_state: true,
                },
                count.unwrap_or_else(|| int(1)),
                dimensions,
            )?;
            let constant = expr_to_const(state, count.clone())
                .and_then(|count| eval_ast_const_expr(state, &count, &dimensions.const_env))
                .map(|count| (count as u32 as usize) % count_members);
            if let Some(shift) = constant {
                let shift = if method == Method::Next {
                    shift
                } else {
                    count_members - shift
                };
                let pairs = (0..count_members)
                    .map(|index| (member(index), member((index + shift) % count_members)))
                    .collect();
                lookup(
                    domain.width,
                    domain.signed,
                    receiver.clone(),
                    pairs,
                    default,
                )
            } else {
                // The receiver's index, or 2N for a value that is no member,
                // plus the shift selects from the members repeated twice.
                let index = lookup(
                    64,
                    false,
                    receiver.clone(),
                    (0..count_members)
                        .map(|index| (member(index), wide(index)))
                        .collect(),
                    wide(2 * count_members),
                );
                let shift = Expr::Resize {
                    expr: Box::new(Expr::Binary {
                        left: Box::new(count),
                        op: BinaryOp::Mod,
                        right: Box::new(Expr::Literal(format_typed_parameter_literal(
                            count_members as i128,
                            32,
                            false,
                        ))),
                    }),
                    width: 64,
                    signed: false,
                };
                let position = if method == Method::Next {
                    Expr::Binary {
                        left: Box::new(index),
                        op: BinaryOp::Add,
                        right: Box::new(shift),
                    }
                } else {
                    Expr::Binary {
                        left: Box::new(Expr::Binary {
                            left: Box::new(index),
                            op: BinaryOp::Add,
                            right: Box::new(wide(count_members)),
                        }),
                        op: BinaryOp::Sub,
                        right: Box::new(shift),
                    }
                };
                let pairs = (0..2 * count_members)
                    .map(|position| (wide(position), member(position % count_members)))
                    .collect();
                lookup(domain.width, domain.signed, position, pairs, default)
            }
        }
        Method::Name => {
            // The bytes of each name, which a value that is no member
            // replaces by the empty string (6.19.5.6).
            let width = 8 * members
                .iter()
                .map(|member| member.name.len())
                .max()
                .unwrap_or(0)
                .max(1);
            let pairs = (0..count_members)
                .map(|index| {
                    let bytes = members[index].name.as_bytes();
                    let name = if bytes.is_empty() {
                        "8'h0".to_string()
                    } else {
                        format!(
                            "{}'h{}",
                            8 * bytes.len(),
                            bytes
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>()
                        )
                    };
                    (member(index), Expr::Literal(name))
                })
                .collect();
            let bytes = lookup(
                width,
                false,
                receiver.clone(),
                pairs,
                Expr::Literal(format!("{width}'h0")),
            );
            strings::call(StringOp::FromBits, vec![bytes])
        }
    })
}
