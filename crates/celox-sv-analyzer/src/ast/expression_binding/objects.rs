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
    let id = ungroup(id, native);
    if let ExprKind::Call { expression, .. } = &native.expression(id).kind
        && let ExprKind::Member {
            expression: base,
            separator,
            name,
        } = &native.expression(*expression).kind
        && separator == "::"
        && name.spelling == "self"
        && super::super::processes::scope(tree, *base, native, dimensions)
    {
        return Some(super::super::processes::CLASS.to_owned());
    }
    // A class handle member of an unpacked structure variable.
    if let Some(name) = unpacked_structs::native_member(ungroup(id, native), native, dimensions) {
        return dimensions.get(&name)?.class.clone();
    }
    // A class handle member of a container element of unpacked structure type.
    if let Ok(Some((ty, _))) = containers::element_member(id, native, tree, dimensions) {
        return ty.class().map(str::to_owned);
    }
    let expression = native.expression(ungroup(id, native));
    if let ExprKind::Select {
        expression,
        dimension,
    } = &expression.kind
        && matches!(dimension.kind, celox_sv_syntax::DimensionKind::Index(_))
    {
        if let Some(ty) = containers::native_type(*expression, native, tree, dimensions)
            .ok()
            .flatten()
        {
            return ty.element.class().map(str::to_owned);
        }
        let ExprKind::Name(name) = &native.expression(ungroup(*expression, native)).kind else {
            return None;
        };
        let ty = dimensions.get(&instances::normalize_identifier(&name.spelling))?;
        return ty
            .container
            .as_ref()
            .and_then(|ty| ty.element.class().map(str::to_owned))
            .or_else(|| ty.class.clone().filter(|_| !ty.unpacked.is_empty()));
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
    // The receiver of `h.p` is resolved once for both kinds of property, so
    // a chain `a.b.c` takes one walk.
    if let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &expression.kind
        && separator == "."
        // Otherwise a member of a property of unpacked structure type, found
        // as a property below.
        && let Some(owner) = native_class(tree, *receiver, native, dimensions)
    {
        let member = instances::normalize_identifier(&name.spelling);
        let receiver = (!is_super(*receiver, native)).then_some(*receiver);
        if let Some((property, _)) =
            property_in(tree, dimensions, &owner, &member, receiver).ok()?
        {
            return property.r#type.class().map(str::to_owned);
        }
        return class(tree, dimensions, &owner)
            .ok()?
            .static_property(&member)
            .and_then(|property| property.r#type.class().map(str::to_owned));
    }
    if let Some(class) = static_property_class(tree, id, native, dimensions).ok()? {
        return Some(class);
    }
    let (property, _) = property(tree, id, native, dimensions).ok()??;
    property.r#type.class().map(str::to_owned)
}

/// Access depends on the lexical class, not the class of the receiver (8.18).
pub(in crate::ast) fn check_access(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    owner: &str,
    name: &str,
    local: bool,
    protected: bool,
) -> Converted<()> {
    if !local && !protected {
        return Ok(());
    }
    if let Some(scope) = dimensions.class_scope.as_deref() {
        if scope == owner
            || (!local
                && class(tree, dimensions, scope)?.derives_from(
                    tree,
                    owner,
                    &dimensions.const_env,
                    &dimensions.type_aliases,
                ))
        {
            return Ok(());
        }
    }
    Err(AnalyzerError::InaccessibleClassMember {
        owner: owner.to_owned(),
        name: name.to_owned(),
        visibility: if local { "local" } else { "protected" },
    })
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

/// The class that native expression `id` names, as the scope of `C::name`:
/// a class, or the specialization that a parameter value assignment or the
/// defaults of a parameterized class make (IEEE 1800-2023 8.25.1).
pub(in crate::ast) fn named_class(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<String>> {
    let (name, arguments) = match &native.expression(ungroup(id, native)).kind {
        // A class of a package, `p::C` (IEEE 1800-2023 26.3).
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => {
            let ExprKind::Name(package) = &native.expression(*expression).kind else {
                return Ok(None);
            };
            let scope_name = instances::normalize_identifier(&package.spelling);
            let member_name = instances::normalize_identifier(&name.spelling);
            if let Some(scope_class) = dimensions
                .type_aliases
                .get(&scope_name)
                .and_then(|ty| ty.class().map(str::to_owned))
                .or_else(|| classes::resolve(tree, &scope_name))
                && let Some(ty) =
                    classes::specializations::member_type(tree, &scope_class, &member_name)
                && let Some(class) = ty.class()
            {
                return Ok(Some(class.to_owned()));
            }
            let class = scope::qualified_name(&tree.state, &scope_name, &member_name);
            // A typedef of the package may name a class or a specialization.
            if let Some(aliased) = dimensions
                .type_aliases
                .get(&class)
                .and_then(|ty| ty.class().map(str::to_owned))
            {
                return Ok(Some(aliased));
            }
            if classes::handle_type(tree, &class).is_some() {
                return Ok(Some(class));
            }
            // Inside its package, `p::C` is a name of the package itself,
            // whose classes are declared under their qualified names.
            return Ok(classes::resolve(tree, &class)
                .filter(|class| classes::handle_type(tree, class).is_some()));
        }
        ExprKind::Name(name) => (instances::normalize_identifier(&name.spelling), None),
        ExprKind::Parameterized {
            expression,
            arguments,
        } => match &native.expression(*expression).kind {
            ExprKind::Name(name) => (
                instances::normalize_identifier(&name.spelling),
                Some(&arguments[..]),
            ),
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "::" => {
                let ExprKind::Name(package) = &native.expression(*expression).kind else {
                    return Ok(None);
                };
                (
                    format!(
                        "{}::{}",
                        instances::normalize_identifier(&package.spelling),
                        instances::normalize_identifier(&name.spelling)
                    ),
                    Some(&arguments[..]),
                )
            }
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    if dimensions.get(&name).is_some() {
        return Ok(None);
    }
    // Inside a parameterized class and its out-of-block methods, its name
    // alone refers to the specialization.
    if arguments.is_none()
        && let Some(class) = dimensions
            .type_aliases
            .get(&classes::specializations::scope_key(&name))
            .or_else(|| {
                // A class of a package is declared under its qualified name.
                let class = classes::resolve(tree, &name)?;
                dimensions
                    .type_aliases
                    .get(&classes::specializations::scope_key(&class))
            })
            .or_else(|| dimensions.type_aliases.get(&name))
            .and_then(|ty| ty.class().map(str::to_owned))
    {
        return Ok(Some(class));
    }
    let Some(name) = classes::resolve(tree, &name) else {
        return Ok(None);
    };
    if !classes::is_parameterized(tree, &name) {
        if arguments.is_some_and(|arguments| !arguments.is_empty()) {
            return Err(unsupported(format!(
                "parameter value assignment of class `{name}`, which has no parameters"
            )));
        }
        return Ok(Some(name));
    }
    // Elsewhere a parameterized class needs a specialization, `C#()` for
    // the default one.
    let Some(arguments) = arguments else {
        return Err(AnalyzerError::UnspecializedClassScope { class: name });
    };
    classes::specializations::specialize(tree, &name, arguments, dimensions).map(Some)
}

/// The value of the class parameter that native expression `id` names,
/// `h.p` or `C::p` (IEEE 1800-2023 8.25), as a literal.
pub(in crate::ast) fn class_parameter(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Expr>> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(ungroup(id, native)).kind
    else {
        return Ok(None);
    };
    let class = match separator.as_str() {
        "." => native_class(tree, *expression, native, dimensions),
        "::" => named_class(tree, *expression, native, dimensions)?,
        _ => None,
    };
    Ok(class.and_then(|class| {
        classes::specializations::parameter(
            tree,
            &class,
            &instances::normalize_identifier(&name.spelling),
        )
    }))
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
        "::" => named_class(tree, *expression, native, dimensions)?,
        _ => None,
    };
    let Some(class_name) = class_name else {
        return Ok(None);
    };
    if class_name == super::super::processes::CLASS {
        return Ok(None);
    }
    let class = class(tree, dimensions, &class_name)?;
    let Some(property) = class.static_property(&instances::normalize_identifier(&name.spelling))
    else {
        return Ok(None);
    };
    check_access(
        tree,
        dimensions,
        &property.owner,
        &property.name,
        property.local,
        property.protected,
    )?;
    Ok(Some(property.variable.clone()))
}

/// The class of the handle that the static property native expression `id`
/// names holds, `h.p` or `C::p`.
fn static_property_class(
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
        "::" => named_class(tree, *expression, native, dimensions)?,
        _ => None,
    };
    let Some(class_name) = class_name else {
        return Ok(None);
    };
    Ok(class(tree, dimensions, &class_name)?
        .static_property(&instances::normalize_identifier(&name.spelling))
        .and_then(|property| property.r#type.class().map(str::to_owned)))
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
    let Some((class, name, receiver)) = member_chain(tree, id, native, dimensions)? else {
        return Ok(None);
    };
    property_of(tree, dimensions, &class, &name, receiver)
}

/// The instance property `name` of class `class_name`, read through
/// `receiver` as [`property`] returns it.
fn property_in(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    class_name: &str,
    name: &str,
    receiver: Option<ExprId>,
) -> Converted<Option<(classes::Property, Option<ExprId>)>> {
    if class_name == super::super::processes::CLASS {
        return Ok(None);
    }
    let class = class(tree, dimensions, class_name)?;
    property_of(tree, dimensions, &class, name, receiver)
}

/// The instance property `name` of `class`, read through `receiver` as
/// [`property`] returns it.
fn property_of(
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    class: &Class,
    name: &str,
    receiver: Option<ExprId>,
) -> Converted<Option<(classes::Property, Option<ExprId>)>> {
    let Some(property) = class.property(name) else {
        if let Some(property) = class.static_property(name) {
            check_access(
                tree,
                dimensions,
                &property.owner,
                &property.name,
                property.local,
                property.protected,
            )?;
        }
        return Ok(None);
    };
    check_access(
        tree,
        dimensions,
        &property.owner,
        &property.name,
        property.local,
        property.protected,
    )?;
    if receiver.is_none() && dimensions.get(THIS).is_none() {
        return Err(unsupported(format!(
            "non-static property `{}` in a static method",
            property.name
        )));
    }
    Ok(Some((property.clone(), receiver)))
}

/// The property of unpacked structure type, or member of one, that native
/// expression `id` names, with the receiver as for [`property`].
pub(in crate::ast) fn aggregate_property(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(classes::AggregateProperty, Rc<Class>, Option<ExprId>)>> {
    let Some((class, name, receiver)) = member_chain(tree, id, native, dimensions)? else {
        return Ok(None);
    };
    let Some(aggregate) = class.aggregate(&name).cloned() else {
        return Ok(None);
    };
    check_access(
        tree,
        dimensions,
        &aggregate.owner,
        &aggregate.name,
        aggregate.local,
        aggregate.protected,
    )?;
    if receiver.is_none() && dimensions.get(THIS).is_none() {
        return Err(unsupported(format!(
            "non-static property `{}` in a static method",
            aggregate.name
        )));
    }
    Ok(Some((aggregate, class, receiver)))
}

/// The flat value of property `aggregate` of `class` in the object
/// `handle`: the concatenation of its leaf properties.
pub(in crate::ast) fn aggregate_load(
    aggregate: &classes::AggregateProperty,
    class: &Class,
    handle: &Expr,
) -> Converted<Expr> {
    let leaves = aggregate
        .r#type
        .unpacked_struct()
        .expect("an unpacked structure property")
        .leaves();
    // Each member is read through the receiver, so it must not change: a
    // subroutine call, pop or `$urandom` would be made once per member.
    if leaves.len() > 1 && inlining::effectful(handle) {
        return Err(unsupported(
            "whole unpacked structure property read through a receiver with a side effect",
        ));
    }
    Ok(Expr::Concat(
        leaves
            .iter()
            .map(|leaf| {
                let property = class
                    .property(&leaf.name(&aggregate.name))
                    .ok_or_else(|| unsupported("unpacked structure property member"))?;
                Ok(load(property, handle.clone()))
            })
            .collect::<Converted<Vec<_>>>()?,
    ))
}

/// The class, property path and receiver of `h.p`, `h.p.m`, or an
/// unqualified `p`/`p.m` inside a method of its class. A path names a
/// member of a property of unpacked structure type.
fn member_chain(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(Rc<Class>, String, Option<ExprId>)>> {
    let mut names = Vec::new();
    let mut current = ungroup(id, native);
    let (class_name, receiver) = loop {
        match &native.expression(current).kind {
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." => {
                names.push(instances::normalize_identifier(&name.spelling));
                if let Some(class) = native_class(tree, *expression, native, dimensions) {
                    break (
                        class,
                        (!is_super(*expression, native)).then_some(*expression),
                    );
                }
                current = ungroup(*expression, native);
            }
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                if dimensions.get(&name).is_some() {
                    return Ok(None);
                }
                names.push(name);
                match scope_class(dimensions) {
                    Some(class) => break (class, None),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        }
    };
    names.reverse();
    // A process handle has no properties (IEEE 1800-2023 9.7).
    if class_name == super::super::processes::CLASS {
        return Ok(None);
    }
    Ok(Some((
        class(tree, dimensions, &class_name)?,
        names.join("."),
        receiver,
    )))
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
    if class_name == super::super::processes::CLASS {
        return Ok(None);
    }
    let class = class(tree, dimensions, &class_name)?;
    let Some(property) = class.property(name) else {
        if let Some(property) = class.static_property(name) {
            check_access(
                tree,
                dimensions,
                &property.owner,
                &property.name,
                property.local,
                property.protected,
            )?;
        }
        return Ok(None);
    };
    check_access(
        tree,
        dimensions,
        &property.owner,
        &property.name,
        property.local,
        property.protected,
    )?;
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
/// Returns the method's subroutine, the names of its formals, and the object
/// it is called on. A virtual method is called through its dispatcher,
/// except as `super.m`, which names the base's implementation (IEEE
/// 1800-2023 8.15, 8.20).
pub(in crate::ast) fn method(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(String, Vec<Option<String>>, Receiver)>> {
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
        } if separator == "::" => match named_class(tree, *expression, native, dimensions)? {
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
    if class_name == super::super::processes::CLASS {
        return Ok(None);
    }
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
            class.check_randomize()?;
        }
        return Ok(Some((
            Class::subroutine_name(&class_name, &method_name),
            vec![None],
            receiver,
        )));
    }
    let Some(method) = class.method(&method_name) else {
        return Ok(None);
    };
    check_access(
        tree,
        dimensions,
        &method.owner,
        &method.name,
        method.qualifiers.local,
        method.qualifiers.protected,
    )?;
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
    let formals = method.formals.clone();
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
    if class_name == ancestor {
        return Ok(true);
    }
    if class_name == super::super::processes::CLASS || ancestor == super::super::processes::CLASS {
        return Ok(false);
    }
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
    // `new h` copies the properties of the object of `h` into a new object
    // of its class; handles are copied, not the objects they refer to (IEEE
    // 1800-2023 8.12).
    if let ExprKind::Copy(source) = &native.expression(ungroup(id, native)).kind {
        if class_name == super::super::processes::CLASS {
            return Err(unsupported("constructing a process with new"));
        }

        let source_class = match native_class(tree, *source, native, dimensions) {
            Some(class) => Some(class),
            None => static_property_class(tree, *source, native, dimensions)?,
        };
        let Some(source_class) = source_class else {
            return Err(unsupported(
                "shallow copy of a value that is not a class handle",
            ));
        };
        if !derives_from(tree, dimensions, &source_class, class_name)? {
            return Err(AnalyzerError::IncompatibleClassHandle {
                context: "shallow copy".to_owned(),
                actual: source_class,
                target: class_name.to_owned(),
            });
        }
        let handle = bind_value(*source, native, tree, dimensions, None)?;
        return Ok(Some(classes::object_call(ObjectOp::Copy, vec![handle])));
    }
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
            match named_class(tree, *expression, native, dimensions)? {
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
    if class_name == super::super::processes::CLASS {
        return Err(unsupported("constructing a process with new"));
    }
    let class = class(tree, dimensions, &class_name)?;
    if let Some(method) = class.method("new") {
        check_access(
            tree,
            dimensions,
            &method.owner,
            &method.name,
            method.qualifiers.local,
            method.qualifiers.protected,
        )?;
    }
    if class.is_abstract {
        return Err(unsupported(format!(
            "object of abstract class `{class_name}`"
        )));
    }
    let mut args = vec![Some(class.allocation())];
    let (actuals, order) = class.constructor_arguments(arguments)?;
    for actual in actuals {
        args.push(
            actual
                .map(|value| bind_value(value, native, tree, dimensions, None))
                .transpose()?,
        );
    }
    Ok(Some(Expr::Call {
        callee: Callee::Subroutine(Class::subroutine_name(&class_name, "new")),
        args,
        // The allocation is passed first; the actuals are evaluated in the
        // order they are written (13.5.4).
        arg_order: std::iter::once(0)
            .chain(order.into_iter().map(|index| index + 1))
            .collect(),
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
                // The constraint solver writes plain property bytes, not
                // the planes of a 4-state value (IEEE 1800-2023 18.4).
                if property.field.four_state {
                    return Err(unsupported("randomization of a 4-state class property"));
                }
                if property.field.handle.is_some()
                    || (property.r#type.is_enum && property.r#type.enum_domain.is_none())
                    || (property.randc && property.cyclic_domain_size().is_none())
                {
                    return Err(unsupported(
                        "randomize arguments require integral properties with a supported enum domain",
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

/// Constraint block modes are independent of member rand_mode and inline constraints (18.9).
pub(in crate::ast) fn constraint_mode_call(
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
    let (owner, block, handle) = match &native.expression(ungroup(callee, native)).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." && name.spelling == "constraint_mode" => {
            if let Some(owner) = native_class(tree, *expression, native, dimensions) {
                (
                    owner,
                    None,
                    receiver(tree, Some(*expression), native, dimensions)?,
                )
            } else {
                match &native.expression(ungroup(*expression, native)).kind {
                    ExprKind::Member {
                        expression: object,
                        separator,
                        name,
                    } if separator == "." => {
                        let Some(owner) = native_class(tree, *object, native, dimensions) else {
                            return Ok(None);
                        };
                        (
                            owner,
                            Some(name.spelling.clone()),
                            receiver(tree, Some(*object), native, dimensions)?,
                        )
                    }
                    ExprKind::Name(name)
                        if scope_class(dimensions).is_some()
                            && dimensions.get(THIS).is_some()
                            && dimensions.get(&name.spelling).is_none() =>
                    {
                        (
                            scope_class(dimensions).unwrap(),
                            Some(name.spelling.clone()),
                            this(),
                        )
                    }
                    _ => return Ok(None),
                }
            }
        }
        ExprKind::Name(name)
            if name.spelling == "constraint_mode"
                && dimensions.get("constraint_mode").is_none() =>
        {
            let Some(owner) = scope_class(dimensions) else {
                return Ok(None);
            };
            if dimensions.get(THIS).is_none() {
                return Err(unsupported("constraint_mode requires an object receiver"));
            }
            (owner, None, this())
        }
        _ => return Ok(None),
    };
    let class = class(tree, dimensions, &owner)?;
    if class.constraints.iter().any(|(_, block)| {
        block
            .as_ref()
            .is_err_and(|reason| reason == "unsupported constraint block form")
    }) {
        return Err(unsupported(
            "constraint_mode for external or static constraint blocks",
        ));
    }
    let index = block
        .map(|name| {
            class
                .constraints
                .iter()
                .position(|(block, _)| *block == name)
                .map(|index| index as u32)
                .ok_or_else(|| unsupported(format!("unknown constraint block {name}")))
        })
        .transpose()?;
    if args.is_empty() {
        let index = index
            .ok_or_else(|| unsupported("constraint_mode getter requires one named constraint"))?;
        return Ok(Some(classes::object_call(
            ObjectOp::ConstraintModeGet(index),
            vec![handle],
        )));
    }
    if !allow_set {
        return Err(unsupported(
            "constraint_mode setter is void and cannot be used as a value",
        ));
    }
    if args.len() != 1 || args[0].name.is_some() || args[0].value.is_none() {
        return Err(unsupported(
            "constraint_mode setter requires one on_off argument",
        ));
    }
    let enabled = bind_value(args[0].value.unwrap(), native, tree, dimensions, None)?;
    Ok(Some(classes::object_call(
        ObjectOp::ConstraintModeSet(index),
        vec![handle, enabled],
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
    check_access(
        tree,
        dimensions,
        &property.owner,
        &property.name,
        property.local,
        property.protected,
    )
}

pub(super) fn randomize_with(
    tree: &AnalysisContext<'_>,
    expression: ExprId,
    constraints: Option<&[ExprId]>,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let (callee, arguments) = match &native.expression(ungroup(expression, native)).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (expression, &[][..]),
    };
    let (method_name, _, receiver) = method(tree, callee, native, dimensions)?
        .filter(|(name, _, _)| name.ends_with("::randomize"))
        .ok_or_else(|| unsupported("inline constraints require class randomize"))?;
    let owner = method_name.strip_suffix("::randomize").unwrap();
    let class = class(tree, dimensions, owner)?;
    // Inline constraints need the solver even on otherwise unconstrained classes.
    class.constraint_program()?;
    let constraint_ids = constraints.ok_or_else(|| unsupported("inline constraint form"))?;
    let mut inputs = HashMap::default();
    let mut input_types = Vec::new();
    let mut input_values = Vec::new();
    let mut pending = constraint_ids.to_vec();
    while let Some(id) = pending.pop() {
        let candidate = match &native.expression(id).kind {
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                if class.properties.iter().any(|p| p.name == name) {
                    None
                } else if class.statics.iter().any(|p| p.name == name) {
                    return Err(unsupported("static properties in inline constraints"));
                } else {
                    Some((name.clone(), name))
                }
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "::"
                && matches!(&native.expression(*expression).kind, ExprKind::Name(root) if root.spelling == "local") =>
            {
                let name = instances::normalize_identifier(&name.spelling);
                Some((format!("local::{name}"), name))
            }
            ExprKind::Group(id) | ExprKind::Unary { expression: id, .. } => {
                pending.push(*id);
                None
            }
            ExprKind::Binary { left, right, .. } => {
                pending.extend([*right, *left]);
                None
            }
            ExprKind::Set(items) => {
                for item in items {
                    match item.kind {
                        celox_sv_syntax::DimensionKind::Index(id) => pending.push(id),
                        celox_sv_syntax::DimensionKind::Range { left, right } => {
                            pending.extend([right, left])
                        }
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        };
        let Some((key, name)) = candidate else {
            continue;
        };
        if inputs.contains_key(&key) {
            continue;
        }
        let variable = dimensions
            .get(&name)
            .ok_or_else(|| unsupported(format!("inline local variable `{name}`")))?;
        if variable.real
            || variable.string
            || variable.class.is_some()
            || variable.container.is_some()
            || !variable.unpacked.is_empty()
            || !variable.is_2state
        {
            return Err(unsupported(
                "inline inputs require two-state scalar variables",
            ));
        }
        let value = Expr::Ident(name);
        let width = expr_static_width(&tree.state, &value, dimensions)
            .filter(|width| (1..=64).contains(width))
            .ok_or_else(|| unsupported("inline input width"))?;
        let mut ty = classes::unsigned_type(width);
        ty.is_signed = variable.signed;
        inputs.insert(key, (input_types.len(), width as u32, variable.signed));
        input_types.push(ty);
        input_values.push(Some(value));
    }
    let constraints = constraint_ids
        .iter()
        .map(|&id| {
            classes::constraints::bind_inputs(
                id,
                tree,
                &class.properties,
                &dimensions.const_env,
                &inputs,
            )
            .map_err(|reason| unsupported(format!("inline constraints: {reason}")))
        })
        .collect::<Converted<Vec<_>>>()?;
    let selection = random_selection(tree, dimensions, &method_name, arguments, native)?;
    let handle = match receiver {
        Receiver::Handle(id) => bind_value(id, native, tree, dimensions, None)?,
        Receiver::This => this(),
        Receiver::Static => return Err(unsupported("inline randomize object receiver")),
    };
    let key = tree.state.next_call_site.get();
    tree.state.next_call_site.set(key + 1);
    let inline = classes::constraints::Inline {
        key,
        class,
        constraints,
        inputs: input_types,
        dimensions: dimensions.clone(),
    };
    let name = inline.name(owner);
    // Argument evaluation is left-to-right. Prepare the receiver and run its
    // pre-hook before sampling any caller-scope input. Evaluate the receiver once.
    let handle = if input_values.is_empty() {
        handle
    } else {
        Expr::Call {
            callee: Callee::Subroutine(inline.prepare_name(owner)),
            args: vec![Some(handle), Some(selection.clone())],
            arg_order: Vec::new(),
        }
    };
    tree.state.inline_randomizations.borrow_mut().push(inline);
    Ok(Expr::Call {
        callee: Callee::Subroutine(name),
        args: [vec![Some(handle), Some(selection)], input_values].concat(),
        arg_order: Vec::new(),
    })
}

/// The declared result type of the method that native callee expression
/// `id` names (`h.m`, `C::m`, or unqualified inside a method), when known.
pub(in crate::ast) fn method_result(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<Type>> {
    let (class_name, name) = match &native.expression(ungroup(id, native)).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } => match separator.as_str() {
            "." => (native_class(tree, *expression, native, dimensions), name),
            "::" => (named_class(tree, *expression, native, dimensions)?, name),
            _ => return Ok(None),
        },
        ExprKind::Name(name)
            if dimensions
                .get(&instances::normalize_identifier(&name.spelling))
                .is_none() =>
        {
            (scope_class(dimensions), name)
        }
        _ => return Ok(None),
    };
    let Some(class_name) = class_name.filter(|name| name != super::super::processes::CLASS) else {
        return Ok(None);
    };
    Ok(class(tree, dimensions, &class_name)?
        .method(&instances::normalize_identifier(&name.spelling))
        .and_then(|method| method.result.clone().flatten()))
}
