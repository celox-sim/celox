//! Assignment expressions and increment/decrement (IEEE 1800-2023 11.3.6, 11.4.2).
//!
//! Assignment values are cast and saved before the target is updated, and
//! the expression binds to that saved, destination-typed value.
//!
//! An increment/decrement operator is executed as statements before the statement that holds
//! it: they read the operand into a hidden local, write the updated value
//! back with a blocking assignment, and the operator binds to the local,
//! the operand's value before (`x++`) or after (`++x`) the update. The
//! operators of an operand that `&&`, `||`, `->` or `?:` may skip (11.4.7,
//! 11.4.11) run only where it is evaluated: the operand that decides is
//! evaluated once, into a hidden local the expression then reads. A loop
//! condition runs its operators every time it is evaluated (see
//! [`BodyBuilder::loop_condition`]). Other contexts that skip or repeat an
//! operand keep rejecting it.
//!
//! A `$cast` to a class property runs the same way, as a cast to a hidden
//! handle that the property takes when the cast succeeds (8.16). So does a
//! `$cast` whose classes make it fail whatever its source's value, when
//! evaluating the source may call a subroutine: the source is still
//! evaluated. A `$cast` to an enumerated type runs the same way: it
//! assigns the source only when its value is one of the type's members
//! (6.24.2).
use super::*;
use celox_sv_syntax::{Declarations, DimensionKind, ExprId, ExprKind};

/// The operand of increment or decrement `id`, whether it is postfix, and
/// whether it increments.
fn update(native: &Declarations, id: ExprId) -> Option<(ExprId, bool, bool)> {
    match &native.expression(id).kind {
        ExprKind::Unary {
            operator,
            expression,
            postfix,
        } if matches!(operator.as_str(), "++" | "--") => {
            Some((*expression, *postfix, operator == "++"))
        }
        _ => None,
    }
}

fn dimension_operands(dimension: &DimensionKind, operands: &mut Vec<ExprId>) {
    match dimension {
        DimensionKind::Index(id) | DimensionKind::Queue(Some(id)) => operands.push(*id),
        DimensionKind::Range { left, right } => operands.extend([*left, *right]),
        DimensionKind::Indexed { base, width, .. } => operands.extend([*base, *width]),
        DimensionKind::Unsized | DimensionKind::Queue(None) | DimensionKind::Wildcard => {}
    }
}

