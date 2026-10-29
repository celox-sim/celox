//! The built-in parameterized mailbox class (IEEE 1800-2023 15.4). A mailbox
//! is a heap object holding a queue of its messages, which the scheduler
//! operates on.
use super::*;
use celox_design::ProcessOp;
use celox_sv_syntax::{Argument, Declarations, ExprId, ExprKind, TypeSegment};
use system_functions::HandleProperty;

/// The start of the class name of each `mailbox #(T)`.
const PREFIX: &str = "$builtin::mailbox#(";

/// Whether class `name` is a `mailbox #(T)`.
pub(super) fn is_class(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// The message type of a mailbox, as its class name records it.
struct Message {
    width: u32,
    signed: bool,
    two_state: bool,
    handle: Option<HandleProperty>,
    /// The class of a message that is a class, process or event handle.
    class: Option<String>,
}

impl Message {
    /// The message type of `mailbox #(ty)`.
    fn of(state: &AnalysisState, ty: &Type, env: &HashMap<String, i128>) -> Converted<Self> {
        if !ty.unpacked_ranges.is_empty()
            || ty.container.is_some()
            || ty.unpacked_struct().is_some()
            || matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal)
        {
            return Err(unsupported(
                "mailbox messages that are not integral values, strings or handles",
            ));
        }
        let shape = casts::expr_type_from_type(state, ty, env)
            .ok_or_else(|| unsupported("mailbox message type"))?;
        if shape.width == 0 || shape.width > 64 {
            return Err(unsupported("mailbox messages wider than 64 bits"));
        }
        let handle = match ty.kind() {
            TypeKind::String => Some(HandleProperty::String),
            TypeKind::Object => Some(HandleProperty::Object),
            _ => None,
        };
        Ok(Self {
            width: shape.width as u32,
            signed: shape.signed && handle.is_none(),
            two_state: handle.is_some()
                || matches!(
                    ty.kind(),
                    TypeKind::Bit | TypeKind::Process | TypeKind::Event
                ),
            handle,
            class: ty.class().map(str::to_owned),
        })
    }

    /// The class name of `mailbox #(ty)`: the spelling of the type, then the
    /// shape its methods use.
    fn class_name(&self, ty: &Type) -> String {
        format!(
            "{PREFIX}{})@{},{},{},{}",
            classes::specializations::spelling(ty),
            self.width,
            u8::from(self.signed),
            u8::from(self.two_state),
            match (self.handle, self.class.as_deref()) {
                (Some(HandleProperty::String), _) => 1,
                (Some(HandleProperty::Object), _) => 2,
                (None, Some(processes::CLASS)) => 3,
                (None, Some(events::CLASS)) => 4,
                (None, _) => 0,
            },
        )
    }

    /// The message type of mailbox class `name`.
    fn parse(name: &str) -> Option<Self> {
        let (spelling, shape) = name.strip_prefix(PREFIX)?.rsplit_once(")@")?;
        let mut fields = shape.split(',');
        let mut field = || fields.next()?.parse::<u32>().ok();
        let (width, signed, two_state, handle) = (field()?, field()?, field()?, field()?);
        let (handle, class) = match handle {
            0 => (None, None),
            1 => (Some(HandleProperty::String), None),
            2 => (Some(HandleProperty::Object), Some(spelling)),
            3 => (None, Some(processes::CLASS)),
            _ => (None, Some(events::CLASS)),
        };
        Some(Self {
            width,
            signed: signed != 0,
            two_state: two_state != 0,
            handle,
            class: class.map(str::to_owned),
        })
    }

    /// The heap container parameter of the queue of messages.
    fn layout(&self) -> u64 {
        celox_heap::containers::parameter(
            self.width,
            self.two_state,
            match self.handle {
                None => 0,
                Some(HandleProperty::String) => 1,
                Some(HandleProperty::Object) => 2,
            },
            1,
            0,
            false,
            false,
            false,
        )
    }
}

