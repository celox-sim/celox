//! Subroutine, method and system task calls as statements.
use super::*;

impl BodyBuilder<'_, '_, '_> {
    /// A call of a class method as a statement, `h.m(args)`, `h.m`, or an
    /// unqualified `m(args)` inside a method of its class (IEEE 1800-2023
    /// 8.10).
    pub(super) fn method_call_statement(
        &mut self,
        root: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        if let Some(mut call) =
            processes::value(self.tree, self.session, root, native, &self.dims, true)?
        {
            self.rename_expr(&mut call);
            return Ok(Some(Stmt::Eval(call)));
        }
        if let Some(mut call) =
            semaphores::value(self.tree, self.session, root, native, &self.dims, true)?
        {
            self.rename_expr(&mut call);
            let stmt = Stmt::Eval(call);
            if events::blocks(&stmt) {
                self.allow_timing("semaphore get")?;
            }
            return Ok(Some(stmt));
        }
        if let Some(mut call) =
            mailboxes::value(self.tree, self.session, root, native, &self.dims, true)?
        {
            self.rename_expr(&mut call);
            let stmt = Stmt::Eval(call);
            if events::blocks(&stmt) {
                self.allow_timing("mailbox put, get or peek")?;
            }
            return Ok(Some(stmt));
        }
        // The rand_mode of a static rand property is one bit every object
        // shares (IEEE 1800-2023 18.8).
        if let Some((off, Some(argument))) = expression_binding::object_static_rand_mode(
            self.tree,
            self.session,
            root,
            native,
            &self.dims,
        )? {
            let mut enabled = self.native_value(argument)?;
            self.rename_expr(&mut enabled);
            let mut lhs = LValue::Ident(off);
            self.rename_lvalue(&mut lhs);
            return Ok(Some(Stmt::Assign {
                lhs,
                rhs: Expr::Unary {
                    op: UnaryOp::LogicNot,
                    expr: Box::new(enabled),
                },
                nonblocking: false,
            }));
        }
        if let Some(mut call) = expression_binding::object_constraint_mode_call(
            self.tree,
            self.session,
            root,
            native,
            &self.dims,
            true,
        )?
        .or(expression_binding::object_rand_mode_call(
            self.tree,
            self.session,
            root,
            native,
            &self.dims,
            true,
        )?) {
            self.rename_expr(&mut call);
            return Ok(Some(match call {
                Expr::Call {
                    callee: Callee::Subroutine(name),
                    args,
                    arg_order,
                } => Stmt::Call {
                    name,
                    args,
                    arg_order,
                },
                call => Stmt::Eval(call),
            }));
        }
        let call = match native.expression(root).kind {
            celox_sv_syntax::ExprKind::Call { .. } => {
                if expression_binding::object_call_method(
                    self.tree,
                    self.session,
                    root,
                    native,
                    &self.dims,
                )?
                .is_none()
                {
                    return Ok(None);
                }
                // The result of a call statement is discarded, which a
                // chandle result may be (IEEE 1800-2023 6.14).
                expression_binding::bind_chandle_value(
                    root,
                    native,
                    self.tree,
                    self.session,
                    &self.dims,
                    None,
                )?
            }
            _ => {
                let Some((name, formals, receiver)) = expression_binding::object_method(
                    self.tree,
                    self.session,
                    root,
                    native,
                    &self.dims,
                )?
                else {
                    return Ok(None);
                };
                let mut args = match receiver {
                    expression_binding::ObjectReceiver::Handle(receiver) => {
                        vec![Some(expression_binding::value(
                            receiver,
                            native,
                            self.tree,
                            self.session,
                            &self.dims,
                        )?)]
                    }
                    expression_binding::ObjectReceiver::This => {
                        vec![Some(expression_binding::object_this())]
                    }
                    expression_binding::ObjectReceiver::Static => Vec::new(),
                };
                if name.ends_with("::randomize") {
                    args.push(Some(Expr::Literal("1'b0".into())));
                }
                if name.ends_with("::srandom") {
                    return Err(unsupported("srandom requires a seed argument"));
                }
                args.resize(
                    formals.len()
                        + usize::from(receiver != expression_binding::ObjectReceiver::Static),
                    None,
                );
                Expr::Call {
                    callee: Callee::Subroutine(name),
                    args,
                    arg_order: Vec::new(),
                }
            }
        };
        let mut call = call;
        self.rename_expr(&mut call);
        let Expr::Call {
            callee: Callee::Subroutine(name),
            args,
            arg_order,
        } = call
        else {
            return Err(unsupported("method call statement"));
        };
        Ok(Some(Stmt::Call {
            name,
            args,
            arg_order,
        }))
    }

    /// `string = op(string, args)`: a method that updates the string it is
    /// called on.
    pub(super) fn string_update(
        &mut self,
        string: celox_sv_syntax::ExprId,
        op: StringOp,
        args: Vec<Expr>,
        nonblocking: bool,
    ) -> Result<Stmt, AnalyzerError> {
        let native = self.tree.declarations();
        if let Some((property, receiver)) = expression_binding::object_property(
            self.tree,
            self.session,
            string,
            native,
            &self.dims,
        )? {
            expression_binding::check_writable(&property, &self.dims)?;
            if nonblocking {
                return Err(unsupported("nonblocking assignment to a class property"));
            }
            let handle = expression_binding::object_receiver(
                self.tree,
                self.session,
                receiver,
                native,
                &self.dims,
            )?;
            let receiver = (op == StringOp::Putc).then(|| {
                classes::object_call(
                    system_functions::ObjectOp::Load(property.field),
                    vec![handle.clone()],
                )
            });
            let value =
                expression_binding::string_call(op, receiver.into_iter().chain(args).collect());
            let mut store = classes::object_call(
                system_functions::ObjectOp::Store(property.field),
                vec![handle, value],
            );
            self.rename_expr(&mut store);
            return Ok(Stmt::Eval(store));
        }
        if expression_binding::fixed_string_array_element(
            self.tree,
            self.session,
            string,
            native,
            &self.dims,
        ) {
            if nonblocking {
                return Err(unsupported(
                    "nonblocking assignment to a string array character",
                ));
            }
            // Capture authored indices before flattening repeats them in bounds
            // and range guards. The method's arguments may change those indices.
            let mut indices = Vec::new();
            let mut base = string;
            while let celox_sv_syntax::ExprKind::Select {
                expression,
                dimension,
            } = &native
                .expression(expression_binding::ungroup(base, native))
                .kind
            {
                let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
                    return Err(unsupported("string mutation of a subarray"));
                };
                indices.push(index);
                base = *expression;
            }
            let saved = self.dims.effect_values.clone();
            let mut body = Vec::new();
            for index in indices.into_iter().rev() {
                let mut value =
                    expression_binding::value(index, native, self.tree, self.session, &self.dims)?;
                let ty = dimensions::bound_bits_type(self.tree, &value, &self.dims)
                    .ok_or_else(|| unsupported("string array index type"))?;
                let (source, unique) = self.hidden(vector_type(ty.width, ty.signed));
                self.rename_expr(&mut value);
                body.push(Stmt::Assign {
                    lhs: LValue::Ident(unique),
                    rhs: value,
                    nonblocking: false,
                });
                Arc::make_mut(&mut self.dims.effect_values).insert(index, Expr::Ident(source));
            }
            let target = self.native_lvalue(string, false);
            self.dims.effect_values = saved;
            let (written, _) = target?;
            let receiver = (op == StringOp::Putc).then(|| {
                expression_binding::string_call(
                    StringOp::FromHandle,
                    vec![expr_from_lvalue(self.tree, &written, &self.dims)],
                )
            });
            let rhs =
                expression_binding::string_call(op, receiver.into_iter().chain(args).collect());
            body.extend(self.emit_assign(written, rhs, nonblocking)?);
            return Ok(Stmt::If {
                condition: Expr::Literal("1'b1".into()),
                then_body: body,
                else_body: Vec::new(),
            });
        }
        let (written, _) = self.native_lvalue(string, false)?;
        let LValue::Ident(name) = &written else {
            return Err(unsupported(format!(
                "string method `{op}` on an array element"
            )));
        };
        let receiver = (op == StringOp::Putc).then(|| Expr::Ident(name.clone()));
        let rhs = expression_binding::string_call(op, receiver.into_iter().chain(args).collect());
        let mut stmts = self.emit_assign(written, rhs, nonblocking)?;
        Ok(match stmts.as_slice() {
            [_] => stmts.remove(0),
            _ => Stmt::If {
                condition: Expr::Literal("1'b1".into()),
                then_body: stmts,
                else_body: Vec::new(),
            },
        })
    }

    pub(super) fn subroutine_call_statement(
        &mut self,
        root: celox_sv_syntax::ExprId,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        if let Some((mut value, _)) =
            array_reductions::bind(root, native, self.tree, self.session, &self.dims)?
        {
            self.rename_expr(&mut value);
            return Ok(vec![Stmt::Eval(value)]);
        }
        if let Some(mut value) =
            containers::value(root, native, self.tree, self.session, &self.dims, true)?
        {
            self.rename_expr(&mut value);
            return Ok(vec![Stmt::Eval(value)]);
        }
        if let celox_sv_syntax::ExprKind::Call {
            expression,
            arguments,
        } = &native
            .expression(expression_binding::ungroup(root, native))
            .kind
            && let Some((op, string)) = expression_binding::string_method(
                self.tree,
                self.session,
                *expression,
                native,
                &self.dims,
            )
        {
            let (_, count) = StringOp::method(&op.to_string()).expect("a method name");
            let args = arguments
                .iter()
                .map(|arg| arg.value.filter(|_| arg.name.is_none()))
                .collect::<Option<Vec<_>>>()
                .filter(|args| args.len() == count)
                .ok_or_else(|| unsupported(format!("arguments of string method `{op}`")))?;
            let args = args
                .into_iter()
                .map(|arg| {
                    expression_binding::value(arg, native, self.tree, self.session, &self.dims)
                })
                .collect::<Result<Vec<_>, _>>()?;
            // A method that returns a value has no effect as a statement.
            return if op.updates_receiver() {
                Ok(vec![self.string_update(string, op, args, false)?])
            } else {
                Ok(Vec::new())
            };
        }
        let discarded = matches!(
            self.tree.declarations().expression(root).kind,
            celox_sv_syntax::ExprKind::Cast { .. }
        );
        {
            let query_root =
                expression_binding::call_statement_root(root, discarded, self.tree.declarations())?;
            if discarded
                && let Some((mut value, _)) =
                    array_reductions::bind(query_root, native, self.tree, self.session, &self.dims)?
            {
                self.rename_expr(&mut value);
                return Ok(vec![Stmt::Eval(value)]);
            }
            if discarded
                && let Some(mut value) = containers::value(
                    query_root,
                    native,
                    self.tree,
                    self.session,
                    &self.dims,
                    true,
                )?
            {
                self.rename_expr(&mut value);
                return Ok(vec![Stmt::Eval(value)]);
            }
            if matches!(
                native.expression(query_root).kind,
                celox_sv_syntax::ExprKind::RandomizeWith { .. }
            ) {
                let mut call = self.native_value(query_root)?;
                self.rename_expr(&mut call);
                return Ok(vec![Stmt::Eval(call)]);
            }
            if let Some(call) = self.method_call_statement(query_root)? {
                return Ok(vec![call]);
            }
            if let celox_sv_syntax::ExprKind::Call { expression, .. } =
                self.tree.declarations().expression(query_root).kind
                && matches!(
                    expression_binding::native_name(
                        self.tree,
                        expression,
                        self.tree.declarations(),
                    )
                    .as_deref(),
                    Some("randomize" | "std::randomize")
                )
            {
                let mut call = self.native_value(query_root)?;
                self.rename_expr(&mut call);
                return Ok(vec![Stmt::Eval(call)]);
            }
            if timescales::is_query(query_root, self.tree.declarations()) {
                let value = timescales::query(query_root, self.tree)?;
                return Ok(vec![Stmt::Eval(Expr::Literal(
                    parameters::format_typed_parameter_literal(i128::from(value), 32, true),
                ))]);
            }
            let signatures = if self.dims.subroutine_param_names.is_empty() {
                self.state.subroutine_params
            } else {
                &self.dims.subroutine_param_names
            };
            if let Some(mut call) = expression_binding::call_statement(
                root,
                discarded,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
                signatures,
            )? {
                self.rename_expr(&mut call);
                let Expr::Call {
                    callee: Callee::Subroutine(name),
                    args,
                    arg_order,
                } = call
                else {
                    unreachable!("ordinary native statement call")
                };
                return Ok(vec![Stmt::Call {
                    name,
                    args,
                    arg_order,
                }]);
            }
        }
        let root =
            expression_binding::call_statement_root(root, discarded, self.tree.declarations())?;
        let declarations = self.tree.declarations();
        let (callee, arguments) = match &declarations.expression(root).kind {
            celox_sv_syntax::ExprKind::Call {
                expression,
                arguments,
            } => (*expression, arguments.as_slice()),
            celox_sv_syntax::ExprKind::Name(_) => (root, &[][..]),
            _ => return Err(unsupported("system task or function name")),
        };
        let celox_sv_syntax::ExprKind::Name(name) = &declarations.expression(callee).kind else {
            return Err(unsupported("system task or function name"));
        };
        let presence = arguments
            .iter()
            .map(|arg| {
                if arg.value.is_some() {
                    system_functions::Arg::Given
                } else {
                    system_functions::Arg::Omitted
                }
            })
            .collect::<Vec<_>>();
        let args = (!arguments.iter().any(|arg| arg.name.is_some())).then_some(presence);
        let name = name.spelling.as_str();
        // `$cast` as a task reports a run-time error when the cast fails
        // (IEEE 1800-2023 6.24.2); the result of a `void'` cast is discarded.
        if name == "$cast"
            && let [destination, source] = arguments
            && let (Some(destination), Some(_), None, None) = (
                destination.value,
                source.value,
                &destination.name,
                &source.name,
            )
            && expression_binding::enum_domain(
                self.tree,
                self.session,
                destination,
                declarations,
                &self.dims,
            )?
            .is_some()
        {
            // The effects of the statement made the cast; `root` is bound
            // to whether it was valid.
            if discarded {
                return Ok(Vec::new());
            }
            return Ok(vec![Stmt::If {
                condition: self.native_value(root)?,
                then_body: Vec::new(),
                else_body: vec![Stmt::SystemTask {
                    task: SystemTask::Severity(system_functions::Severity::Error),
                    site: source_span(declarations.expression(root).tokens, self.tree),
                    assertion: None,
                    args: vec![SystemTaskArg::Str(
                        "$cast of a value that is no member of the enumerated type".into(),
                    )],
                }],
            }]);
        }
        // The effects of the statement made the cast, and `root` is bound to
        // whether it was valid (6.24.2): an assignment or a pack always is.
        if name == "$cast"
            && let Some((_, _, plan)) = self.cast_plan(root)?
            && !matches!(plan, effects::CastPlan::Object)
        {
            if let effects::CastPlan::Unsupported(reason) = plan {
                return Err(unsupported(reason));
            }
            if discarded || matches!(plan, effects::CastPlan::Assign | effects::CastPlan::Pack) {
                return Ok(Vec::new());
            }
            return Ok(vec![Stmt::If {
                condition: self.native_value(root)?,
                then_body: Vec::new(),
                else_body: vec![Stmt::SystemTask {
                    task: SystemTask::Severity(system_functions::Severity::Error),
                    site: source_span(declarations.expression(root).tokens, self.tree),
                    assertion: None,
                    args: vec![SystemTaskArg::Str(
                        "$cast of a value that is not cast compatible with its destination".into(),
                    )],
                }],
            }]);
        }
        if name == "$cast"
            && let [destination, source] = arguments
            && let (Some(destination), Some(source), None, None) = (
                destination.value,
                source.value,
                &destination.name,
                &source.name,
            )
            && let Some(cast) = expression_binding::object_cast_to(
                self.tree,
                self.session,
                destination,
                source,
                declarations,
                &self.dims,
            )?
        {
            if discarded {
                if let expression_binding::ObjectCast::Fails = cast {
                    return Ok(Vec::new());
                }
                return Ok(vec![Stmt::Eval(self.native_value(root)?)]);
            }
            let error = Stmt::SystemTask {
                task: SystemTask::Severity(system_functions::Severity::Error),
                site: source_span(declarations.expression(root).tokens, self.tree),
                assertion: None,
                args: vec![SystemTaskArg::Str(
                    "$cast of a class handle to an incompatible class".into(),
                )],
            };
            if let expression_binding::ObjectCast::Fails = cast {
                return Ok(vec![error]);
            }
            let mut succeeded = self.native_value(root)?;
            self.rename_expr(&mut succeeded);
            return Ok(vec![Stmt::If {
                condition: succeeded,
                then_body: Vec::new(),
                else_body: vec![error],
            }]);
        }
        // `$sformat(output, format, args...)` assigns what `$sformatf(format,
        // args...)` returns (IEEE 1800-2023 21.3.3).
        if name == "$sformat" {
            if arguments.iter().any(|arg| arg.name.is_some()) {
                return Err(unsupported("named arguments of `$sformat`"));
            }
            let output = arguments.first().and_then(|arg| arg.value).ok_or_else(|| {
                AnalyzerError::InvalidSystemTfCall {
                    name: name.into(),
                    detail: "expected an output argument".into(),
                }
            })?;
            let ids = expression_binding::format_arguments(name, &arguments[1..])?;
            let mut values = Vec::new();
            for &id in &ids {
                values.push(expression_binding::bind_value(
                    id,
                    declarations,
                    self.tree,
                    self.session,
                    &self.dims,
                    None,
                )?);
            }
            let value = expression_binding::format_call(
                timescales::time_divisor(root, self.tree)?,
                &ids,
                values,
                self.tree,
                self.session,
                declarations,
                &self.dims,
            )?;
            return self.format_assignment(output, value);
        }
        // `$swrite(output, args...)` assigns what `$write(args...)` would
        // print, in the radix of its name (IEEE 1800-2023 21.3.3).
        if let Some(radix) = swrite_radix(name) {
            if arguments.iter().any(|arg| arg.name.is_some()) {
                return Err(unsupported(format!("named arguments of `{name}`")));
            }
            let output = arguments.first().and_then(|arg| arg.value).ok_or_else(|| {
                AnalyzerError::InvalidSystemTfCall {
                    name: name.into(),
                    detail: "expected an output argument".into(),
                }
            })?;
            // As for `$write`, a string literal that no conversion consumes
            // continues the format, and a value without a conversion of its
            // own is written in the radix of the task (21.2.1.1).
            let mut text = Vec::new();
            let mut ids = Vec::new();
            let mut values = Vec::new();
            let mut pending = 0;
            for (position, arg) in arguments.iter().enumerate().skip(1) {
                let id = arg
                    .value
                    .ok_or_else(|| AnalyzerError::InvalidSystemTfCall {
                        name: name.into(),
                        detail: format!("argument {} is omitted", position + 1),
                    })?;
                let literal = match &declarations.expression(id).kind {
                    celox_sv_syntax::ExprKind::Literal(text) => {
                        expression_binding::string_literal_bytes(text)
                    }
                    _ => None,
                };
                if pending == 0
                    && let Some(literal) = literal
                {
                    pending = format_conversions(&String::from_utf8_lossy(&literal)).len();
                    text.extend_from_slice(&literal);
                    continue;
                }
                let value = expression_binding::bind_value(
                    id,
                    declarations,
                    self.tree,
                    self.session,
                    &self.dims,
                    None,
                )?;
                if pending > 0 {
                    pending -= 1;
                } else {
                    text.extend_from_slice(
                        default_conversion(
                            literal.is_some()
                                || expression_binding::string_value(&value, &self.dims),
                            expression_binding::real_value(&value, &self.dims),
                            radix,
                        )
                        .as_bytes(),
                    );
                }
                ids.push(id);
                values.push(value);
            }
            text = expression_binding::scoped_template(&text, self.dims.scope_name.as_deref());
            if expression_binding::has_pattern(&text) {
                text = expression_binding::rewrite_format(
                    &text,
                    &ids,
                    &mut values,
                    self.tree,
                    self.session,
                    declarations,
                    &self.dims,
                )?;
            }
            let format = expression_binding::bytes_literal(&text);
            let value = expression_binding::string_call(
                StringOp::Format {
                    ticks_per_unit: timescales::time_divisor(root, self.tree)?,
                },
                std::iter::once(format).chain(values).collect(),
            );
            return self.format_assignment(output, value);
        }
        if name == "$sformatf" {
            let mut value = expression_binding::bind_value(
                root,
                declarations,
                self.tree,
                self.session,
                &self.dims,
                None,
            )?;
            self.rename_expr(&mut value);
            return Ok(vec![Stmt::Eval(value)]);
        }
        let task = match system_functions::check_call(
            name,
            args.as_deref(),
            system_functions::CallSite::Statement(self.body),
        )? {
            system_functions::SystemCall::Function(_) => {
                // A system function called as a statement is
                // converted and checked like the same call in an
                // expression, then its value is discarded. `$bits`
                // and `$size` become constants and do not evaluate
                // their operands (IEEE 1800-2023 20.6.2).
                let mut value = expression_binding::bind_value(
                    root,
                    declarations,
                    self.tree,
                    self.session,
                    &self.dims,
                    None,
                )?;
                self.rename_expr(&mut value);
                return Ok(vec![Stmt::Eval(value)]);
            }
            system_functions::SystemCall::Task(SystemTask::Printtimescale) => {
                return Ok(vec![Stmt::SystemTask {
                    task: SystemTask::Display(system_functions::Radix::Decimal),
                    site: source_span(declarations.expression(root).tokens, self.tree),
                    assertion: None,
                    args: vec![SystemTaskArg::Str(timescales::print_message(
                        root,
                        self.tree,
                        system_functions::CallSite::Statement(self.body),
                    )?)],
                }]);
            }
            // The coverage database tasks name a database Celox does not
            // keep: with no coverage data they have no effect (40.3.2.7)
            // but that of evaluating the name.
            system_functions::SystemCall::Task(
                SystemTask::SetCoverageDbName | SystemTask::LoadCoverageDb,
            ) => {
                // A constant function ignores its system task calls
                // (13.4.3).
                if self.dims.constant_function_bodies {
                    return Ok(Vec::new());
                }
                let mut stmts = Vec::new();
                for id in arguments.iter().filter_map(|arg| arg.value) {
                    if matches!(
                        declarations.expression(id).kind,
                        celox_sv_syntax::ExprKind::Literal(_)
                    ) {
                        continue;
                    }
                    let mut value = expression_binding::bind_value(
                        id,
                        declarations,
                        self.tree,
                        self.session,
                        &self.dims,
                        None,
                    )?;
                    self.rename_expr(&mut value);
                    stmts.push(Stmt::Eval(value));
                }
                return Ok(stmts);
            }
            system_functions::SystemCall::Task(task) => task,
        };
        if matches!(task, SystemTask::Readmem(_) | SystemTask::Writemem(_))
            && let Some(memory) = arguments.get(1).and_then(|arg| arg.value)
        {
            expression_binding::check_chandle_memory(memory, self.tree, &self.dims, self.session)?;
        }
        if matches!(
            task,
            SystemTask::Strobe(_)
                | SystemTask::Fstrobe(_)
                | SystemTask::Monitor(_)
                | SystemTask::Fmonitor(_)
        ) && effects::has_updates(
            declarations,
            &arguments
                .iter()
                .filter_map(|arg| arg.value)
                .collect::<Vec<_>>(),
        ) {
            return Err(unsupported(format!(
                "`{task}` with an assignment, increment or decrement argument"
            )));
        }
        let args = arguments
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                let Some(id) = arg.value else {
                    return Ok(SystemTaskArg::Empty);
                };
                if matches!(task, SystemTask::Vcd(system_functions::VcdTask::Vars)) && index > 0 {
                    let text = dump_selection_name(
                        id,
                        self.tree,
                        self.session,
                        &self.dims,
                        &|value| self.names.rename_const(value),
                        &|name| {
                            self.names
                                .scopes
                                .iter()
                                .any(|scope| scope.entries.iter().any(|entry| entry.source == name))
                        },
                    )?;
                    return Ok(SystemTaskArg::Str(text));
                }
                // Filenames use the normal packed-string expression binder,
                // including octal/hex escapes and multiline literal delimiters.
                if !matches!(task, SystemTask::Vcd(system_functions::VcdTask::File))
                    && let celox_sv_syntax::ExprKind::Literal(text) =
                        &declarations.expression(id).kind
                    && let Some(text) = text
                        .strip_prefix('"')
                        .and_then(|text| text.strip_suffix('"'))
                {
                    return Ok(SystemTaskArg::Str(text.to_string()));
                }
                // Renamed below, once the types of the source names have
                // chosen the conversions of the format.
                let value = expression_binding::bind_value(
                    id,
                    declarations,
                    self.tree,
                    self.session,
                    &self.dims,
                    None,
                )?;
                Ok(SystemTaskArg::Expr(value))
            })
            .collect::<Result<Vec<_>, AnalyzerError>>()?;
        let mut args = args;
        // The native expression of each argument, kept in step with `args`.
        let mut ids = arguments.iter().map(|arg| arg.value).collect::<Vec<_>>();
        self.merge_display_formats(task, &mut args, &mut ids);
        self.scope_display_format(task, &mut args);
        self.rewrite_display_patterns(task, &mut args, &ids)?;
        for arg in &mut args {
            if let SystemTaskArg::Expr(value) = arg {
                self.rename_expr(value);
            }
        }
        // A string literal that %t converts is bound as a value: the binder
        // handles every literal form, triple quotes and escapes included.
        let divisor = timescales::time_divisor(root, self.tree)?;
        let mut bind = |index: usize| -> Result<Option<Expr>, AnalyzerError> {
            let Some(id) = ids.get(index).copied().flatten() else {
                return Ok(None);
            };
            let mut value = expression_binding::bind_value(
                id,
                declarations,
                self.tree,
                self.session,
                &self.dims,
                None,
            )?;
            self.names.rename_expr(&mut value);
            Ok(Some(value))
        };
        scale_time_conversions(task, &mut args, divisor, &mut bind)?;
        if matches!(task, SystemTask::Timeformat) {
            // A constant units number is a time unit, from 2 to -15; the
            // precision and minimum field width are not negative (IEEE
            // 1800-2023 20.4.3).
            for (index, name, range) in [
                (0, "units number", -15..=2),
                (1, "precision number", 0..=i128::MAX),
                (3, "minimum field width", 0..=i128::MAX),
            ] {
                let Some(id) = arguments.get(index).and_then(|arg| arg.value) else {
                    continue;
                };
                let value = constant_binding::bind(id, self.tree, self.session, &self.dims)
                    .ok()
                    .flatten()
                    .and_then(|value| {
                        eval_ast_const_expr(&self.tree.functions, &value, &self.dims.const_env)
                    });
                if let Some(value) = value.filter(|value| !range.contains(value)) {
                    return Err(AnalyzerError::InvalidSystemTfCall {
                        name: "$timeformat".into(),
                        detail: format!(
                            "{name} {value} is outside {}..{} (IEEE 1800-2023 20.4.3)",
                            range.start(),
                            if *range.end() == i128::MAX {
                                String::new()
                            } else {
                                range.end().to_string()
                            }
                        ),
                    });
                }
            }
            args = timeformat_arguments(args, timescales::tick_exponent(root, self.tree)?)?;
        }
        // Keep Veryl's assertion calls as calls: their message arguments
        // execute even on success, unlike an SV assertion's fail action.
        Ok(vec![Stmt::SystemTask {
            task,
            args,
            site: source_span(declarations.expression(root).tokens, self.tree),
            assertion: None,
        }])
    }
}

