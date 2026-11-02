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
    let (id, predicate) = match &native.expression(id).kind {
        ExprKind::ArrayMethodWith {
            expression,
            predicate,
        } => (ungroup(*expression, native), Some(*predicate)),
        _ => (id, None),
    };
    let callee = match &native.expression(id).kind {
        ExprKind::Call { expression, .. } => ungroup(*expression, native),
        ExprKind::Member { .. } => id,
        // A system function called without parentheses, such as `$time`.
        ExprKind::Name(name) if name.spelling.starts_with('$') => id,
        _ => return None,
    };
    if let ExprKind::Name(name) = &native.expression(callee).kind {
        return system_function(tree, session, id, &name.spelling, native, dims);
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
    let reduction = array_reductions::reduction_op(method);
    // Do not traverse a receiver for arbitrary property/method names.
    if locator.is_none()
        && string.is_none()
        && reduction.is_none()
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
            // 7.12.3: a reduction gives a value of the element type, or of
            // the type of its `with` expression.
            if reduction.is_some() {
                return match predicate {
                    Some(predicate) => type_of(tree, session, predicate, native, dims).ok(),
                    None => Some(element),
                };
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

/// The result of a system function, by its declaration in IEEE 1800-2023
/// clauses 20 and 21 and Annex D, whatever its arguments' kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SystemResult {
    Integral {
        width: u128,
        signed: bool,
        four_state: bool,
    },
    Real,
    Shortreal,
    String,
    /// The first argument's type with this signing (11.7).
    Signing(bool),
    /// The first argument's type (16.9.3).
    Operand,
    /// No value of a self-determined type: a clocking event or a pattern
    /// sized by its context.
    Untyped,
}

/// The result of system function `name`; `None` for a name that is not a
/// system function.
pub(super) fn system_result(name: &str) -> Option<SystemResult> {
    use SystemResult::*;
    const INTEGER: SystemResult = Integral {
        width: 32,
        signed: true,
        four_state: true,
    };
    const INT: SystemResult = Integral {
        width: 32,
        signed: true,
        four_state: false,
    };
    const BIT: SystemResult = Integral {
        width: 1,
        signed: false,
        four_state: false,
    };
    Some(match name {
        // 20.3
        "$time" => Integral {
            width: 64,
            signed: false,
            four_state: true,
        },
        "$stime" => Integral {
            width: 32,
            signed: false,
            four_state: true,
        },
        "$realtime" => Real,
        // 20.4, 20.6, 20.7, 20.8.1
        "$timeunit"
        | "$timeprecision"
        | "$bits"
        | "$dimensions"
        | "$unpacked_dimensions"
        | "$left"
        | "$right"
        | "$low"
        | "$high"
        | "$increment"
        | "$size"
        | "$clog2" => INTEGER,
        // 20.5
        "$rtoi" => INTEGER,
        "$itor" | "$bitstoreal" => Real,
        "$bitstoshortreal" => Shortreal,
        "$realtobits" => Integral {
            width: 64,
            signed: false,
            four_state: false,
        },
        "$shortrealtobits" => Integral {
            width: 32,
            signed: false,
            four_state: false,
        },
        // 11.7
        "$signed" => Signing(true),
        "$unsigned" => Signing(false),
        // 6.24.2: as a function, $cast returns int.
        "$cast" => INT,
        // 20.6.1, 21.3.3
        "$typename" | "$sformatf" => String,
        "$isunbounded" => BIT,
        // 20.8.2
        "$ln" | "$log10" | "$exp" | "$sqrt" | "$pow" | "$floor" | "$ceil" | "$sin" | "$cos"
        | "$tan" | "$asin" | "$acos" | "$atan" | "$atan2" | "$hypot" | "$sinh" | "$cosh"
        | "$tanh" | "$asinh" | "$acosh" | "$atanh" => Real,
        // 20.9
        "$countbits" | "$countones" => INT,
        "$onehot" | "$onehot0" | "$isunknown" => BIT,
        // 16.9.3, 16.9.4
        "$sampled" | "$past" | "$past_gclk" | "$future_gclk" => Operand,
        "$rose" | "$fell" | "$stable" | "$changed" | "$rose_gclk" | "$fell_gclk"
        | "$stable_gclk" | "$changed_gclk" | "$rising_gclk" | "$falling_gclk" | "$steady_gclk"
        | "$changing_gclk" => BIT,
        // 14.14, 16.14.7: clocking events, no values.
        "$global_clock" | "$inferred_clock" | "$inferred_disable" => Untyped,
        // 20.14
        "$coverage_control" | "$coverage_get_max" | "$coverage_get" | "$coverage_merge"
        | "$coverage_save" => INTEGER,
        "$get_coverage" => Real,
        // 20.15, 20.16, 18.13
        "$random" | "$dist_uniform" | "$dist_normal" | "$dist_exponential" | "$dist_poisson"
        | "$dist_chi_square" | "$dist_t" | "$dist_erlang" | "$q_full" => INTEGER,
        "$urandom" | "$urandom_range" => Integral {
            width: 32,
            signed: false,
            four_state: false,
        },
        // 20.18, 20.17: $system returns int, $stacktrace as a function a
        // string.
        "$system" => INT,
        "$stacktrace" => String,
        // 21.3, 21.6
        "$fopen" | "$fgetc" | "$ungetc" | "$fgets" | "$fscanf" | "$sscanf" | "$fread"
        | "$ftell" | "$fseek" | "$rewind" | "$ferror" | "$feof" | "$test$plusargs"
        | "$value$plusargs" => INTEGER,
        // Annex D
        "$countdrivers" | "$reset_count" | "$reset_value" => INTEGER,
        "$getpattern" | "$scale" => Untyped,
        _ => return None,
    })
}

/// The result type of a call `id` of system function `name`.
fn system_function(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: ExprId,
    name: &str,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    let first = match &native.expression(id).kind {
        ExprKind::Call { arguments, .. } => arguments.first().and_then(|argument| argument.value),
        _ => None,
    };
    match system_result(name)? {
        SystemResult::Integral {
            width,
            signed,
            four_state,
        } => values::integral(&mut session.types, width, signed, four_state),
        SystemResult::Real => Some(session.types.intern(TyData::Real)),
        SystemResult::Shortreal => Some(session.types.intern(TyData::Shortreal)),
        SystemResult::String => Some(session.types.intern(TyData::String)),
        SystemResult::Signing(signed) => {
            let operand = type_of(tree, session, first?, native, dims).ok()?;
            let TyData::Packed(packed) = session.types.data(operand).clone() else {
                return None;
            };
            values::integral(&mut session.types, packed.bits(), signed, packed.four_state)
        }
        SystemResult::Operand => type_of(tree, session, first?, native, dims).ok(),
        SystemResult::Untyped => None,
    }
}