/// The handle type of `mailbox #(T)` or `std::mailbox #(T)`, unless a class
/// of the design is named `mailbox`.
pub(super) fn named_type(
    path: &[TypeSegment],
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Option<Box<Type>>> {
    let segment = match path {
        [segment]
            if segment.name.spelling == "mailbox"
                && classes::resolve(tree, "mailbox").is_none() =>
        {
            segment
        }
        [std, segment] if std.name.spelling == "std" && segment.name.spelling == "mailbox" => {
            segment
        }
        _ => return Ok(None),
    };
    let argument = match segment.arguments.as_deref() {
        // A typeless mailbox checks message types at run time (15.4.9).
        None | Some([]) => return Err(unsupported("mailbox without a message type")),
        Some([argument]) if argument.name.is_none() => argument.value,
        Some(_) => return Err(unsupported("mailbox parameters")),
    };
    let argument = argument.ok_or_else(|| unsupported("mailbox message type"))?;
    // The type may name a constant that `context` does not know yet.
    let Some(message) = classes::specializations::type_argument(tree, argument, context)? else {
        return Ok(None);
    };
    let name = Message::of(&tree.state, &message, &context.const_env)?.class_name(&message);
    Ok(Some(Box::new(classes::handle(&name))))
}

/// `new(bound)` of mailbox class `class` (15.4.1).
pub(super) fn new(
    tree: &AnalysisContext<'_>,
    arguments: &[Argument],
    native: &Declarations,
    dimensions: &PackedDimensions,
    class: &str,
) -> Converted<Expr> {
    let message = Message::parse(class).ok_or_else(|| unsupported("mailbox class"))?;
    // `new()` lists one empty argument.
    let arguments = match arguments {
        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
        arguments => arguments,
    };
    let bound = match arguments {
        [] => Expr::Literal("32'sd0".into()),
        [argument]
            if argument
                .name
                .as_ref()
                .is_none_or(|name| name.spelling == "bound") =>
        {
            match argument.value {
                Some(value) => expression_binding::value(value, native, tree, dimensions)?,
                None => Expr::Literal("32'sd0".into()),
            }
        }
        _ => return Err(unsupported("arguments of mailbox new")),
    };
    Ok(events::call(
        ProcessOp::MailboxNew,
        vec![bound, Expr::Literal(format!("64'd{}", message.layout()))],
    ))
}

/// A call of a method of a mailbox (15.4.2–15.4.8), as a statement or, for
/// the methods that are functions, as a value.
pub(super) fn value(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    statement: bool,
) -> Converted<Option<Expr>> {
    let id = expression_binding::ungroup(id, native);
    let (callee, arguments) = match &native.expression(id).kind {
        ExprKind::Call {
            expression,
            arguments,
        } => (*expression, arguments.as_slice()),
        _ => (id, &[][..]),
    };
    let ExprKind::Member {
        expression: receiver,
        separator,
        name,
    } = &native.expression(callee).kind
    else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let Some(class) = expression_binding::object_native_class(tree, *receiver, native, dimensions)
        .filter(|class| is_class(class))
    else {
        return Ok(None);
    };
    let message = Message::parse(&class).ok_or_else(|| unsupported("mailbox class"))?;
    let method = instances::normalize_identifier(&name.spelling);
    let op = match method.as_str() {
        "num" => ProcessOp::MailboxNum,
        "put" => ProcessOp::MailboxPut,
        "try_put" => ProcessOp::MailboxTryPut,
        "get" => ProcessOp::MailboxGet,
        "try_get" => ProcessOp::MailboxTryGet,
        "peek" => ProcessOp::MailboxPeek,
        "try_peek" => ProcessOp::MailboxTryPeek,
        _ => return Err(unsupported(format!("mailbox method `{method}`"))),
    };
    if !statement
        && matches!(
            op,
            ProcessOp::MailboxPut | ProcessOp::MailboxGet | ProcessOp::MailboxPeek
        )
    {
        return Err(unsupported(format!(
            "mailbox {method}, which is a task, used as a value"
        )));
    }
    // `m.num()` lists one empty argument.
    let arguments = match arguments {
        [argument] if argument.name.is_none() && argument.value.is_none() => &[],
        arguments => arguments,
    };
    let mailbox = expression_binding::value(*receiver, native, tree, dimensions)?;
    if op == ProcessOp::MailboxNum {
        if !arguments.is_empty() {
            return Err(unsupported("arguments of mailbox num"));
        }
        return Ok(Some(events::call(op, vec![mailbox])));
    }
    let [argument] = arguments else {
        return Err(unsupported(format!("arguments of mailbox {method}")));
    };
    let Some(argument) = argument.value.filter(|_| {
        argument
            .name
            .as_ref()
            .is_none_or(|name| name.spelling == "message")
    }) else {
        return Err(unsupported(format!("arguments of mailbox {method}")));
    };
    // A message has the type of the mailbox (15.4.9).
    let class_matches =
        |id| expression_binding::object_native_class(tree, id, native, dimensions) == message.class;
    let string = expression_binding::native_string(tree, argument, native, dimensions);
    let message_value = if matches!(op, ProcessOp::MailboxPut | ProcessOp::MailboxTryPut) {
        match message.handle {
            Some(HandleProperty::Object) => {
                let class = message.class.as_deref().unwrap_or_default();
                if let Some(call) = expression_binding::object_constructor(
                    tree, argument, native, dimensions, class,
                )? {
                    call
                } else {
                    expression_binding::check_handle_assignment(
                        tree,
                        argument,
                        native,
                        dimensions,
                        class,
                        || "mailbox message".to_owned(),
                    )?;
                    expression_binding::value(argument, native, tree, dimensions)?
                }
            }
            Some(HandleProperty::String) => {
                let value = expression_binding::value(argument, native, tree, dimensions)?;
                expression_binding::string_operand(value, dimensions)
            }
            None => {
                if let Some(class) = &message.class {
                    expression_binding::check_handle_assignment(
                        tree,
                        argument,
                        native,
                        dimensions,
                        class,
                        || "mailbox message".to_owned(),
                    )?;
                }
                // The lowering converts it to the message type.
                expression_binding::value(argument, native, tree, dimensions)?
            }
        }
    } else {
        let compatible = match message.handle {
            Some(HandleProperty::String) => string,
            // A process or event message has its built-in class.
            Some(HandleProperty::Object) | None if message.class.is_some() => {
                class_matches(argument)
            }
            _ => {
                !string
                    && expression_binding::object_native_class(tree, argument, native, dimensions)
                        .is_none()
            }
        };
        if !compatible {
            return Err(unsupported(format!(
                "mailbox {method} into a variable of another type than its messages"
            )));
        }
        expression_binding::value(argument, native, tree, dimensions)?
    };
    // The lowering takes the message shape from the layout, which it does
    // not pass on to the scheduler, and copies a message out as a value of
    // its signedness.
    let layout = Expr::Literal(format!("64'd{}", message.layout()));
    let signed = Expr::Literal(format!("1'd{}", u8::from(message.signed)));
    Ok(Some(events::call(
        op,
        vec![mailbox, message_value, layout, signed],
    )))
}
