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
/// before it is bound: a handle variable, an element of an array of
/// handles, or a handle property.
pub(in crate::ast) fn native_class(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<String> {
    let expression = native.expression(ungroup(id, native));
    if let ExprKind::Select {
        expression,
        dimension,
    } = &expression.kind
        && matches!(dimension.kind, celox_sv_syntax::DimensionKind::Index(_))
        && let ExprKind::Name(name) = &native.expression(ungroup(*expression, native)).kind
    {
        let ty = dimensions.get(&instances::normalize_identifier(&name.spelling))?;
        return ty.class.clone().filter(|_| !ty.unpacked.is_empty());
    }
    if let ExprKind::Name(name) = &expression.kind {
        let name = instances::normalize_identifier(&name.spelling);
        if let Some(ty) = dimensions.get(&name) {
            return ty.class.clone();
        }
        if name == "super" {
            return super_class(tree, dimensions);
        }
    }
    let (property, _) = property(tree, id, native, dimensions).ok()??;
    property.r#type.class().map(str::to_owned)
}

/// The class whose method is being bound, whose properties and methods
/// unqualified names may refer to.
pub(in crate::ast) fn scope_class(dimensions: &PackedDimensions) -> Option<String> {
    dimensions.class_scope.clone()
}

/// The base of the scope class, which `super` refers to inside a method that
/// has an object (IEEE 1800-2023 8.15).
fn super_class(tree: &AnalysisContext<'_>, dimensions: &PackedDimensions) -> Option<String> {
    dimensions.get(THIS)?;
    class(tree, dimensions, &scope_class(dimensions)?)
        .ok()?
        .base
        .clone()
}

/// Whether native expression `id` is `super`, which passes the enclosing
/// method's `this`.
fn is_super(id: ExprId, native: &Declarations) -> bool {
    matches!(&native.expression(ungroup(id, native)).kind,
        ExprKind::Name(name) if name.spelling == "super")
}

/// The class that native expression `id` names, as the scope of `C::name`.
fn named_class(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Option<String> {
    let ExprKind::Name(name) = &native.expression(ungroup(id, native)).kind else {
        return None;
    };
    let name = instances::normalize_identifier(&name.spelling);
    (dimensions.get(&name).is_none() && classes::handle_type(tree, &name).is_some()).then_some(name)
}

/// The variable of the static property that native expression `id` names:
/// `h.p` or `C::p` (IEEE 1800-2023 8.9, 8.23). An unqualified name inside a
/// method of the class is renamed to it where the method's scope begins.
pub(in crate::ast) fn static_property(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<String>> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(ungroup(id, native)).kind
    else {
        return Ok(None);
    };
    let class_name = match separator.as_str() {
        "." => native_class(tree, *expression, native, dimensions),
        "::" => named_class(tree, *expression, native, dimensions),
        _ => None,
    };
    let Some(class_name) = class_name else {
        return Ok(None);
    };
    Ok(class(tree, dimensions, &class_name)?
        .static_property(&instances::normalize_identifier(&name.spelling))
        .map(|property| property.variable.clone()))
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
            Some(class) => (
                class,
                name,
                (!is_super(*expression, native)).then_some(*expression),
            ),
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
    let Some(property) = class.property(&instances::normalize_identifier(&name.spelling)) else {
        return Ok(None);
    };
    if receiver.is_none() && dimensions.get(THIS).is_none() {
        return Err(unsupported(format!(
            "non-static property `{}` in a static method",
            property.name
        )));
    }
    Ok(Some((property.clone(), receiver)))
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

/// A read of property `property` of the object `handle`.
pub(in crate::ast) fn load(property: &classes::Property, handle: Expr) -> Expr {
    classes::object_call(ObjectOp::Load(property.field), vec![handle])
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
    let Some(property) = class.property(name) else {
        return Ok(None);
    };
    if dimensions.get(THIS).is_none() {
        return Err(unsupported(format!(
            "non-static property `{name}` in a static method"
        )));
    }
    Ok(Some(load(property, this())))
}

/// The object a method is called on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum Receiver {
    /// The object of a handle expression.
    Handle(ExprId),
    /// The object of the enclosing method.
    This,
    /// None: a static method (IEEE 1800-2023 8.10).
    Static,
}

/// The method that native callee expression `id` names: through a handle
/// (`h.m`), `this.m`, `super.m`, `C::m`, or unqualified inside a method.
/// Returns the method's subroutine, its number of formals, and the object
/// it is called on. A virtual method is called through its dispatcher,
/// except as `super.m`, which names the base's implementation (IEEE
/// 1800-2023 8.15, 8.20).
pub(in crate::ast) fn method(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(String, usize, Receiver)>> {
    let mut through_super = false;
    let (class_name, method_name, receiver) = match &native.expression(ungroup(id, native)).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." => match native_class(tree, *expression, native, dimensions) {
            Some(class) if is_super(*expression, native) => {
                through_super = true;
                (class, name, Receiver::This)
            }
            Some(class) => (class, name, Receiver::Handle(*expression)),
            None => return Ok(None),
        },
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => match named_class(tree, *expression, native, dimensions) {
            Some(class) => (class, name, Receiver::Static),
            None => return Ok(None),
        },
        ExprKind::Name(name) => {
            let normalized = instances::normalize_identifier(&name.spelling);
            if dimensions.get(&normalized).is_some() {
                return Ok(None);
            }
            match scope_class(dimensions) {
                Some(class) => (class, name, Receiver::This),
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let class = class(tree, dimensions, &class_name)?;
    let method_name = instances::normalize_identifier(&method_name.spelling);
    if method_name == "new" {
        return match receiver {
            // `C::new` is a constructor call, see [`constructor`].
            Receiver::Static => Ok(None),
            _ => Err(unsupported(
                "`super.new` that is not the first statement of a constructor",
            )),
        };
    }
    let Some(method) = class.method(&method_name) else {
        return Ok(None);
    };
    if through_super && method.is_pure() {
        return Err(unsupported(format!(
            "pure virtual method `{}` called through `super`",
            method.name
        )));
    }
    let receiver = if method.qualifiers.is_static {
        Receiver::Static
    } else {
        match receiver {
            Receiver::This if dimensions.get(THIS).is_none() => {
                return Err(unsupported(format!(
                    "non-static method `{}` called from a static method",
                    method.name
                )));
            }
            Receiver::Static => {
                return Err(unsupported(format!(
                    "non-static method `{}` called through its class scope",
                    method.name
                )));
            }
            receiver => receiver,
        }
    };
    let formals = native
        .subroutine(method.native)
        .ports
        .as_ref()
        .map_or(0, Vec::len);
    let subroutine = if method.is_virtual && !through_super && receiver != Receiver::Static {
        Class::dispatcher_name(&method.owner, &method.name)
    } else {
        Class::subroutine_name(&method.owner, &method.name)
    };
    Ok(Some((subroutine, formals, receiver)))
}

/// The receiver of an unqualified method call inside a method.
pub(in crate::ast) fn this() -> Expr {
    Expr::Ident(THIS.into())
}

/// The constructor call that native expression `id` makes when it is `new`,
/// `new(args)`, `C::new` or `C::new(args)`, the plain forms creating an
/// object of class `class_name` (IEEE 1800-2023 8.7, 8.8). The call
/// allocates the object and passes it to the constructor, which initializes
/// it and returns it.
pub(in crate::ast) fn constructor(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    class_name: &str,
) -> Converted<Option<Expr>> {
    let (callee, arguments): (ExprId, &[celox_sv_syntax::Argument]) =
        match &native.expression(ungroup(id, native)).kind {
            ExprKind::Call {
                expression,
                arguments,
            } => (*expression, arguments),
            _ => (id, &[]),
        };
    let class_name = match &native.expression(ungroup(callee, native)).kind {
        ExprKind::Name(name) if name.spelling == "new" => class_name.to_owned(),
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" && name.spelling == "new" => {
            match named_class(tree, *expression, native, dimensions) {
                Some(class) => class,
                None => return Ok(None),
            }
        }
        _ => return Ok(None),
    };
    let class = class(tree, dimensions, &class_name)?;
    if class.is_abstract {
        return Err(unsupported(format!(
            "object of abstract class `{class_name}`"
        )));
    }
    let formals = class.constructor_formals(native);
    let mut args = vec![Some(class.allocation())];
    for argument in arguments {
        if argument.name.is_some() {
            return Err(unsupported("named constructor argument"));
        }
        args.push(
            argument
                .value
                .map(|value| bind_value(value, native, tree, dimensions, None))
                .transpose()?,
        );
    }
    if args.len() > formals + 1 {
        return Err(unsupported(format!(
            "constructor of class `{class_name}` with {} arguments",
            args.len() - 1
        )));
    }
    args.resize(formals + 1, None);
    Ok(Some(Expr::Call {
        callee: Callee::Subroutine(Class::subroutine_name(&class_name, "new")),
        args,
        arg_order: Vec::new(),
    }))
}