/// Every operand of `kind`, in source order.
fn operands(kind: &ExprKind) -> Vec<ExprId> {
    let mut operands = Vec::new();
    match kind {
        ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Type(_) => {}
        ExprKind::MinTypMax {
            minimum,
            typical,
            maximum,
        } => operands.extend([*minimum, *typical, *maximum]),
        ExprKind::Group(id) | ExprKind::Copy(id) => operands.push(*id),
        ExprKind::Unary { expression, .. } | ExprKind::Member { expression, .. } => {
            operands.push(*expression)
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Assignment { left, right, .. } => {
            operands.extend([*left, *right])
        }
        ExprKind::Conditional {
            condition,
            then_expression,
            else_expression,
        } => operands.extend([*condition, *then_expression, *else_expression]),
        ExprKind::Tagged { value, .. } => operands.extend(*value),
        ExprKind::Call {
            expression,
            arguments,
        }
        | ExprKind::Parameterized {
            expression,
            arguments,
        } => {
            operands.push(*expression);
            operands.extend(arguments.iter().filter_map(|argument| argument.value));
        }
        ExprKind::ArrayMethodWith {
            expression,
            predicate,
        } => operands.extend([*expression, *predicate]),
        ExprKind::RandomizeWith {
            expression,
            constraints,
        } => {
            operands.push(*expression);
            operands.extend(constraints.iter().flatten());
        }
        ExprKind::Select {
            expression,
            dimension,
        }
        | ExprKind::StreamSelect {
            expression,
            dimension,
        } => {
            operands.push(*expression);
            dimension_operands(&dimension.kind, &mut operands);
        }
        ExprKind::Cast { target, expression } => operands.extend([*target, *expression]),
        ExprKind::Concatenation(items) => operands.extend(items),
        ExprKind::Replication { count, expressions } => {
            operands.push(*count);
            operands.extend(expressions);
        }
        ExprKind::Pattern { target, items } => {
            operands.extend(*target);
            for item in items {
                operands.extend(item.key);
                operands.push(item.value);
            }
        }
        ExprKind::Set(dimensions) => {
            for dimension in dimensions {
                dimension_operands(&dimension.kind, &mut operands);
            }
        }
        ExprKind::Streaming {
            slice, expressions, ..
        } => {
            operands.extend(*slice);
            operands.extend(expressions);
        }
    }
    operands
}

/// The destination and source of `$cast(destination, source)` call `id`.
fn cast_operands(native: &Declarations, id: ExprId) -> Option<(ExprId, ExprId)> {
    let ExprKind::Call {
        expression,
        arguments,
    } = &native.expression(id).kind
    else {
        return None;
    };
    let ExprKind::Name(name) = &native.expression(*expression).kind else {
        return None;
    };
    match arguments.as_slice() {
        [destination, source]
            if name.spelling == "$cast" && destination.name.is_none() && source.name.is_none() =>
        {
            Some((destination.value?, source.value?))
        }
        _ => None,
    }
}

/// Whether `id` holds a call.
fn calls(native: &Declarations, id: ExprId) -> bool {
    let mut jobs = vec![id];
    while let Some(id) = jobs.pop() {
        let kind = &native.expression(id).kind;
        if matches!(kind, ExprKind::Call { .. }) {
            return true;
        }
        jobs.extend(operands(kind));
    }
    false
}

/// The expressions of `roots` that hold an assignment, increment or
/// decrement, or a `$cast` that `hoists_cast`.
fn updating(
    native: &Declarations,
    roots: &[ExprId],
    hoists_cast: &mut dyn FnMut(ExprId) -> bool,
) -> HashSet<ExprId> {
    let mut updating = HashSet::default();
    let mut jobs: Vec<(ExprId, bool)> = roots.iter().map(|id| (*id, false)).collect();
    let mut path = Vec::new();
    while let Some((id, exit)) = jobs.pop() {
        if exit {
            path.pop();
            continue;
        }
        if update(native, id).is_some()
            || matches!(native.expression(id).kind, ExprKind::Assignment { .. })
            || hoists_cast(id)
        {
            updating.insert(id);
            updating.extend(path.iter().copied());
        }
        path.push(id);
        jobs.push((id, true));
        jobs.extend(
            operands(&native.expression(id).kind)
                .into_iter()
                .rev()
                .map(|id| (id, false)),
        );
    }
    updating
}

/// What kind of value the source of a `$cast` to an enumerated type is.
enum CastSource {
    /// An integral value of its self-determined width and signing, which
    /// may be a member's (6.24.2).
    Integral {
        width: usize,
        signed: bool,
    },
    /// A class handle, of the class when known, or another value that is not
    /// cast compatible: a virtual interface, an unpacked structure or array.
    Handle(Option<String>),
    Chandle,
    Null,
}

fn unsupported_context() -> AnalyzerError {
    unsupported("assignment, increment or decrement expression in this context")
}

impl BodyBuilder<'_, '_, '_> {
    /// Execute assignment, increment and decrement operators of `roots`, evaluated
    /// in that order, and return the statements that do.
    pub(super) fn effects(&mut self, roots: &[ExprId]) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let updating = updating(native, roots, &mut |id| {
            matches!(self.hoisted_cast(id), Ok(Some(_)))
        });
        let mut stmts = Vec::new();
        if updating.is_empty() {
            return Ok(stmts);
        }
        for root in roots {
            self.effect(*root, &updating, &mut stmts)?;
        }
        Ok(stmts)
    }

    fn effect(
        &mut self,
        id: ExprId,
        updating: &HashSet<ExprId>,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        if !updating.contains(&id) {
            return Ok(());
        }
        let native = self.tree.declarations();
        if let Some((destination, source)) = self.hoisted_cast(id)? {
            self.effect(destination, updating, stmts)?;
            self.effect(source, updating, stmts)?;
            return self.cast_to_property(id, destination, source, stmts);
        }
        if let Some((operand, postfix, increment)) = update(native, id) {
            self.effect(operand, updating, stmts)?;
            return self.update(id, operand, Some(postfix), increment, stmts);
        }
        if let ExprKind::Assignment {
            left,
            operator,
            right,
        } = &native.expression(id).kind
        {
            let (left, right, operator) = (*left, *right, operator.clone());
            self.effect(left, updating, stmts)?;
            self.effect(right, updating, stmts)?;
            return self.assignment_expression(id, left, &operator, right, stmts);
        }
        match &native.expression(id).kind {
            ExprKind::Binary {
                left,
                operator,
                right,
            } if matches!(operator.as_str(), "&&" | "||" | "->") && updating.contains(right) => {
                self.effect(*left, updating, stmts)?;
                let decided = self.truth(*left, stmts)?;
                // `&&` and `->` evaluate the right operand unless the left
                // one is false, `||` unless it is true.
                let skip = if operator == "||" { "1'b1" } else { "1'b0" };
                let mut right_stmts = Vec::new();
                self.effect(*right, updating, &mut right_stmts)?;
                stmts.push(Self::unless(&decided, skip, right_stmts));
            }
            ExprKind::Conditional {
                condition,
                then_expression,
                else_expression,
            } if updating.contains(then_expression) || updating.contains(else_expression) => {
                self.effect(*condition, updating, stmts)?;
                let decided = self.truth(*condition, stmts)?;
                // An unknown condition evaluates both (11.4.11).
                for (arm, skip) in [(*then_expression, "1'b0"), (*else_expression, "1'b1")] {
                    let mut arm_stmts = Vec::new();
                    self.effect(arm, updating, &mut arm_stmts)?;
                    if !arm_stmts.is_empty() {
                        stmts.push(Self::unless(&decided, skip, arm_stmts));
                    }
                }
            }
            ExprKind::Group(_)
            | ExprKind::Unary { .. }
            | ExprKind::Binary { .. }
            | ExprKind::Conditional { .. }
            | ExprKind::Member { .. }
            | ExprKind::Select { .. }
            | ExprKind::Concatenation(_) => {
                for operand in operands(&native.expression(id).kind) {
                    self.effect(operand, updating, stmts)?;
                }
            }
            // The callee, then each actual.
            ExprKind::Call {
                expression,
                arguments,
            } if !matches!(
                native.expression(*expression).kind,
                ExprKind::ArrayMethodWith { .. } | ExprKind::RandomizeWith { .. }
            ) =>
            {
                self.effect(*expression, updating, stmts)?;
                // A query does not evaluate its first operand (20.6, 20.7).
                let unevaluated = match &native.expression(*expression).kind {
                    ExprKind::Name(name) => SystemFunction::from_name(&name.spelling)
                        .is_some_and(SystemFunction::has_unevaluated_operand),
                    _ => false,
                };
                // An update in $ferror's output index is lifted into the
                // prelude. Save the preceding FD first, including its calls,
                // so that the prelude cannot reorder the two arguments.
                if matches!(&native.expression(*expression).kind,
                    ExprKind::Name(name) if name.spelling == "$ferror")
                    && let [fd, output] = arguments.as_slice()
                    && let (Some(fd), Some(output)) = (fd.value, output.value)
                    && updating.contains(&output)
                {
                    self.effect(fd, updating, stmts)?;
                    let mut rhs = self.native_chandle_value(fd)?;
                    let (source, unique) = self.hidden(
                        type_binding::builtin_type("integer", None).expect("a builtin type"),
                    );
                    self.rename_expr(&mut rhs);
                    stmts.push(Stmt::Assign {
                        lhs: LValue::Ident(unique),
                        rhs,
                        nonblocking: false,
                    });
                    self.bind_effect(fd, Expr::Ident(source));
                    self.effect(output, updating, stmts)?;
                    return Ok(());
                }
                for argument in arguments
                    .iter()
                    .skip(usize::from(unevaluated))
                    .filter_map(|argument| argument.value)
                {
                    self.effect(argument, updating, stmts)?;
                }
            }
            ExprKind::Cast { expression, .. } => self.effect(*expression, updating, stmts)?,
            ExprKind::Replication { expressions, .. } => {
                for operand in expressions {
                    self.effect(*operand, updating, stmts)?;
                }
            }
            _ => return Err(unsupported_context()),
        }
        Ok(())
    }

    /// The destination and source of `id` when it is a `$cast` that runs
    /// as statements: one to a class property or an element of one (see
    /// [`Self::property_cast`]), or one that fails whatever its source's
    /// value while an operand calls a subroutine, whose call must still run.
    fn hoisted_cast(&mut self, id: ExprId) -> Converted<Option<(ExprId, ExprId)>> {
        if let Some(operands) = self.property_cast(id)? {
            return Ok(Some(operands));
        }
        if let Some((destination, source, _)) = self.enum_cast(id)? {
            return Ok(Some((destination, source)));
        }
        let native = self.tree.declarations();
        let Some((destination, source)) = cast_operands(native, id) else {
            return Ok(None);
        };
        if !self.effectful(source) && !self.effectful(destination) {
            return Ok(None);
        }
        let cast = expression_binding::object_cast_to(
            self.tree,
            self.session,
            destination,
            source,
            native,
            &self.dims,
        )?;
        Ok(matches!(cast, Some(expression_binding::ObjectCast::Fails))
            .then_some((destination, source)))
    }

    /// The destination, source and destination type of `id` when it is a
    /// `$cast` to a variable of an enumerated type (6.24.2).
    fn enum_cast(
        &mut self,
        id: ExprId,
    ) -> Converted<Option<(ExprId, ExprId, Arc<enums::EnumDomain>)>> {
        let native = self.tree.declarations();
        let Some((destination, source)) = cast_operands(native, id) else {
            return Ok(None);
        };
        Ok(expression_binding::enum_domain(
            self.tree,
            self.session,
            destination,
            native,
            &self.dims,
        )?
        .map(|domain| (destination, source, domain)))
    }

    /// The destination and source of `id` when it is a `$cast` to a class
    /// property or to an element of one, which is no variable that the
    /// cast's subroutine could write (8.16).
    fn property_cast(&mut self, id: ExprId) -> Converted<Option<(ExprId, ExprId)>> {
        let native = self.tree.declarations();
        let Some((destination, source)) = cast_operands(native, id) else {
            return Ok(None);
        };
        if expression_binding::object_native_class(
            self.tree,
            self.session,
            destination,
            native,
            &self.dims,
        )
        .is_none()
        {
            return Ok(None);
        }
        let mut root = destination;
        loop {
            if expression_binding::object_property(
                self.tree,
                self.session,
                root,
                native,
                &self.dims,
            )?
            .is_some()
            {
                return Ok(Some((destination, source)));
            }
            match &native
                .expression(expression_binding::ungroup(root, native))
                .kind
            {
                ExprKind::Select { expression, .. } => root = *expression,
                _ => return Ok(None),
            }
        }
    }

    /// Whether evaluating native expression `id` has an effect: it calls a
    /// subroutine, also a method named without parentheses, as its bound
    /// value shows.
    fn effectful(&mut self, id: ExprId) -> bool {
        let native = self.tree.declarations();
        calls(native, id)
            || expression_binding::value(id, native, self.tree, self.session, &self.dims)
                .is_ok_and(|value| inlining::effectful(&value))
    }

    /// The operands of target `destination` that a `$cast` captures, outside
    /// in: the class-handle receivers its write goes through, which an
    /// assignment reads before its value (a saved receiver covers the
    /// operands inside it), and the indices with an effect, which it
    /// evaluates once whether or not it writes (8.16, 6.24.2). A pure index
    /// is read where the destination is written, as an assignment reads it.
    fn destination_operands(&mut self, destination: ExprId) -> (Vec<ExprId>, Vec<ExprId>) {
        let native = self.tree.declarations();
        let (mut receivers, mut indices) = (Vec::new(), Vec::new());
        let mut id = expression_binding::ungroup(destination, native);
        loop {
            match &native.expression(id).kind {
                ExprKind::Member {
                    expression,
                    separator,
                    ..
                } if separator == "." => {
                    let receiver = *expression;
                    if expression_binding::object_native_class(
                        self.tree,
                        self.session,
                        receiver,
                        native,
                        &self.dims,
                    )
                    .is_some()
                    {
                        receivers.push(receiver);
                        break;
                    }
                    id = expression_binding::ungroup(receiver, native);
                }
                ExprKind::Select {
                    expression,
                    dimension,
                } => {
                    let (base, selects) = match &dimension.kind {
                        DimensionKind::Index(index) | DimensionKind::Queue(Some(index)) => {
                            (*expression, vec![*index])
                        }
                        DimensionKind::Range { left, right } => (*expression, vec![*right, *left]),
                        DimensionKind::Indexed { base, .. } => (*expression, vec![*base]),
                        _ => (*expression, Vec::new()),
                    };
                    for index in selects {
                        if self.effectful(index) {
                            indices.push(index);
                        }
                    }
                    id = expression_binding::ungroup(base, native);
                }
                _ => break,
            }
        }
        (receivers, indices)
    }

    /// Evaluate `operands`, found outside in, left to right into hidden
    /// locals of their types, which every later binding of them reads.
    fn save_operands(&mut self, operands: Vec<ExprId>, stmts: &mut Vec<Stmt>) -> Converted<()> {
        let native = self.tree.declarations();
        for operand in operands.into_iter().rev() {
            let mut value =
                expression_binding::value(operand, native, self.tree, self.session, &self.dims)?;
            let (source, unique) = self.hidden_of(operand)?;
            self.rename_expr(&mut value);
            stmts.push(Stmt::Assign {
                lhs: LValue::Ident(unique),
                rhs: value,
                nonblocking: false,
            });
            self.bind_effect(operand, Expr::Ident(source));
        }
        Ok(())
    }

    /// `$cast(destination, source)` `id` to a class property or an element
    /// of one: the cast is to a hidden handle, which the destination takes
    /// only when the cast succeeds, as a failed cast leaves the destination
    /// unchanged (8.16). `id` binds to whether it succeeded. A cast that
    /// fails whatever the source's value only evaluates the source.
    fn cast_to_property(
        &mut self,
        id: ExprId,
        destination: ExprId,
        source: ExprId,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        let native = self.tree.declarations();
        // The destination is addressed as an assignment to it is: its
        // receivers before the source, its indices after; it is evaluated
        // whether or not the cast succeeds, but only written when it does.
        let (receivers, indices) = self.destination_operands(destination);
        self.save_operands(receivers, stmts)?;
        if let Some((_, _, domain)) = self.enum_cast(id)? {
            return self.cast_to_enum(id, destination, source, &domain, indices, stmts);
        }
        let (succeeded_source, succeeded) = self.hidden(int_type());
        let cast = expression_binding::object_cast_to(
            self.tree,
            self.session,
            destination,
            source,
            native,
            &self.dims,
        )?;
        let Some(expression_binding::ObjectCast::Call { name, .. }) = cast else {
            // The source is evaluated although the cast fails, into a
            // hidden handle of its class, which a failing cast knows.
            if self.effectful(source)
                && let Some(_) = expression_binding::object_native_class(
                    self.tree,
                    self.session,
                    source,
                    native,
                    &self.dims,
                )
            {
                let (_, handle) = self.hidden_of(source)?;
                let mut value =
                    expression_binding::value(source, native, self.tree, self.session, &self.dims)?;
                self.rename_expr(&mut value);
                stmts.push(Stmt::Assign {
                    lhs: LValue::Ident(handle),
                    rhs: value,
                    nonblocking: false,
                });
            }
            self.save_operands(indices, stmts)?;
            stmts.push(Stmt::Assign {
                lhs: LValue::Ident(succeeded),
                rhs: Expr::Literal("0".into()),
                nonblocking: false,
            });
            self.bind_effect(id, Expr::Ident(succeeded_source));
            return Ok(());
        };
        // The cast's subroutine writes a hidden handle of the destination's
        // type.
        let (handle_source, _) = self.hidden_of(destination)?;
        let value = expression_binding::value(source, native, self.tree, self.session, &self.dims)?;
        let mut call = Expr::Call {
            callee: Callee::Subroutine(name),
            args: vec![Some(Expr::Ident(handle_source.clone())), Some(value)],
            arg_order: Vec::new(),
        };
        self.rename_expr(&mut call);
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(succeeded),
            rhs: call,
            nonblocking: false,
        });
        self.save_operands(indices, stmts)?;
        // The destination is written as `destination = id`, with `id` bound
        // to the handle for the write.
        self.bind_effect(id, Expr::Ident(handle_source));
        let mut write = Vec::new();
        self.write_update(destination, id, &mut write)?;
        let mut condition = Expr::Ident(succeeded_source.clone());
        self.rename_expr(&mut condition);
        stmts.push(Stmt::If {
            condition,
            then_body: write,
            else_body: Vec::new(),
        });
        self.bind_effect(id, Expr::Ident(succeeded_source));
        Ok(())
    }

    /// What kind of value source `id` of a `$cast` to an enumerated type is,
    /// from its self-determined type (6.22.4, 6.24.2). A source whose type
    /// is not resolved is unsupported rather than read as a number.
    fn cast_source_kind(&mut self, id: ExprId) -> Converted<CastSource> {
        let native = self.tree.declarations();
        if expression_binding::native_chandle(self.tree, id, native, &self.dims, self.session) {
            return Ok(CastSource::Chandle);
        }
        let semantic = expression_types::type_of(self.tree, self.session, id, native, &self.dims)
            .ok()
            .map(|ty| self.session.types.data(ty).clone());
        match semantic {
            Some(semantic_types::TyData::Packed(packed)) => Ok(CastSource::Integral {
                width: usize::try_from(packed.bits())
                    .map_err(|_| unsupported("`$cast` source width"))?,
                signed: packed.signed,
            }),
            Some(semantic_types::TyData::Class(semantic_types::Decl::Class(name))) => {
                Ok(CastSource::Handle(Some(name.to_string())))
            }
            Some(
                semantic_types::TyData::Class(_)
                | semantic_types::TyData::VirtualInterface { .. }
                | semantic_types::TyData::UnpackedStruct(_)
                | semantic_types::TyData::Array { .. },
            ) => Ok(CastSource::Handle(None)),
            Some(semantic_types::TyData::Real | semantic_types::TyData::Shortreal) => {
                Err(unsupported("`$cast` of a real value to an enumerated type"))
            }
            Some(semantic_types::TyData::String) => {
                Err(unsupported("`$cast` of a string to an enumerated type"))
            }
            // `null` is a handle literal, no integral value (6.24.2).
            Some(semantic_types::TyData::Null) => Ok(CastSource::Null),
            None => Err(unsupported(
                "`$cast` source of an unresolved type to an enumerated type",
            )),
        }
    }

    /// `$cast(destination, source)` `id` to a variable of enumerated type
    /// `domain`: an integral source is assigned only when its value is that
    /// of a member, and a value of another type (a class handle, virtual
    /// interface, chandle, `null` or aggregate), which is not cast
    /// compatible with the type, never is; otherwise the destination is left
    /// unchanged (6.24.2). `id` binds to whether it was assigned.
    fn cast_to_enum(
        &mut self,
        id: ExprId,
        destination: ExprId,
        source: ExprId,
        domain: &enums::EnumDomain,
        indices: Vec<ExprId>,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        let native = self.tree.declarations();
        let (succeeded_source, succeeded) = self.hidden(int_type());
        // Whether the source is cast compatible follows from its type,
        // whatever form of expression gives it (6.22.4): only an integral
        // value can be one of the type's members.
        let kind = self.cast_source_kind(source)?;
        let (source_width, source_signed) = match kind {
            CastSource::Integral { width, signed } => (width, signed),
            kind => {
                // The source is evaluated although the cast fails, by the
                // binder of its own type: a call, also of a method named
                // without parentheses, still runs; a source without an effect
                // is not bound.
                match kind {
                    CastSource::Handle(Some(_)) => {
                        let mut value = expression_binding::value(
                            source,
                            native,
                            self.tree,
                            self.session,
                            &self.dims,
                        )?;
                        if inlining::effectful(&value) {
                            let (_, handle) = self.hidden_of(source)?;
                            self.rename_expr(&mut value);
                            stmts.push(Stmt::Assign {
                                lhs: LValue::Ident(handle),
                                rhs: value,
                                nonblocking: false,
                            });
                        }
                    }
                    CastSource::Chandle => {
                        let value = expression_binding::bind_chandle_value(
                            source,
                            native,
                            self.tree,
                            self.session,
                            &self.dims,
                            None,
                        )?;
                        if inlining::effectful(&value) {
                            return Err(unsupported(
                                "`$cast` of a chandle with a side effect to an enumerated type",
                            ));
                        }
                    }
                    CastSource::Handle(None) => {
                        if calls(native, source)
                            || expression_binding::value(
                                source,
                                native,
                                self.tree,
                                self.session,
                                &self.dims,
                            )
                            .is_ok_and(|value| inlining::effectful(&value))
                        {
                            return Err(unsupported(
                                "`$cast` of a value with a side effect that is not cast compatible with an enumerated type",
                            ));
                        }
                    }
                    CastSource::Null | CastSource::Integral { .. } => {}
                }
                self.save_operands(indices, stmts)?;
                stmts.push(Stmt::Assign {
                    lhs: LValue::Ident(succeeded),
                    rhs: Expr::Literal("0".into()),
                    nonblocking: false,
                });
                self.bind_effect(id, Expr::Ident(succeeded_source));
                return Ok(());
            }
        };
        let mut rhs =
            expression_binding::value(source, native, self.tree, self.session, &self.dims)?;
        let source_type = ExprType {
            width: source_width,
            signed: source_signed,
        };
        // The source keeps its own value until it is known to be a member's:
        // a local wide enough for every member's value (one more bit for a
        // signed source) and 4-state, so that unknown bits make the cast
        // invalid. The write converts it to the destination.
        let width = source_type
            .width
            .max(domain.width + usize::from(source_type.signed));
        let logic = |width: usize| {
            let mut ty = Type::new(TypeKind::Logic);
            ty.is_signed = source_type.signed;
            if width > 1 {
                ty.packed_ranges = vec![PackedRange::new(
                    ConstExpr::Literal((width - 1).to_string()),
                    ConstExpr::Literal("0".into()),
                )];
            }
            ty
        };
        // The source is evaluated at its self-determined width first, so
        // that the wider local does not size a context-determined source.
        let (own_source, own) = self.hidden_of(source)?;
        self.rename_expr(&mut rhs);
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(own),
            rhs,
            nonblocking: false,
        });
        let (value_source, value) = self.hidden(logic(width));
        let mut widened = Expr::Ident(own_source);
        self.rename_expr(&mut widened);
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(value),
            rhs: widened,
            nonblocking: false,
        });
        self.save_operands(indices, stmts)?;
        let mut test = expression_binding::enum_membership(
            domain,
            Expr::Ident(value_source.clone()),
            width,
            source_type.signed,
        );
        self.rename_expr(&mut test);
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(succeeded),
            rhs: test,
            nonblocking: false,
        });
        self.bind_effect(id, Expr::Ident(value_source));
        let mut write = Vec::new();
        self.write_update(destination, id, &mut write)?;
        let mut condition = Expr::Ident(succeeded_source.clone());
        self.rename_expr(&mut condition);
        stmts.push(Stmt::If {
            condition,
            then_body: write,
            else_body: Vec::new(),
        });
        self.bind_effect(id, Expr::Ident(succeeded_source));
        Ok(())
    }

    /// Stack the destination-typed value before updating the target (11.3.6).
    /// Reuse ordinary assignment checking and compound-operator sizing.
    fn assignment_expression(
        &mut self,
        id: ExprId,
        left: ExprId,
        operator: &str,
        right: ExprId,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        let native = self.tree.declarations();
        let current = expression_binding::value(left, native, self.tree, self.session, &self.dims)?;
        if inlining::effectful(&current) {
            return Err(unsupported(
                "assignment expression target selected with a side effect",
            ));
        }
        dimensions::bound_bits_type(self.tree, &current, &self.dims)
            .ok_or_else(|| unsupported("assignment expression target type"))?;
        let shape = dimensions::operand_shape(left, self.tree, self.session, &self.dims)?;
        if shape.as_ref().is_some_and(|shape| {
            !shape.unpacked.is_empty()
                || shape.container.is_some()
                || shape.unpacked_struct.is_some()
        }) {
            return Err(unsupported(
                "assignment expression to an unpacked aggregate",
            ));
        }
        if !matches!(current, Expr::Ident(_))
            && shape.as_ref().is_some_and(|shape| {
                shape.enum_domain.is_some()
                    || shape.class.is_some()
                    || shape.virtual_interface.is_some()
                    || shape.chandle
                    || !shape.members.is_empty()
            })
        {
            return Err(unsupported(
                "assignment expression to this selected target type",
            ));
        }
        let mut assignment = self.assignment_value(operator, false, (left, right))?;
        let rhs = match &mut assignment {
            Stmt::Assign { rhs, .. } | Stmt::AssignConcat { rhs, .. } => rhs,
            _ => return Err(unsupported("assignment expression to this target")),
        };
        // Index reads must name the same target when a compound value is read
        // and written. Calls in a selected target or RHS remain unsupported.
        if !matches!(current, Expr::Ident(_)) && inlining::effectful(rhs) {
            return Err(unsupported(
                "assignment expression with an effectful RHS and a selected target",
            ));
        }
        // The value is stacked in a local of the target's type.
        let (source, unique) = self.hidden_of(left)?;
        let value = std::mem::replace(rhs, Expr::Ident(unique.clone()));
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(unique),
            rhs: value,
            nonblocking: false,
        });
        stmts.push(assignment);
        self.bind_effect(id, Expr::Ident(source));
        Ok(())
    }

    /// `stmts`, run unless `decided` is `skip`.
    fn unless(decided: &str, skip: &str, stmts: Vec<Stmt>) -> Stmt {
        Stmt::If {
            condition: Expr::Binary {
                left: Box::new(Expr::Ident(decided.to_string())),
                op: BinaryOp::NeCase,
                right: Box::new(Expr::Literal(skip.into())),
            },
            then_body: stmts,
            else_body: Vec::new(),
        }
    }

    /// Declare a hidden local of type `ty` and return its source and
    /// unique names.
    pub(super) fn hidden(&mut self, ty: Type) -> (String, String) {
        // `%` cannot start or appear in an identifier, so the source name
        // shadows no declaration.
        let source = format!("update%{}", *self.state.counter);
        let unique = self.declare(&source, ty);
        (source, unique)
    }

    /// Declare a hidden local that saves the value of native expression
    /// `id`, of the expression's own type: a variable's declared type, or
    /// the self-determined semantic type of any other expression (a string,
    /// a class handle, a real or an integral vector), and return its source
    /// and unique names. Every saved operand is declared through this.
    pub(super) fn hidden_of(&mut self, id: ExprId) -> Converted<(String, String)> {
        let native = self.tree.declarations();
        if let Some(name) = expression_binding::native_name(self.tree, id, native)
            && let Some(ty) = self
                .dims
                .get(&name)
                .and_then(type_names::declared_type)
                .cloned()
        {
            return Ok(self.hidden(ty));
        }
        let semantic = expression_types::type_of(self.tree, self.session, id, native, &self.dims)?;
        let ty = match self.session.types.data(semantic).clone() {
            semantic_types::TyData::Packed(packed) => {
                let width = usize::try_from(packed.bits())
                    .map_err(|_| unsupported("a saved value of this width"))?;
                let mut ty = Type::new(if packed.four_state {
                    TypeKind::Logic
                } else {
                    TypeKind::Bit
                });
                ty.is_signed = packed.signed;
                if width > 1 {
                    ty.packed_ranges = vec![PackedRange::new(
                        ConstExpr::Literal((width - 1).to_string()),
                        ConstExpr::Literal("0".into()),
                    )];
                }
                ty
            }
            semantic_types::TyData::Real => {
                type_binding::builtin_type("real", None).expect("a builtin type")
            }
            semantic_types::TyData::Shortreal => {
                type_binding::builtin_type("shortreal", None).expect("a builtin type")
            }
            semantic_types::TyData::String => {
                type_binding::builtin_type("string", None).expect("a builtin type")
            }
            semantic_types::TyData::Class(semantic_types::Decl::Class(class)) => {
                classes::handle_type(self.tree, self.session, &class)
                    .ok_or_else(|| unsupported(format!("class `{class}`")))?
            }
            _ => return Err(unsupported("a saved value of this type")),
        };
        Ok(self.hidden(ty))
    }

    /// Evaluate the truth of `id` into a hidden 1-bit local, which `id`
    /// binds to from now on, and return its unique name.
    fn truth(&mut self, id: ExprId, stmts: &mut Vec<Stmt>) -> Converted<String> {
        let value = expression_binding::bind_chandle_value(
            id,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
            None,
        )?;
        let (source, unique) = self.hidden(Type::new(TypeKind::Logic));
        let truth = |value: Expr| Expr::Unary {
            op: UnaryOp::LogicNot,
            expr: Box::new(value),
        };
        let mut rhs = truth(truth(value));
        self.rename_expr(&mut rhs);
        stmts.push(Stmt::Assign {
            lhs: LValue::Ident(unique.clone()),
            rhs,
            nonblocking: false,
        });
        self.bind_effect(id, Expr::Ident(source));
        Ok(unique)
    }

    fn bind_effect(&mut self, id: ExprId, value: Expr) {
        Arc::make_mut(&mut self.dims.effect_values).insert(id, value);
    }

    /// Update `operand` of increment or decrement `id`. `postfix` is `None`
    /// for an operator statement, whose value is discarded; otherwise `id`
    /// binds to a hidden local that holds its value.
    pub(super) fn update(
        &mut self,
        id: ExprId,
        operand: ExprId,
        postfix: Option<bool>,
        increment: bool,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        let native = self.tree.declarations();
        self.reject_tagged_integral_target(operand)?;
        if unpacked_structs::native_struct(operand, native, self.tree, self.session, &self.dims)?
            .is_some()
        {
            return Err(unsupported(
                "increment or decrement of an unpacked structure",
            ));
        }
        // A virtual interface is no number to step (IEEE 1800-2023 25.9).
        if let Some(ty) =
            virtual_interfaces::native_type(self.tree, self.session, operand, native, &self.dims)
        {
            return Err(virtual_interfaces::arithmetic(
                if increment { "++" } else { "--" },
                &ty,
            ));
        }
        if expression_binding::native_string(self.tree, self.session, operand, native, &self.dims) {
            return Err(unsupported("increment or decrement of a string"));
        }
        let current =
            expression_binding::value(operand, native, self.tree, self.session, &self.dims)?;
        // The operand is read and then written: a select of it must not
        // change in between. A member read through a virtual interface checks
        // for `null`, which changes nothing; its handle is pure.
        if inlining::effectful(&current)
            && virtual_interfaces::native_member(
                self.tree,
                self.session,
                operand,
                native,
                &self.dims,
            )
            .is_none()
        {
            return Err(unsupported(
                "increment or decrement of an operand selected with a side effect",
            ));
        }
        let step = |value: Expr| Expr::Binary {
            left: Box::new(value),
            op: if increment {
                BinaryOp::Add
            } else {
                BinaryOp::Sub
            },
            right: Box::new(Expr::Literal("1".into())),
        };
        let Some(postfix) = postfix else {
            self.bind_effect(id, step(current));
            self.write_update(operand, id, stmts)?;
            return Ok(());
        };
        // The local is of the operand's type, so a prefix result is the value
        // the update stores.
        let (source, unique) = self.hidden_of(operand)?;
        let local = Expr::Ident(source);
        let read = |this: &mut Self, value: Expr, stmts: &mut Vec<Stmt>| {
            let mut rhs = value;
            this.rename_expr(&mut rhs);
            stmts.push(Stmt::Assign {
                lhs: LValue::Ident(unique.clone()),
                rhs,
                nonblocking: false,
            });
        };
        // The local holds the operand's value before or after the update. The
        // operand is selected once more, by the write, before the update.
        if postfix {
            read(self, current, stmts);
            self.bind_effect(id, step(local.clone()));
        } else {
            read(self, step(current), stmts);
            self.bind_effect(id, local.clone());
        }
        self.write_update(operand, id, stmts)?;
        self.bind_effect(id, local);
        Ok(())
    }

    /// Write the value that `id` binds into `operand`.
    fn write_update(
        &mut self,
        operand: ExprId,
        id: ExprId,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        // A select of a class property is written through a local.
        if let Some(write) = self.property_select_assignment("=", false, operand, id)? {
            stmts.extend(write);
        } else {
            stmts.push(self.assignment_value("=", false, (operand, id))?);
        }
        Ok(())
    }

    /// The statements of a loop whose condition `id` has increment or
    /// decrement operators, which run every time it is evaluated: they
    /// start each iteration, which ends the loop when the condition does not
    /// hold. `None` when the condition has no operator.
    pub(super) fn loop_condition(&mut self, id: ExprId) -> Converted<Option<Vec<Stmt>>> {
        let mut stmts = self.effects(&[id])?;
        if stmts.is_empty() {
            return Ok(None);
        }
        stmts.push(Stmt::If {
            condition: self.native_chandle_value(id)?,
            then_body: Vec::new(),
            else_body: vec![Stmt::Break],
        });
        Ok(Some(stmts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_expressions_keep_context_and_target_checks() {
        for body in [
            "int i; wire y; assign y=(i=1);",
            "int i; always @((i=1)) i=2;",
            "int i; initial begin static int t=(i=1); end",
            "int a[4],i,d; function int f(); i++; return i; endfunction initial d=(a[f()]+=1);",
            "int a[4],i,d; function int f(); i++; return i; endfunction initial d=(a[i]+=f());",
            "int d; initial d=(5=1);",
            "string s; initial s=(s+=1);",
        ] {
            let source = format!("module M; {body} endmodule");
            assert!(
                crate::analyze_source(&source, Path::new("assignment-expression-context.sv"))
                    .is_err(),
                "{body}"
            );
        }
    }

    #[test]
    fn operators_in_contexts_that_skip_or_repeat_operands_are_rejected() {
        for (body, diagnostic) in [
            (
                "int q[$], r[$], i; initial r = q.find with (item == i++);",
                "increment or decrement expression in this context",
            ),
            (
                "int i; initial wait (i++ > 0);",
                "increment or decrement expression",
            ),
            (
                "int i; initial begin static int t = i++; end",
                "increment or decrement in a static variable initializer",
            ),
            (
                "int a[4], i; initial case (i) i++: a[0] = 1; endcase",
                "increment or decrement expression",
            ),
            (
                "int a[4], n; function int f(); n++; return n; endfunction initial a[f()]++;",
                "increment or decrement of an operand selected with a side effect",
            ),
            (
                "int i, n; initial n = $bits(i++);",
                "increment or decrement expression",
            ),
            (
                "string s; initial s++;",
                "increment or decrement of a string",
            ),
        ] {
            let source = format!("module M; {body} endmodule");
            let error =
                crate::analyze_source(&source, Path::new("increment-context.sv")).expect_err(body);
            assert!(
                matches!(&error, AnalyzerError::Unsupported(construct) if construct.contains(diagnostic)),
                "{body}: {error}"
            );
        }
    }
}
