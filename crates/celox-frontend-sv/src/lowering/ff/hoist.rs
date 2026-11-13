//! Hoisting: the subroutine and system function calls of an expression or of
//! a select position run first, left to right, and the expression then reads
//! their results; and the freezing of the positions of an lvalue.

use super::*;

impl Ff<'_, '_> {
    /// Evaluate the subroutine calls of a select position, left to right,
    /// and refer to their results. A flattened select repeats an index in
    /// its bounds and range checks; the copies of one call share its site,
    /// so the call is evaluated once.
    pub(super) fn hoist_const(
        &mut self,
        expr: &sv::ir::ConstExpr,
        calls: &mut Vec<(sv::ir::ConstExpr, sv::ir::ConstExpr)>,
    ) -> Result<sv::ir::ConstExpr, sv::AnalyzerError> {
        use sv::ir::ConstExpr;
        if !self.m.const_calls(expr) {
            return Ok(expr.clone());
        }
        let operand = |expr: &ConstExpr| {
            expr_from_const_expr(expr).ok_or_else(|| unsupported("operand in a select"))
        };
        Ok(match expr {
            ConstExpr::Function { callee, args, .. } if self.m.is_callee(callee) => {
                if let Some((_, result)) = calls.iter().find(|(call, _)| call == expr) {
                    return Ok(result.clone());
                }
                let mut lowered = Vec::with_capacity(args.len());
                for arg in args {
                    // Sampled/history functions must resolve their operands
                    // before nested calls hide the original inputs behind
                    // temporaries holding current-slot values.
                    let arg = if matches!(
                        callee,
                        sv::ir::Callee::System(
                            sv::ir::SystemFunction::Sampled
                                | sv::ir::SystemFunction::Past
                                | sv::ir::SystemFunction::Rose
                                | sv::ir::SystemFunction::Fell
                                | sv::ir::SystemFunction::Stable
                                | sv::ir::SystemFunction::Changed
                        )
                    ) {
                        arg.clone()
                    } else {
                        self.hoist_const(arg, calls)?
                    };
                    lowered.push(expr_from_const_expr_with_calls(&arg, true).ok_or_else(|| {
                        unsupported(format!("argument of `{callee}` in a select"))
                    })?);
                }
                let call = sv::ir::Expr::Call {
                    arg_order: Vec::new(),
                    callee: callee.clone(),
                    args: lowered.into_iter().map(Some).collect(),
                };
                let result = match self.hoist(&call)? {
                    sv::ir::Expr::Ident(result) => result,
                    // A sampled/history operand can retain any expression
                    // shape (including a resize inserted for the index).
                    // Evaluate it only after its references are resolved.
                    hoisted => {
                        let value = self.eval(&hoisted, None)?;
                        let width = self.b.register(&value).width();
                        let (temp, name) = self.m.temp("select", width, false, true);
                        self.store(temp, SIROffset::Static(0), width, value);
                        name
                    }
                };
                let result = ConstExpr::Ident(result);
                calls.push((expr.clone(), result.clone()));
                result
            }
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.hoist_const(arg, calls))
                    .collect::<Result<_, _>>()?,
                site: *site,
            },
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: Box::new(self.hoist_const(expr, calls)?),
                msb: Box::new(self.hoist_const(msb, calls)?),
                lsb: Box::new(self.hoist_const(lsb, calls)?),
                signed: *signed,
            },
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.hoist_const(expr, calls)?),
                bit: Box::new(self.hoist_const(bit, calls)?),
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(self.hoist_const(expr, calls)?),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.hoist_const(expr, calls)?),
            },
            ConstExpr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && self.m.const_calls(right) =>
            {
                let left = self.hoist_const(left, calls)?;
                let right = self
                    .short_circuit(*op, &operand(&left)?, |this| this.hoist_const(right, calls))?;
                ConstExpr::Binary {
                    left: Box::new(left),
                    op: *op,
                    right: Box::new(right),
                }
            }
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.hoist_const(left, calls)?),
                op: *op,
                right: Box::new(self.hoist_const(right, calls)?),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.hoist_const(condition, calls)?;
                let (then_expr, else_expr) =
                    if self.m.const_calls(then_expr) || self.m.const_calls(else_expr) {
                        // A part of both arms is a copy the flattening of a
                        // select made, which runs whichever arm is taken.
                        for shared in self.m.shared_calls(then_expr, else_expr) {
                            self.hoist_const(shared, calls)?;
                        }
                        // `calls` is shared by both arms, which run one after
                        // the other.
                        self.mux_arms(
                            &operand(&condition)?,
                            calls,
                            |this, calls| this.hoist_const(then_expr, calls),
                            |this, calls| this.hoist_const(else_expr, calls),
                        )?
                    } else {
                        ((**then_expr).clone(), (**else_expr).clone())
                    };
                ConstExpr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            ConstExpr::Literal(_) | ConstExpr::Ident(_) => expr.clone(),
        })
    }

    /// An assignment target whose select positions refer to the results of
    /// their subroutine calls, evaluated now.
    pub(super) fn hoist_lvalue(
        &mut self,
        lvalue: &sv::ir::LValue,
    ) -> Result<sv::ir::LValue, sv::AnalyzerError> {
        if !self.m.lvalue_calls(lvalue) {
            return Ok(lvalue.clone());
        }
        let mut lvalue = lvalue.clone();
        if let sv::ir::LValue::Select { msb, lsb, .. } = &mut lvalue {
            let mut calls = Vec::new();
            *lsb = self.hoist_const(lsb, &mut calls)?;
            *msb = self.hoist_const(msb, &mut calls)?;
        }
        Ok(lvalue)
    }

    /// A copy of a position for [`Self::freeze_const`].
    pub(super) fn position_temp(
        &mut self,
        width: usize,
        signed: bool,
        is_4state: bool,
    ) -> Result<(SourceVarId, String), sv::AnalyzerError> {
        let (temp, name) = self.m.temp("position", width, signed, is_4state);
        if self.activation.frame_positions && self.activation.forked {
            let slot = self.activation.frame_locals.len();
            self.activation.frame_locals.push(temp);
            self.with_kernel(|kernel| kernel.local(slot))
                .map_err(kernel_error)?;
        }
        Ok((temp, name))
    }

    /// `lvalue` with the variables its select positions read replaced by
    /// copies of their current values, so a later write to one does not move
    /// the target.
    pub(super) fn freeze_lvalue(
        &mut self,
        mut lvalue: sv::ir::LValue,
    ) -> Result<sv::ir::LValue, sv::AnalyzerError> {
        let sv::ir::LValue::Select { msb, lsb, .. } = &mut lvalue else {
            return Ok(lvalue);
        };
        let mut copies = HashMap::default();
        *lsb = self.freeze_const(lsb, &mut copies)?;
        *msb = self.freeze_const(msb, &mut copies)?;
        Ok(lvalue)
    }

    /// `expr` with each variable it reads, and each bit it reads from an
    /// unpacked array, replaced by a copy of its current value.
    pub(super) fn freeze_const(
        &mut self,
        expr: &sv::ir::ConstExpr,
        copies: &mut HashMap<sv::ir::ConstExpr, sv::ir::ConstExpr>,
    ) -> Result<sv::ir::ConstExpr, sv::AnalyzerError> {
        use sv::ir::ConstExpr;
        if let Some(copy) = copies.get(expr) {
            return Ok(copy.clone());
        }
        let array = |this: &Self, name: &str| {
            this.m
                .id(name)
                .map(|id| !this.m.var(id).array_dims.is_empty())
        };
        let frozen = match expr {
            ConstExpr::Ident(name) if array(self, name) == Some(false) => {
                let id = self.m.id(name).expect("a variable");
                let variable = self.m.var(id);
                let (width, signed, is_4state) =
                    (variable.width, variable.signed, variable.is_4state);
                let (temp, temp_name) = self.position_temp(width, signed, is_4state)?;
                let value = self.eval(&sv::ir::Expr::Ident(name.clone()), Some((width, signed)))?;
                self.store(temp, SIROffset::Static(0), width, value);
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::SelectRange {
                expr: base,
                msb,
                lsb,
                signed,
            } if matches!(&**base, ConstExpr::Ident(name) if self.m.id(name).is_some()) => {
                let ConstExpr::Ident(name) = &**base else {
                    unreachable!()
                };
                let msb = self.freeze_const(msb, copies)?;
                let lsb = self.freeze_const(lsb, copies)?;
                let is_4state = self.m.var(self.m.id(name).expect("a variable")).is_4state;
                let read = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                    msb,
                    lsb,
                    signed: *signed,
                };
                let value = self.eval(&read, None)?;
                let width = self.b.register(&value).width();
                let (temp, temp_name) = self.position_temp(width, *signed, is_4state)?;
                // A frame slot starts a new block: read the bits there.
                let value = if self.activation.frame_positions && self.activation.forked {
                    self.eval(&read, None)?
                } else {
                    value
                };
                self.store(temp, SIROffset::Static(0), width, value);
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::Select { expr: base, bit } if matches!(&**base, ConstExpr::Ident(name) if array(self, name) == Some(true)) =>
            {
                let ConstExpr::Ident(name) = &**base else {
                    unreachable!()
                };
                let bit = self.freeze_const(bit, copies)?;
                let is_4state = self.m.var(self.m.id(name).expect("a variable")).is_4state;
                let read = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                    msb: bit.clone(),
                    lsb: bit,
                    signed: false,
                };
                let (temp, temp_name) = self.position_temp(1, false, is_4state)?;
                let value = self.eval(&read, Some((1, false)))?;
                self.store(temp, SIROffset::Static(0), 1, value);
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => return Ok(expr.clone()),
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: Box::new(self.freeze_const(expr, copies)?),
                msb: Box::new(self.freeze_const(msb, copies)?),
                lsb: Box::new(self.freeze_const(lsb, copies)?),
                signed: *signed,
            },
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.freeze_const(expr, copies)?),
                bit: Box::new(self.freeze_const(bit, copies)?),
            },
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.freeze_const(arg, copies))
                    .collect::<Result<_, _>>()?,
                site: *site,
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(self.freeze_const(expr, copies)?),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.freeze_const(expr, copies)?),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.freeze_const(left, copies)?),
                op: *op,
                right: Box::new(self.freeze_const(right, copies)?),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new(self.freeze_const(condition, copies)?),
                then_expr: Box::new(self.freeze_const(then_expr, copies)?),
                else_expr: Box::new(self.freeze_const(else_expr, copies)?),
            },
        };
        copies.insert(expr.clone(), frozen.clone());
        Ok(frozen)
    }

    /// Execute the user subroutine calls of an expression and replace each by
    /// a hidden variable holding its result.
    pub(super) fn hoist(&mut self, expr: &sv::ir::Expr) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        self.observation.implicit_write_depth += 1;
        let result = self.hoist_inner(expr);
        self.observation.implicit_write_depth -= 1;
        result
    }

    pub(super) fn hoist_inner(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if !self.m.calls(expr) {
            return Ok(expr.clone());
        }
        Ok(match expr {
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Process(op)),
                args,
                ..
            } if op.is_mailbox() => self.mailbox_call(*op, args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Process(op)),
                args,
                ..
            } => self.process_call(*op, args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::Checked {
                        width,
                        signed,
                        check,
                    },
                args,
                ..
            } => {
                let [Some(condition), Some(value)] = args.as_slice() else {
                    return Err(unsupported("invalid tagged access check"));
                };
                self.system_task(
                    sv::ir::SystemTask::Assert,
                    &[
                        sv::ir::SystemTaskArg::Expr(condition.clone()),
                        sv::ir::SystemTaskArg::Str(check.message().into()),
                    ],
                    None,
                    None,
                )?;
                let (_, name) = self.m.temp("tagged_member", *width, *signed, true);
                self.assign(&sv::ir::LValue::Ident(name.clone()), value)?;
                Expr::Ident(name)
            }
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Sampled),
                args,
                ..
            } => {
                let operand = self.sampled(args)?;
                self.hoist(&operand)?
            }
            Expr::Call {
                callee: sv::ir::Callee::System(function @ sv::ir::SystemFunction::Past),
                args,
                ..
            } => {
                let operand = self.past(*function, args)?;
                self.hoist(&operand)?
            }
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::Rose
                        | sv::ir::SystemFunction::Fell
                        | sv::ir::SystemFunction::Stable
                        | sv::ir::SystemFunction::Changed),
                    ),
                args,
                ..
            } => {
                let change = self.value_change(*function, args)?;
                self.hoist(&change)?
            }
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Fread),
                args,
                ..
            } => self.fread(args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::Fgets | sv::ir::SystemFunction::Ferror),
                    ),
                args,
                ..
            } => self.string_file_function(*function, args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::Ftell
                        | sv::ir::SystemFunction::Fseek
                        | sv::ir::SystemFunction::Rewind
                        | sv::ir::SystemFunction::Fgetc
                        | sv::ir::SystemFunction::Ungetc
                        | sv::ir::SystemFunction::Feof),
                    ),
                args,
                ..
            } => self.file_function(*function, args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function
                        @ (sv::ir::SystemFunction::Sscanf | sv::ir::SystemFunction::Fscanf),
                    ),
                args,
                ..
            } => self.scan_function(*function, args)?,
            Expr::Call {
                callee: sv::ir::Callee::String(op),
                args,
                ..
            } => self.string_call(op.clone(), args)?,
            Expr::Call {
                callee: sv::ir::Callee::Object(op),
                args,
                ..
            } => match &**op {
                sv::ir::ObjectOp::Container { op, layout } => {
                    self.container_call(*op, *layout, args)?
                }
                sv::ir::ObjectOp::Locator(locator) => self.locator(*locator, args)?,
                sv::ir::ObjectOp::Pattern(pattern) => self.aggregate_pattern(*pattern, args)?,
                sv::ir::ObjectOp::Concat(concat) => self.container_concat(concat, args)?,
                sv::ir::ObjectOp::Stream(op) => self.stream_call(op, args)?,
                _ => self.object_call(op, args)?,
            },
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::System),
                args,
                ..
            } => self.system_call(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Fopen),
                args,
                ..
            } => self.fopen(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::ScopeRandomizeEnum),
                args,
                ..
            } => self.randomize_enum(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::ScopeRandomize),
                args,
                ..
            } => {
                for arg in args.iter().flatten() {
                    let Expr::Ident(name) = arg else {
                        return Err(unsupported("scope randomize variable"));
                    };
                    let target = sv::ir::LValue::Ident(name.clone());
                    let width = self.lvalue_width(&target)?;
                    let mut words = Vec::with_capacity(width.div_ceil(32));
                    for offset in (0..width).step_by(32) {
                        words.push(Expr::Resize {
                            expr: Box::new(self.randomize_word()?),
                            width: (width - offset).min(32),
                            signed: false,
                        });
                    }
                    words.reverse();
                    self.assign(&target, &Expr::Concat(words))?;
                }
                Expr::Literal("32'sd1".into())
            }
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Urandom),
                args,
                ..
            } => {
                let seed = match args.first().and_then(Option::as_ref) {
                    Some(seed) => Some(self.hoist(seed)?),
                    None => None,
                };
                self.urandom(seed)?
            }
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::UrandomRange),
                args,
                ..
            } => self.urandom_range(args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::TestPlusargs
                        | sv::ir::SystemFunction::ValuePlusargs),
                    ),
                args,
                ..
            } => self.plusargs(*function, args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::DistNormal
                        | sv::ir::SystemFunction::DistExponential
                        | sv::ir::SystemFunction::DistPoisson
                        | sv::ir::SystemFunction::DistChiSquare
                        | sv::ir::SystemFunction::DistT
                        | sv::ir::SystemFunction::DistErlang),
                    ),
                args,
                ..
            } => self.distribution(*function, args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::DistUniform),
                args,
                ..
            } => self.dist_uniform(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Random),
                args,
                ..
            } => self.random(args.first().and_then(Option::as_ref))?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::Time
                        | sv::ir::SystemFunction::Stime
                        | sv::ir::SystemFunction::Realtime),
                    ),
                args,
                ..
            } => self.time(*function, args)?,
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                args,
                arg_order,
            } if self.m.dpi_imports.contains_key(name) => {
                let import = self.m.dpi_imports[name].clone();
                let Some(r#type) = import.return_type() else {
                    return Err(unsupported(format!(
                        "void DPI-C function `{name}` used as a value"
                    )));
                };
                let args = args.clone();
                let result = self
                    .dpi_call(&import, &args, arg_order)?
                    .expect("a non-void DPI-C import defines a result");
                let (temp, temp_name) = self.m.temp(
                    "dpi",
                    r#type.width(),
                    r#type.is_signed(),
                    r#type.is_4state(),
                );
                if r#type == sv::ir::DpiType::String {
                    self.m.variables.get_mut(&temp).unwrap().value_kind =
                        celox_design::ValueKind::StrRef;
                }
                self.store(temp, SIROffset::Static(0), r#type.width(), result);
                Expr::Ident(temp_name)
            }
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                args,
                arg_order,
            } if self.m.subroutines.contains_key(name) => {
                let args = args.clone();
                let subroutine = self
                    .m
                    .subroutine(name)
                    .cloned()
                    .ok_or_else(|| unsupported(format!("function `{name}`")))?;
                let Some(r#type) = subroutine.return_type.clone() else {
                    return Err(unsupported(format!(
                        "void function `{name}` used as a value"
                    )));
                };
                let (width, signed, is_4state) = self.m.type_shape(&r#type)?;
                let result = self.call(name, &args, arg_order)?;
                let (temp, temp_name) = self.m.temp("call", width, signed, is_4state);
                self.m.variables.get_mut(&temp).unwrap().float = match r#type.kind() {
                    sv::ir::TypeKind::Real => Some(celox_sir::FloatPrecision::F64),
                    sv::ir::TypeKind::Shortreal => Some(celox_sir::FloatPrecision::F32),
                    _ => None,
                };
                self.m.variables.get_mut(&temp).unwrap().value_kind = match r#type.kind() {
                    sv::ir::TypeKind::String => celox_design::ValueKind::StrRef,
                    sv::ir::TypeKind::Object => celox_design::ValueKind::ObjRef,
                    _ => celox_design::ValueKind::Bits,
                };
                let Some(result) = result else {
                    return Err(unsupported(format!(
                        "void function `{name}` used as a value"
                    )));
                };
                let value = self.eval(&Expr::Ident(result), Some((width, signed)))?;
                self.store(temp, SIROffset::Static(0), width, value);
                Expr::Ident(temp_name)
            }
            Expr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && self.m.calls(right) =>
            {
                let left = self.hoist(left)?;
                let right = self.short_circuit(*op, &left, |this| this.hoist(right))?;
                Expr::Binary {
                    left: Box::new(left),
                    op: *op,
                    right: Box::new(right),
                }
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } if self.m.calls(then_expr) || self.m.calls(else_expr) => {
                let condition = self.hoist(condition)?;
                let (then_expr, else_expr) = self.mux_arms(
                    &condition,
                    &mut (),
                    |this, ()| this.hoist(then_expr),
                    |this, ()| this.hoist(else_expr),
                )?;
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            Expr::Call {
                callee,
                args,
                arg_order,
            } => Expr::Call {
                arg_order: arg_order.clone(),
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| arg.as_ref().map(|arg| self.hoist(arg)).transpose())
                    .collect::<Result<_, _>>()?,
            },
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => {
                let expr = self.hoist(expr)?;
                let mut calls = Vec::new();
                let lsb = self.hoist_const(lsb, &mut calls)?;
                let msb = self.hoist_const(msb, &mut calls)?;
                Expr::Select {
                    expr: Box::new(expr),
                    msb,
                    lsb,
                    signed: *signed,
                }
            }
            Expr::Concat(parts) => Expr::Concat(
                parts
                    .iter()
                    .map(|part| self.hoist(part))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.clone(),
                parts: parts
                    .iter()
                    .map(|part| self.hoist(part))
                    .collect::<Result<_, _>>()?,
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new(self.hoist(expr)?),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(self.hoist(expr)?),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.hoist(left)?),
                op: *op,
                right: Box::new(self.hoist(right)?),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new(self.hoist(condition)?),
                then_expr: then_expr.clone(),
                else_expr: else_expr.clone(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new(self.hoist(expr)?),
                items: items
                    .iter()
                    .map(|item| -> Result<sv::ir::InsideItem, sv::AnalyzerError> {
                        Ok(match item {
                            sv::ir::InsideItem::Value(value) => {
                                sv::ir::InsideItem::Value(self.hoist(value)?)
                            }
                            sv::ir::InsideItem::Range { low, high } => sv::ir::InsideItem::Range {
                                low: self.hoist(low)?,
                                high: self.hoist(high)?,
                            },
                        })
                    })
                    .collect::<Result<_, _>>()?,
            },
            Expr::Ident(_) | Expr::Literal(_) => expr.clone(),
        })
    }
}
