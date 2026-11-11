//! Hoisting: the subroutine calls of an expression or of the select
//! positions of a target are executed first, left to right, and replaced by
//! hidden variables holding their results; freezing the positions of a
//! target.

use super::*;

impl Comb<'_, '_> {
    /// Evaluate the subroutine calls of a select position, left to right,
    /// and refer to their results. A flattened select repeats an index in
    /// its bounds and range checks; the copies of one call share its site,
    /// so the call is evaluated once.
    pub(super) fn hoist_const(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
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
            ConstExpr::Function {
                callee: callee @ sv::ir::Callee::System(_),
                ..
            } if self.m.is_callee(callee) => {
                return Err(unsupported(format!(
                    "{callee} called in combinational logic"
                )));
            }
            ConstExpr::Function { callee, args, .. } if self.m.is_callee(callee) => {
                if let Some((_, result)) = calls.iter().find(|(call, _)| call == expr) {
                    return Ok(result.clone());
                }
                let mut lowered = Vec::with_capacity(args.len());
                for arg in args {
                    let arg = self.hoist_const(store, frames, arg, calls)?;
                    lowered.push(expr_from_const_expr(&arg).ok_or_else(|| {
                        unsupported(format!("argument of `{callee}` in a select"))
                    })?);
                }
                let call = sv::ir::Expr::Call {
                    arg_order: Vec::new(),
                    callee: callee.clone(),
                    args: lowered.into_iter().map(Some).collect(),
                };
                let sv::ir::Expr::Ident(result) = self.hoist(store, frames, &call)? else {
                    return Err(unsupported(format!("call of `{callee}` in a select")));
                };
                let result = ConstExpr::Ident(result);
                calls.push((expr.clone(), result.clone()));
                result
            }
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.hoist_const(store, frames, arg, calls))
                    .collect::<Result<_, _>>()?,
                site: *site,
            },
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: Box::new(self.hoist_const(store, frames, expr, calls)?),
                msb: Box::new(self.hoist_const(store, frames, msb, calls)?),
                lsb: Box::new(self.hoist_const(store, frames, lsb, calls)?),
                signed: *signed,
            },
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.hoist_const(store, frames, expr, calls)?),
                bit: Box::new(self.hoist_const(store, frames, bit, calls)?),
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(self.hoist_const(store, frames, expr, calls)?),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.hoist_const(store, frames, expr, calls)?),
            },
            ConstExpr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && self.m.const_calls(right) =>
            {
                let left = self.hoist_const(store, frames, left, calls)?;
                let guard = self.short_circuit_guard(store, frames, *op, &operand(&left)?)?;
                let right = self.guarded(store, guard, |this, store| {
                    this.hoist_const(store, frames, right, calls)
                })?;
                ConstExpr::Binary {
                    left: Box::new(left),
                    op: *op,
                    right: Box::new(right),
                }
            }
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.hoist_const(store, frames, left, calls)?),
                op: *op,
                right: Box::new(self.hoist_const(store, frames, right, calls)?),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.hoist_const(store, frames, condition, calls)?;
                let (then_expr, else_expr) =
                    if self.m.const_calls(then_expr) || self.m.const_calls(else_expr) {
                        // A part of both arms is a copy the flattening of a
                        // select made, which runs whichever arm is taken.
                        for shared in self.m.shared_calls(then_expr, else_expr) {
                            self.hoist_const(store, frames, shared, calls)?;
                        }
                        let (then_guard, else_guard) =
                            self.mux_guards(store, frames, &operand(&condition)?)?;
                        let then_expr = self.guarded(store, then_guard, |this, store| {
                            this.hoist_const(store, frames, then_expr, calls)
                        })?;
                        let else_expr = self.guarded(store, else_guard, |this, store| {
                            this.hoist_const(store, frames, else_expr, calls)
                        })?;
                        (then_expr, else_expr)
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
        store: &mut Store,
        frames: &[Frame],
        lvalue: &sv::ir::LValue,
    ) -> Result<sv::ir::LValue, sv::AnalyzerError> {
        if !self.m.lvalue_calls(lvalue) {
            return Ok(lvalue.clone());
        }
        let mut lvalue = lvalue.clone();
        if let sv::ir::LValue::Select { msb, lsb, .. } = &mut lvalue {
            let mut calls = Vec::new();
            *lsb = self.hoist_const(store, frames, lsb, &mut calls)?;
            *msb = self.hoist_const(store, frames, msb, &mut calls)?;
        }
        Ok(lvalue)
    }

    /// `lvalue` with the variables its select positions read replaced by
    /// copies of their current values, so a later write to one does not move
    /// the target.
    pub(super) fn freeze_lvalue(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        mut lvalue: sv::ir::LValue,
    ) -> Result<sv::ir::LValue, sv::AnalyzerError> {
        let sv::ir::LValue::Select { msb, lsb, .. } = &mut lvalue else {
            return Ok(lvalue);
        };
        let mut copies = HashMap::default();
        *lsb = self.freeze_const(store, frames, lsb, &mut copies)?;
        *msb = self.freeze_const(store, frames, msb, &mut copies)?;
        Ok(lvalue)
    }

    /// `expr` with each variable it reads, and each bit it reads from an
    /// unpacked array, replaced by a copy of its current value.
    pub(super) fn freeze_const(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
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
                let (temp, temp_name) = self.m.temp("position", width, signed, is_4state);
                let value = self.read(store, id, full(width))?;
                self.write(store, temp, full(width), value)?;
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
                let msb = self.freeze_const(store, frames, msb, copies)?;
                let lsb = self.freeze_const(store, frames, lsb, copies)?;
                let is_4state = self.m.var(self.m.id(name).expect("a variable")).is_4state;
                let read = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                    msb,
                    lsb,
                    signed: *signed,
                };
                let (node, sources) = self.eval(store, frames, &read, None)?;
                let width = celox_slt::get_width(node, self.arena);
                let (temp, temp_name) = self.m.temp("position", width, *signed, is_4state);
                self.write(store, temp, full(width), (node, sources))?;
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::Select { expr: base, bit } if matches!(&**base, ConstExpr::Ident(name) if array(self, name) == Some(true)) =>
            {
                let ConstExpr::Ident(name) = &**base else {
                    unreachable!()
                };
                let bit = self.freeze_const(store, frames, bit, copies)?;
                let is_4state = self.m.var(self.m.id(name).expect("a variable")).is_4state;
                let read = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                    msb: bit.clone(),
                    lsb: bit,
                    signed: false,
                };
                let (node, sources) = self.eval(store, frames, &read, Some((1, false)))?;
                let node =
                    coerce_node_width(self.arena, node, Some(1), false).map_err(slt_error)?;
                let (temp, temp_name) = self.m.temp("position", 1, false, is_4state);
                self.write(store, temp, full(1), (node, sources))?;
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => return Ok(expr.clone()),
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: Box::new(self.freeze_const(store, frames, expr, copies)?),
                msb: Box::new(self.freeze_const(store, frames, msb, copies)?),
                lsb: Box::new(self.freeze_const(store, frames, lsb, copies)?),
                signed: *signed,
            },
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.freeze_const(store, frames, expr, copies)?),
                bit: Box::new(self.freeze_const(store, frames, bit, copies)?),
            },
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.freeze_const(store, frames, arg, copies))
                    .collect::<Result<_, _>>()?,
                site: *site,
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(self.freeze_const(store, frames, expr, copies)?),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.freeze_const(store, frames, expr, copies)?),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.freeze_const(store, frames, left, copies)?),
                op: *op,
                right: Box::new(self.freeze_const(store, frames, right, copies)?),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new(self.freeze_const(store, frames, condition, copies)?),
                then_expr: Box::new(self.freeze_const(store, frames, then_expr, copies)?),
                else_expr: Box::new(self.freeze_const(store, frames, else_expr, copies)?),
            },
        };
        copies.insert(expr.clone(), frozen.clone());
        Ok(frozen)
    }

    /// Execute the user subroutine calls of an expression, left to right, and
    /// replace each by a hidden variable holding its result. Calls in an
    /// operand that short-circuit evaluation may skip run only when it is
    /// evaluated (IEEE 1800-2023 11.4.7, 11.4.11).
    pub(super) fn hoist(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        expr: &sv::ir::Expr,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if !self.m.calls(expr) {
            return Ok(expr.clone());
        }
        Ok(match expr {
            Expr::Call {
                callee: callee @ sv::ir::Callee::System(_),
                ..
            } if self.m.is_callee(callee) => {
                return Err(unsupported(format!(
                    "{callee} called in combinational logic"
                )));
            }
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                ..
            } if self.m.dpi_imports.contains_key(name) => {
                return Err(unsupported(format!(
                    "DPI-C function `{name}` called in combinational logic"
                )));
            }
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                args,
                arg_order,
            } if self.m.subroutines.contains_key(name) => {
                let args = args.clone();
                let result = self.call(store, frames, name, &args, arg_order)?;
                let Some((node, sources)) = result else {
                    return Err(unsupported(format!(
                        "void function `{name}` used as a value"
                    )));
                };
                let subroutine = self
                    .m
                    .subroutine(name)
                    .cloned()
                    .ok_or_else(|| unsupported(format!("function `{name}`")))?;
                let (width, signed, is_4state) = match &subroutine.return_type {
                    Some(r#type) => self.m.type_shape(r#type)?,
                    None => (1, false, false),
                };
                let (temp, temp_name) = self.m.temp("call", width, signed, is_4state);
                self.m.variables.get_mut(&temp).unwrap().float = subroutine
                    .return_type
                    .as_ref()
                    .and_then(|ty| match ty.kind() {
                        sv::ir::TypeKind::Real => Some(celox_sir::FloatPrecision::F64),
                        sv::ir::TypeKind::Shortreal => Some(celox_sir::FloatPrecision::F32),
                        _ => None,
                    });
                let node =
                    coerce_node_width(self.arena, node, Some(width), signed).map_err(slt_error)?;
                self.write(store, temp, full(width), (node, sources))?;
                Expr::Ident(temp_name)
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
                    .map(|arg| {
                        arg.as_ref()
                            .map(|arg| self.hoist(store, frames, arg))
                            .transpose()
                    })
                    .collect::<Result<_, _>>()?,
            },
            Expr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && self.m.calls(right) =>
            {
                let left = self.hoist(store, frames, left)?;
                let guard = self.short_circuit_guard(store, frames, *op, &left)?;
                let right =
                    self.guarded(store, guard, |this, store| this.hoist(store, frames, right))?;
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
                let condition = self.hoist(store, frames, condition)?;
                let (then_guard, else_guard) = self.mux_guards(store, frames, &condition)?;
                let then_expr = self.guarded(store, then_guard, |this, store| {
                    this.hoist(store, frames, then_expr)
                })?;
                let else_expr = self.guarded(store, else_guard, |this, store| {
                    this.hoist(store, frames, else_expr)
                })?;
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => {
                let expr = self.hoist(store, frames, expr)?;
                let mut calls = Vec::new();
                let lsb = self.hoist_const(store, frames, lsb, &mut calls)?;
                let msb = self.hoist_const(store, frames, msb, &mut calls)?;
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
                    .map(|part| self.hoist(store, frames, part))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.clone(),
                parts: parts
                    .iter()
                    .map(|part| self.hoist(store, frames, part))
                    .collect::<Result<_, _>>()?,
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new(self.hoist(store, frames, expr)?),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(self.hoist(store, frames, expr)?),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.hoist(store, frames, left)?),
                op: *op,
                right: Box::new(self.hoist(store, frames, right)?),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new(self.hoist(store, frames, condition)?),
                then_expr: then_expr.clone(),
                else_expr: else_expr.clone(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new(self.hoist(store, frames, expr)?),
                items: items
                    .iter()
                    .map(|item| -> Result<sv::ir::InsideItem, sv::AnalyzerError> {
                        Ok(match item {
                            sv::ir::InsideItem::Value(value) => {
                                sv::ir::InsideItem::Value(self.hoist(store, frames, value)?)
                            }
                            sv::ir::InsideItem::Range { low, high } => sv::ir::InsideItem::Range {
                                low: self.hoist(store, frames, low)?,
                                high: self.hoist(store, frames, high)?,
                            },
                        })
                    })
                    .collect::<Result<_, _>>()?,
            },
            Expr::Ident(_) | Expr::Literal(_) => expr.clone(),
        })
    }
}
