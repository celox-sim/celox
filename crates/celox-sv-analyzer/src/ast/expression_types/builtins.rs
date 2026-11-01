//! Self-determined results of builtins, from the receiver's semantic type.
//! Arguments and `with` predicates are checked by ordinary binding; querying
//! a result type neither binds them nor executes a method.
use super::*;
use crate::system_functions::{LocatorMethod, StringOp};

pub(super) fn result(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    let id = match &native.expression(id).kind {
        ExprKind::ArrayMethodWith { expression, .. } => ungroup(*expression, native),
        _ => id,
    };
    let callee = match &native.expression(id).kind {
        ExprKind::Call { expression, .. } => ungroup(*expression, native),
        ExprKind::Member { .. } => id,
        _ => return None,
    };
    if let ExprKind::Name(name) = &native.expression(callee).kind {
        // 20.6.1 and 21.3.3: the result is a string, independently of its
        // contents or the formatting arguments.
        return matches!(name.spelling.as_str(), "$typename" | "$sformatf")
            .then(|| session.types.intern(TyData::String));
    }
    let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return None;
    };
    if separator != "." {
        return None;
    }
    // Every recognized builtin is a simple identifier. For those names,
    // removing an escape is exactly identifier normalization; all other
    // spellings are rejected below. Borrow even when querying an ordinary
    // property, rather than allocating a name at every member expression.
    let method = name.spelling.strip_prefix('\\').unwrap_or(&name.spelling);
    let locator = LocatorMethod::from_name(method);
    let string = StringOp::method(method).map(|(op, _)| op);
    // Do not traverse a receiver for arbitrary property/method names.
    if locator.is_none()
        && string.is_none()
        && !matches!(
            method,
            "pop_front"
                | "pop_back"
                | "size"
                | "num"
                | "get_randstate"
                | "first"
                | "last"
                | "next"
                | "prev"
                | "name"
        )
    {
        return None;
    }
    let receiver = type_of(tree, session, *receiver, native, dims).ok()?;
    match session.types.data(receiver).clone() {
        TyData::Array { kind, element } => {
            // 7.12.1: locators return a queue of elements, or a queue of
            // indices. An associative index keeps its declared type.
            if let Some(locator) = locator {
                let element = if locator.returns_index() {
                    match kind {
                        ArrayKind::Associative { key } => key,
                        _ => int(tree, session, dims)?,
                    }
                } else {
                    element
                };
                return Some(session.types.intern(TyData::Array {
                    kind: ArrayKind::Queue { bound: None },
                    element,
                }));
            }
            match (&kind, method) {
                // 7.10.2.4–5: removing an element does not change its type.
                (ArrayKind::Queue { .. }, "pop_front" | "pop_back") => Some(element),
                // 7.5.2, 7.9.1, 7.10.2.1.
                (ArrayKind::Dynamic | ArrayKind::Queue { .. }, "size")
                | (ArrayKind::Associative { .. }, "num" | "size") => int(tree, session, dims),
                _ => None,
            }
        }
        TyData::Packed(semantic_types::Packed {
            element: PackedElement::Enum { array: false, .. },
            ..
        }) => {
            match method {
                // 6.19.5: member methods return the receiver's enum type.
                "first" | "last" | "next" | "prev" => Some(receiver),
                "num" => int(tree, session, dims),
                "name" => Some(session.types.intern(TyData::String)),
                _ => None,
            }
        }
        TyData::String => {
            let op = string?;
            // `putc` and the itoa family are void methods (6.16); their
            // runtime operations return an updated handle internally. Until
            // Void is represented, report an unsupported semantic query.
            if op.updates_receiver() {
                return None;
            }
            if op.returns_string() {
                Some(session.types.intern(TyData::String))
            } else {
                // 6.16.1, 6.16.3, 6.16.6–9: retain the declared integer
                // data type, including its state domain and keyword.
                let name = match op {
                    StringOp::Getc => "byte",
                    // The radix conversion prototypes return integer,
                    // rather than int (6.16.9).
                    StringOp::Atoi(_) => "integer",
                    _ => "int",
                };
                let ty = type_binding::builtin_type(name, None)?;
                semantic_types::of_type(&mut session.types, &tree.functions, &ty, &dims.const_env)
            }
        }
        // 9.7: get_randstate returns the process random state as a string.
        TyData::Class(Decl::Class(name))
            if name.as_ref() == processes::CLASS && method == "get_randstate" =>
        {
            Some(session.types.intern(TyData::String))
        }
        _ => None,
    }
}

fn int(tree: &AnalysisContext<'_>, session: &mut Session, dims: &PackedDimensions) -> Option<TyId> {
    semantic_types::of_type(
        &mut session.types,
        &tree.functions,
        &procedural::int_type(),
        &dims.const_env,
    )
}
