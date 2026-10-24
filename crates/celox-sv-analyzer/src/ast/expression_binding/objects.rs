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
    if matches!(method_name.as_str(), "randomize" | "srandom") {
        if receiver == Receiver::Static
            || (receiver == Receiver::This && dimensions.get(THIS).is_none())
        {
            return Err(unsupported(
                "class randomization requires an object receiver",
            ));
        }
        if method_name == "randomize" {
            class.check_randomize(tree)?;
        }
        return Ok(Some((
            Class::subroutine_name(&class_name, &method_name),
            1,
            receiver,
        )));
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

/// Check that the handle native expression `id` evaluates to may be
/// assigned to a variable of class `target`: its class derives from or
/// implements `target`, or it is `null` or of a class not known before
/// binding (IEEE 1800-2023 8.16, 8.26.5).
pub(in crate::ast) fn check_assignment(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    target: &str,
    context: impl FnOnce() -> String,
) -> Converted<()> {
    let Some(source) = native_class(tree, id, native, dimensions) else {
        return Ok(());
    };
    if derives_from(tree, dimensions, &source, target)? {
        return Ok(());
    }
    Err(AnalyzerError::IncompatibleClassHandle {
        context: context(),
        actual: source,
        target: target.to_owned(),
    })
}

/// Whether class `class_name` derives from or implements `ancestor`.
fn derives_from(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    class_name: &str,
    ancestor: &str,
) -> Converted<bool> {
    Ok(class(tree, dimensions, class_name)?.derives_from(
        tree,
        ancestor,
        &dimensions.const_env,
        &dimensions.type_aliases,
    ))
}

/// A `$cast(destination, source)` whose destination is a class handle
/// (IEEE 1800-2023 8.16).
pub(in crate::ast) enum Cast {
    /// A call of the destination class's [`Class::cast_name`] subroutine
    /// with the destination and the source.
    Call {
        name: String,
        destination: ExprId,
        source: ExprId,
    },
    /// The source's class is neither assignment nor cast compatible with
    /// the destination's, so the cast fails whatever the source's value.
    Fails,
}

/// The `$cast` with `arguments` when its destination is a variable or an
/// array element of a class handle type.
pub(in crate::ast) fn cast(
    tree: &AnalysisContext<'_>,
    arguments: &[celox_sv_syntax::Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Cast>> {
    let [destination, source] = arguments else {
        return Ok(None);
    };
    let (Some(destination), Some(source), None, None) = (
        destination.value,
        source.value,
        &destination.name,
        &source.name,
    ) else {
        return Ok(None);
    };
    let Some(target) = native_class(tree, destination, native, dimensions) else {
        return Ok(None);
    };
    if property(tree, destination, native, dimensions)?.is_some() {
        return Err(unsupported("`$cast` to a class property"));
    }
    class(tree, dimensions, &target)?;
    // The source's class may be a base of the destination's, or an interface
    // class, whose object the call checks; any other class's cannot be
    // assigned (8.16).
    if let Some(source_class) = native_class(tree, source, native, dimensions)
        && !derives_from(tree, dimensions, &source_class, &target)?
        && !derives_from(tree, dimensions, &target, &source_class)?
        && !class(tree, dimensions, &source_class)?.is_interface
    {
        return Ok(Some(Cast::Fails));
    }
    Ok(Some(Cast::Call {
        name: Class::cast_name(&target),
        destination,
        source,
    }))
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
                Some(class) => {
                    if !derives_from(tree, dimensions, &class, class_name)? {
                        return Err(AnalyzerError::IncompatibleClassHandle {
                            context: format!("`{class}::new`"),
                            actual: class,
                            target: class_name.to_owned(),
                        });
                    }
                    class
                }
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

/// Inline randomization names are resolved in the receiver's class, not in
/// the caller's scope (18.11). Bit zero distinguishes an explicit empty set.
pub(super) fn random_selection(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    name: &str,
    arguments: &[celox_sv_syntax::Argument],
    native: &Declarations,
) -> Converted<Expr> {
    let owner = name.strip_suffix("::randomize").expect("randomize method");
    let class = class(tree, dimensions, owner)?;
    let mut bits = vec!['0'; class.properties.len() + 1];
    if !arguments.is_empty() {
        bits[0] = '1';
    }
    for argument in arguments {
        let id = argument
            .value
            .filter(|_| argument.name.is_none())
            .ok_or_else(|| unsupported("randomize arguments must be member names"))?;
        match &native.expression(ungroup(id, native)).kind {
            ExprKind::Literal(value) if value == "null" && arguments.len() == 1 => {}
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                let index = class
                    .properties
                    .iter()
                    .rposition(|p| p.name == name)
                    .ok_or_else(|| {
                        unsupported(format!(
                            "randomize argument `{name}` is not an instance property"
                        ))
                    })?;
                let property = &class.properties[index];
                check_random_access(tree, dimensions, property)?;
                if property.constant {
                    return Err(unsupported("randomize argument is a const property"));
                }
                if property.field.handle.is_some()
                    || (property.r#type.is_enum && property.r#type.enum_values.is_none())
                    || property.randc
                {
                    return Err(unsupported(
                        "randomize arguments require integral, non-randc properties with a supported enum domain",
                    ));
                }
                bits[index + 1] = '1';
            }
            _ => {
                return Err(unsupported(
                    "randomize arguments must be member names or a single null",
                ));
            }
        }
    }
    Ok(Expr::Literal(format!(
        "{}'b{}",
        bits.len(),
        bits.into_iter().rev().collect::<String>()
    )))
}

/// Bind rand_mode's void setter and integer getter separately (18.8).
/// A getter requires one random property; only the setter accepts an object.
pub(in crate::ast) fn rand_mode_call(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    allow_set: bool,
) -> Converted<Option<Expr>> {
    let (callee, args) = match &native.expression(ungroup(id, native)).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (id, &[][..]),
    };
    let (property, owner, handle) = match &native.expression(ungroup(callee, native)).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." && name.spelling == "rand_mode" => {
            let property_target = property(tree, *expression, native, dimensions)?;
            if let Some((property, object)) = property_target.filter(|(property, _)| {
                property.rand || property.randc || property.r#type.class().is_none()
            }) {
                check_random_access(tree, dimensions, &property)?;
                if !property.rand && !property.randc {
                    return Err(unsupported("rand_mode requires a rand or randc property"));
                }
                (
                    Some(property),
                    None,
                    receiver(tree, object, native, dimensions)?,
                )
            } else if let Some(owner) = native_class(tree, *expression, native, dimensions) {
                (
                    None,
                    Some(owner),
                    bind_value(*expression, native, tree, dimensions, None)?,
                )
            } else {
                return Err(unsupported(
                    "rand_mode requires an object or random class property",
                ));
            }
        }
        ExprKind::Name(name)
            if name.spelling == "rand_mode" && dimensions.get("rand_mode").is_none() =>
        {
            let Some(owner) = scope_class(dimensions) else {
                return Ok(None);
            };
            if dimensions.get(THIS).is_none() {
                return Err(unsupported("rand_mode requires an object receiver"));
            }
            (None, Some(owner), this())
        }
        _ => return Ok(None),
    };
    if args.is_empty() {
        let property =
            property.ok_or_else(|| unsupported("rand_mode getter requires one random property"))?;
        return Ok(Some(classes::object_call(
            ObjectOp::ModeGet(property.field.offset, true),
            vec![handle],
        )));
    }
    if !allow_set {
        return Err(unsupported(
            "rand_mode setter is void and cannot be used as a value",
        ));
    }
    if args.len() != 1 || args[0].name.is_some() || args[0].value.is_none() {
        return Err(unsupported("rand_mode setter requires one on_off argument"));
    }
    let enabled = bind_value(args[0].value.unwrap(), native, tree, dimensions, None)?;
    Ok(Some(match property {
        Some(property) => classes::object_call(
            ObjectOp::ModeSet(property.field.offset),
            vec![handle, enabled],
        ),
        None => Expr::Call {
            callee: Callee::Subroutine(Class::subroutine_name(&owner.unwrap(), "rand_mode")),
            args: vec![Some(handle), Some(enabled)],
            arg_order: Vec::new(),
        },
    }))
}

fn check_random_access(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    property: &classes::Property,
) -> Converted<()> {
    let scope = scope_class(dimensions);
    let accessible = if property.local {
        scope.as_deref() == Some(&property.owner)
    } else if property.protected {
        scope
            .and_then(|name| class(tree, dimensions, &name).ok())
            .is_some_and(|class| {
                class.derives_from(
                    tree,
                    &property.owner,
                    &dimensions.const_env,
                    &dimensions.type_aliases,
                )
            })
    } else {
        true
    };
    if !accessible {
        return Err(unsupported(format!(
            "randomization access to non-public property `{}`",
            property.name
        )));
    }
    Ok(())
}
