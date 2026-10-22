//! Class object handles (IEEE 1800-2023 clause 8).
//!
//! A property access through a handle becomes an [`ObjectOp`] call that
//! carries the property's layout, and a method call becomes a call of the
//! method's subroutine with the handle as its first argument. Inside a
//! method, unqualified property and method names refer to `this`.

use std::rc::Rc;

use super::*;
use crate::ast::classes::{self, Class, THIS};
use crate::system_functions::ObjectOp;

/// The class `name`, laid out.
pub(in crate::ast) fn class(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    name: &str,
) -> Converted<Rc<Class>> {
    classes::class(tree, name, &dimensions.const_env, &dimensions.type_aliases)
}

/// The class of native expression `id` when it is an object handle, judged
/// before it is bound: a handle variable, or a handle property.
pub(in crate::ast) fn native_class(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<String> {
    if let ExprKind::Name(name) = &native.expression(ungroup(id, native)).kind
        && let Some(ty) = dimensions.get(&instances::normalize_identifier(&name.spelling))
    {
        return ty.class.clone();
    }
    let (property, _) = property(tree, id, native, dimensions).ok()??;
    property.r#type.class().map(str::to_owned)
}

/// The class whose method is being bound, whose properties and methods
/// unqualified names may refer to.
pub(in crate::ast) fn scope_class(dimensions: &PackedDimensions) -> Option<String> {
    dimensions.get(THIS).and_then(|ty| ty.class.clone())
}

/// The property that native expression `id` names, `h.p` or an unqualified
/// `p` inside a method of its class, with the receiver: the expression of
/// the handle, or `None` for the enclosing method's `this`.
pub(in crate::ast) fn property(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(classes::Property, Option<ExprId>)>> {
    let (class_name, name, receiver) = match &native.expression(ungroup(id, native)).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." => match native_class(tree, *expression, native, dimensions) {
            Some(class) => (class, name, Some(*expression)),
            None => return Ok(None),
        },
        ExprKind::Name(name) => {
            if dimensions
                .get(&instances::normalize_identifier(&name.spelling))
                .is_some()
            {
                return Ok(None);
            }
            match scope_class(dimensions) {
                Some(class) => (class, name, None),
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let class = class(tree, dimensions, &class_name)?;
    Ok(class
        .property(&instances::normalize_identifier(&name.spelling))
        .map(|property| (property.clone(), receiver)))
}

/// The handle of the object that `receiver` of [`property`] refers to.
pub(in crate::ast) fn receiver(
    tree: &AnalysisContext<'_>,
    receiver: Option<ExprId>,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    match receiver {
        Some(receiver) => bind_value(receiver, native, tree, dimensions, None),
        None => Ok(this()),
    }
}

/// A read of property `property` of the object `handle` of class `class`.
pub(in crate::ast) fn load(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    class_name: &str,
    property: &str,
    handle: Expr,
) -> Converted<Expr> {
    let class = class(tree, dimensions, class_name)?;
    let property = class
        .property(property)
        .ok_or_else(|| unsupported(format!("property `{property}` of class `{class_name}`")))?;
    Ok(classes::object_call(
        ObjectOp::Load(property.field),
        vec![handle],
    ))
}

/// The unqualified name `name` inside a method of the scope class, when it
/// names one of its properties: a read through `this`.
pub(in crate::ast) fn unqualified_property(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    name: &str,
) -> Converted<Option<Expr>> {
    if dimensions.get(name).is_some() {
        return Ok(None);
    }
    let Some(class_name) = scope_class(dimensions) else {
        return Ok(None);
    };
    let class = class(tree, dimensions, &class_name)?;
    Ok(class.property(name).map(|property| {
        classes::object_call(
            ObjectOp::Load(property.field),
            vec![Expr::Ident(THIS.into())],
        )
    }))
}

/// The method that native callee expression `id` names: through a handle
/// (`h.m`), `this.m`, or unqualified inside a method. Returns the method's
/// subroutine, its number of formals, and the receiver to pass as `this`,
/// `None` for the enclosing method's own `this`.
pub(in crate::ast) fn method(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(String, usize, Option<ExprId>)>> {
    let (class_name, method_name, receiver) = match &native.expression(ungroup(id, native)).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." => match native_class(tree, *expression, native, dimensions) {
            Some(class) => (class, name, Some(*expression)),
            None => return Ok(None),
        },
        ExprKind::Name(name) if dimensions.get(THIS).is_some() => {
            let normalized = instances::normalize_identifier(&name.spelling);
            if dimensions.get(&normalized).is_some() {
                return Ok(None);
            }
            match scope_class(dimensions) {
                Some(class) => (class, name, None),
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let class = class(tree, dimensions, &class_name)?;
    let Some(method) = class.method(&instances::normalize_identifier(&method_name.spelling)) else {
        return Ok(None);
    };
    if method.qualifiers.is_static || method.qualifiers.is_virtual {
        return Err(unsupported(format!(
            "{} method `{}`",
            if method.qualifiers.is_static {
                "static"
            } else {
                "virtual"
            },
            method.name
        )));
    }
    let formals = native
        .subroutine(method.native)
        .ports
        .as_ref()
        .map_or(0, Vec::len);
    Ok(Some((
        Class::subroutine_name(&method.owner, &method.name),
        formals,
        receiver,
    )))
}

/// The receiver of an unqualified method call inside a method.
pub(in crate::ast) fn this() -> Expr {
    Expr::Ident(THIS.into())
}

/// The constructor call that `new` or `new(args)` in native expression
/// `id` makes for an object of class `class_name`, if `id` is one.
pub(in crate::ast) fn constructor(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    class_name: &str,
) -> Converted<Option<Expr>> {
    let arguments: &[celox_sv_syntax::Argument] = match &native.expression(ungroup(id, native)).kind
    {
        ExprKind::Name(name) if name.spelling == "new" => &[],
        ExprKind::Call {
            expression,
            arguments,
        } if matches!(&native.expression(ungroup(*expression, native)).kind,
            ExprKind::Name(name) if name.spelling == "new") =>
        {
            arguments
        }
        _ => return Ok(None),
    };
    let class = class(tree, dimensions, class_name)?;
    let formals = class.method("new").map_or(0, |method| {
        native
            .subroutine(method.native)
            .ports
            .as_ref()
            .map_or(0, Vec::len)
    });
    let mut args = arguments
        .iter()
        .map(|argument| {
            if argument.name.is_some() {
                return Err(unsupported("named constructor argument"));
            }
            argument
                .value
                .map(|value| bind_value(value, native, tree, dimensions, None))
                .transpose()
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args.len() > formals {
        return Err(unsupported(format!(
            "constructor of class `{class_name}` with {} arguments",
            args.len()
        )));
    }
    args.resize(formals, None);
    Ok(Some(Expr::Call {
        callee: Callee::Subroutine(Class::subroutine_name(class_name, "new")),
        args,
        arg_order: Vec::new(),
    }))
}