pub(super) fn dump_selection_name(
    id: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
    rename: &dyn Fn(&mut ConstExpr),
    is_local: &dyn Fn(&str) -> bool,
) -> Result<String, AnalyzerError> {
    // Preserve identifier/index boundaries in host metadata. A normalized
    // escaped identifier can itself contain '.', '::', or '[0]'. NUL cannot
    // occur in an authored identifier, so it separates these components.
    fn append(
        id: celox_sv_syntax::ExprId,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        dimensions: &PackedDimensions,
        rename: &dyn Fn(&mut ConstExpr),
        kind: &mut &'static str,
        fields: &mut Vec<String>,
    ) -> Result<(), AnalyzerError> {
        use celox_sv_syntax::{DimensionKind, ExprKind};
        match &tree.declarations().expression(id).kind {
            ExprKind::Name(name) if name.spelling == "$root" => {
                *kind = "absolute";
            }
            ExprKind::Name(name) => {
                fields.extend([
                    "name".into(),
                    instances::normalize_identifier(&name.spelling),
                ]);
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." || separator == "::" => {
                append(*expression, tree, session, dimensions, rename, kind, fields)?;
                if separator == "::" {
                    *kind = "package";
                }
                fields.extend([
                    "name".into(),
                    instances::normalize_identifier(&name.spelling),
                ]);
            }
            ExprKind::Select {
                expression,
                dimension,
            } => {
                let DimensionKind::Index(index) = &dimension.kind else {
                    return Err(unsupported(
                        "$dumpvars hierarchy selection requires a single constant index",
                    ));
                };
                append(*expression, tree, session, dimensions, rename, kind, fields)?;
                let mut value = constant_binding::bind(*index, tree, session, dimensions)?
                    .ok_or_else(|| unsupported("$dumpvars hierarchy index must be constant"))?;
                rename(&mut value);
                let index = eval_ast_const_expr(&tree.functions, &value, &dimensions.const_env)
                    .ok_or_else(|| {
                        unsupported("$dumpvars hierarchy index must be a known constant integer")
                    })?;
                fields.extend(["index".into(), index.to_string()]);
            }
            _ => {
                return Err(unsupported(
                    "$dumpvars selection must be a module or variable name",
                ));
            }
        }
        Ok(())
    }
    let mut kind = "relative";
    let mut fields = Vec::new();
    append(
        id,
        tree,
        session,
        dimensions,
        rename,
        &mut kind,
        &mut fields,
    )?;
    // Imports bind declarations, not a module-local spelling. Nested procedural
    // declarations shadow these aliases just as they do for value expressions.
    if kind == "relative" && fields.len() == 2 && !is_local(&fields[1]) {
        let symbols = scope::imported(tree);
        if let Some(target) = symbols.aliases.get(&fields[1])
            && let Some((package, name)) = symbols
                .state_signals
                .iter()
                .find(|signal| signal.name() == target)
                .and_then(|signal| signal.package_variable())
        {
            kind = "package";
            fields = vec!["name".into(), package.into(), "name".into(), name.into()];
        } else if let Ok(unit) = tree.root_unit()
            && unit.kind == celox_sv_syntax::UnitKind::Package
            && packages::package_variables(&unit.name.spelling, tree)?.contains(&fields[1])
        {
            // A package subroutine is later instantiated at its caller, but
            // its unqualified package references retain their lexical owner.
            kind = "package";
            fields = vec![
                "name".into(),
                instances::normalize_identifier(&unit.name.spelling),
                "name".into(),
                fields[1].clone(),
            ];
        }
    }
    if fields.is_empty() {
        return Err(unsupported(
            "$dumpvars selection must name a module or variable",
        ));
    }
    // A lexical declaration wins over a root module with the same spelling.
    // Keep that decision in metadata: the runtime cannot recover lexical
    // binding from an already flattened path or the top module's name.
    if kind == "relative"
        && (is_local(&fields[1]) || imports::scope_names(tree)?.contains(&fields[1]))
    {
        kind = "local";
    }
    Ok(std::iter::once("celox.dump.path.v1".to_string())
        .chain(std::iter::once(kind.to_string()))
        .chain(fields)
        .collect::<Vec<_>>()
        .join("\0"))
}
