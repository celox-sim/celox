//! Reference formals bound to class properties and array elements (IEEE
//! 1800-2023 13.5.2).
//!
//! A call evaluates the object handle of an actual `h.p`, or the index of an
//! actual `a[i]`, once; the inlined body then reads and writes that property
//! of that object, or that element, wherever it names the formal.

use super::*;
use sv::ir::{ConstExpr, Expr, LValue, ObjectOp, Stmt};

/// The purpose of the hidden variables holding the object of a reference
/// formal bound to a class property.
pub(super) const OBJECT_TEMP: &str = "ref_object";

/// What a reference formal stands for in the inlined body.
pub(super) struct Alias {
    pub formal: String,
    /// The formal's name as written, for diagnostics.
    pub source_name: String,
    pub target: Target,
}

pub(super) enum Target {
    /// Property `field` of the object whose handle hidden variable `handle`
    /// holds.
    Property {
        handle: String,
        field: sv::ir::ObjectField,
    },
    /// An element of an unpacked array, its positions fixed at the call.
    /// The element is `width` bits of an array of `total` bits.
    Element {
        element: Box<LValue>,
        signed: bool,
        width: usize,
        total: usize,
    },
}

impl Alias {
    fn load(&self) -> Expr {
        match &self.target {
            Target::Property { handle, field } => Expr::Call {
                callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                args: vec![Some(Expr::Ident(handle.clone()))],
                arg_order: Vec::new(),
            },
            Target::Element {
                element, signed, ..
            } => {
                let LValue::Select { name, msb, lsb, .. } = &**element else {
                    unreachable!("an element is a select")
                };
                Expr::Select {
                    expr: Box::new(Expr::Ident(name.clone())),
                    msb: msb.clone(),
                    lsb: lsb.clone(),
                    signed: *signed,
                }
            }
        }
    }

    fn load_const(&self) -> ConstExpr {
        match &self.target {
            Target::Property { handle, field } => ConstExpr::Function {
                callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                args: vec![ConstExpr::Ident(handle.clone())],
                site: None,
            },
            Target::Element {
                element, signed, ..
            } => {
                let LValue::Select { name, msb, lsb, .. } = &**element else {
                    unreachable!("an element is a select")
                };
                ConstExpr::SelectRange {
                    expr: Box::new(ConstExpr::Ident(name.clone())),
                    msb: Box::new(msb.clone()),
                    lsb: Box::new(lsb.clone()),
                    signed: *signed,
                }
            }
        }
    }

    /// `formal = value`.
    fn store(&self, value: Expr, nonblocking: bool) -> Stmt {
        match &self.target {
            Target::Property { handle, field } => Stmt::Eval(Expr::Call {
                callee: sv::ir::Callee::Object(Box::new(ObjectOp::Store(*field))),
                args: vec![Some(Expr::Ident(handle.clone())), Some(value)],
                arg_order: Vec::new(),
            }),
            Target::Element { element, .. } => Stmt::Assign {
                lhs: (**element).clone(),
                rhs: value,
                nonblocking,
            },
        }
    }

    /// `body` with the formal replaced by what it refers to. `constant` is
    /// the value of a constant position expression.
    pub fn rewrite(
        &self,
        body: Vec<Stmt>,
        constant: &dyn Fn(&ConstExpr) -> Option<i128>,
    ) -> Result<Vec<Stmt>, sv::AnalyzerError> {
        let body = self.constant_positions(body, constant)?;
        let unsupported_write = || {
            unsupported(format!(
                "write to part of ref argument `{}` bound to a class property or an element",
                self.source_name
            ))
        };
        let mut error = None;
        let mut body = body;
        for stmt in &mut body {
            stmt.walk_mut(&mut |stmt| match stmt {
                Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs,
                    nonblocking,
                } if *name == self.formal => {
                    let value = std::mem::replace(rhs, Expr::Literal(String::new()));
                    *stmt = self.store(value, *nonblocking);
                }
                Stmt::Assign { lhs, .. } if lhs.name() == self.formal => {
                    let guard = match lhs {
                        LValue::Select { msb, .. } if !matches!(msb, ConstExpr::Literal(_)) => {
                            match self.inside(msb) {
                                Some(inside) => Some(inside),
                                None => {
                                    error.get_or_insert_with(unsupported_write);
                                    return;
                                }
                            }
                        }
                        _ => None,
                    };
                    match self.within(lhs) {
                        Some(within) => *lhs = within,
                        None => {
                            error.get_or_insert_with(unsupported_write);
                            return;
                        }
                    }
                    // Past the element, a write at a run-time position
                    // writes nothing.
                    if let Some(inside) = guard {
                        let write = std::mem::replace(stmt, Stmt::Break);
                        *stmt = Stmt::If {
                            condition: inside,
                            then_body: vec![write],
                            else_body: Vec::new(),
                        };
                    }
                }
                Stmt::AssignConcat { parts, .. }
                    if parts.iter().any(|part| part.name() == self.formal) =>
                {
                    for part in parts.iter_mut().filter(|part| part.name() == self.formal) {
                        match self.within(part) {
                            Some(within) => *part = within,
                            None => {
                                error.get_or_insert_with(unsupported_write);
                            }
                        }
                    }
                }
                _ => {}
            });
        }
        if let Some(error) = error {
            return Err(error);
        }
        Ok(body
            .into_iter()
            .map(|stmt| {
                stmt.map(&mut |expr| self.expr(expr), &mut |lvalue| {
                    self.lvalue(lvalue)
                })
            })
            .collect())
    }

    /// `body` with the constant positions of every select of an element
    /// formal as literals, and each read at a run-time position guarded: past
    /// the element it reads like a select past the array. Writes are guarded
    /// in [`Self::rewrite`].
    fn constant_positions(
        &self,
        body: Vec<Stmt>,
        constant: &dyn Fn(&ConstExpr) -> Option<i128>,
    ) -> Result<Vec<Stmt>, sv::AnalyzerError> {
        let Target::Element { width, .. } = &self.target else {
            return Ok(body);
        };
        let literal = |position: ConstExpr| match constant(&position) {
            Some(value) => ConstExpr::Literal(value.to_string()),
            None => position,
        };
        // The width of a select at a run-time position, which does not
        // depend on that position.
        let select_width = |msb: &ConstExpr, lsb: &ConstExpr| {
            let sample = |position: &ConstExpr| constant(&sampled(position));
            Some(sample(msb)?.abs_diff(sample(lsb)?) + 1)
        };
        let failed = std::cell::Cell::new(false);
        let read = |msb: ConstExpr, lsb: ConstExpr, signed: bool| {
            let (msb, lsb) = (literal(msb), literal(lsb));
            let select = |msb, lsb| Expr::Select {
                expr: Box::new(Expr::Ident(self.formal.clone())),
                msb,
                lsb,
                signed,
            };
            if matches!(msb, ConstExpr::Literal(_)) {
                return select(msb, lsb);
            }
            let (Some(inside), Some(bits)) = (self.inside(&msb), select_width(&msb, &lsb)) else {
                failed.set(true);
                return select(msb, lsb);
            };
            let past = |offset: u128| ConstExpr::Literal((*width as u128 + offset).to_string());
            Expr::Mux {
                condition: Box::new(inside),
                then_expr: Box::new(select(msb, lsb)),
                else_expr: Box::new(select(past(bits - 1), past(0))),
            }
        };
        let body: Vec<Stmt> = body
            .into_iter()
            .map(|stmt| {
                stmt.map(
                    &mut |expr| self.select_positions(expr, &read),
                    &mut |lvalue| match lvalue {
                        LValue::Select {
                            name,
                            msb,
                            lsb,
                            array_slice_width,
                            array_slice_reversed,
                        } if name == self.formal => LValue::Select {
                            name,
                            msb: literal(msb),
                            lsb: literal(lsb),
                            array_slice_width,
                            array_slice_reversed,
                        },
                        lvalue => lvalue,
                    },
                )
            })
            .collect();
        if failed.get() {
            return Err(unsupported(format!(
                "run-time select of ref argument `{}` bound to an array element",
                self.source_name
            )));
        }
        Ok(body)
    }

    fn select_positions(
        &self,
        expr: Expr,
        read: &dyn Fn(ConstExpr, ConstExpr, bool) -> Expr,
    ) -> Expr {
        let mut inner = |expr| self.select_positions(expr, read);
        match expr {
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } if matches!(&*expr, Expr::Ident(name) if *name == self.formal) => {
                read(msb, lsb, signed)
            }
            expr => map_operands(expr, &mut inner),
        }
    }

    /// Whether the select of an element formal whose highest bit is at `msb`
    /// stays within the element.
    fn inside(&self, msb: &ConstExpr) -> Option<Expr> {
        let Target::Element { width, .. } = &self.target else {
            return None;
        };
        Some(Expr::Binary {
            left: Box::new(expr_from_const_expr(msb)?),
            op: sv::ir::BinaryOp::Lt,
            right: Box::new(Expr::Literal(width.to_string())),
        })
    }

    /// The element bits that `lvalue`, a target naming the formal, writes.
    fn within(&self, lvalue: &LValue) -> Option<LValue> {
        let Target::Element { element, .. } = &self.target else {
            return None;
        };
        let LValue::Select { name, .. } = &**element else {
            return None;
        };
        Some(match lvalue {
            LValue::Ident(_) => (**element).clone(),
            LValue::Select {
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
                ..
            } => {
                let (msb, lsb) =
                    self.place(self.const_expr(msb.clone()), self.const_expr(lsb.clone()));
                LValue::Select {
                    name: name.clone(),
                    msb,
                    lsb,
                    array_slice_width: array_slice_width
                        .clone()
                        .map(|width| self.const_expr(width)),
                    array_slice_reversed: *array_slice_reversed,
                }
            }
        })
    }

    /// The array positions of bits `msb:lsb` of an element formal, which
    /// [`Self::constant_positions`] has made literals. A select beyond the
    /// element stays beyond the array, so it reads and writes nothing instead
    /// of reaching a neighbouring element.
    fn place(&self, msb: ConstExpr, lsb: ConstExpr) -> (ConstExpr, ConstExpr) {
        let Target::Element {
            element,
            width,
            total,
            ..
        } = &self.target
        else {
            unreachable!("only an element formal has positions")
        };
        let LValue::Select { lsb: base, .. } = &**element else {
            unreachable!("an element is a select")
        };
        let value = |position: &ConstExpr| match position {
            ConstExpr::Literal(value) => value.parse::<i128>().ok(),
            _ => None,
        };
        // A select past the element moves past the whole array.
        match (value(&msb), value(&lsb)) {
            (Some(high), Some(low)) if high >= *width as i128 || low >= *width as i128 => {
                let total = *total as i128;
                (
                    ConstExpr::Literal((total + high - low.min(high)).to_string()),
                    ConstExpr::Literal((total + low - low.min(high)).to_string()),
                )
            }
            _ => (offset(base, msb), offset(base, lsb)),
        }
    }

    fn lvalue(&self, lvalue: LValue) -> LValue {
        match lvalue {
            LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
            } => LValue::Select {
                name,
                msb: self.const_expr(msb),
                lsb: self.const_expr(lsb),
                array_slice_width: array_slice_width.map(|width| self.const_expr(width)),
                array_slice_reversed,
            },
            lvalue => lvalue,
        }
    }

    fn expr(&self, expr: Expr) -> Expr {
        let boxed = |expr: Box<Expr>| Box::new(self.expr(*expr));
        match expr {
            Expr::Ident(name) if name == self.formal => self.load(),
            Expr::Ident(_) | Expr::Literal(_) => expr,
            // A select of an element formal selects bits of the element.
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } if matches!(&*expr, Expr::Ident(name) if *name == self.formal)
                && matches!(self.target, Target::Element { .. }) =>
            {
                let Target::Element { element, .. } = &self.target else {
                    unreachable!("checked by the guard")
                };
                let LValue::Select { name, .. } = &**element else {
                    unreachable!("an element is a select")
                };
                let (msb, lsb) = self.place(self.const_expr(msb), self.const_expr(lsb));
                Expr::Select {
                    expr: Box::new(Expr::Ident(name.clone())),
                    msb,
                    lsb,
                    signed,
                }
            }
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: boxed(expr),
                msb: self.const_expr(msb),
                lsb: self.const_expr(lsb),
                signed,
            },
            Expr::Concat(parts) => {
                Expr::Concat(parts.into_iter().map(|part| self.expr(part)).collect())
            }
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: self.const_expr(count),
                parts: parts.into_iter().map(|part| self.expr(part)).collect(),
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: boxed(expr),
                width,
                signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op,
                expr: boxed(expr),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: boxed(left),
                op,
                right: boxed(right),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: boxed(condition),
                then_expr: boxed(then_expr),
                else_expr: boxed(else_expr),
            },
            Expr::Call {
                callee,
                args,
                arg_order,
            } => Expr::Call {
                callee,
                args: args
                    .into_iter()
                    .map(|arg| arg.map(|arg| self.expr(arg)))
                    .collect(),
                arg_order,
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: boxed(expr),
                items: items
                    .iter()
                    .map(|item| item.map(&mut |e| self.expr(e.clone())))
                    .collect(),
            },
        }
    }

    fn const_expr(&self, expr: ConstExpr) -> ConstExpr {
        let boxed = |expr: Box<ConstExpr>| Box::new(self.const_expr(*expr));
        match expr {
            ConstExpr::Ident(name) if name == self.formal => self.load_const(),
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => expr,
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: boxed(expr),
                bit: boxed(bit),
            },
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: boxed(expr),
                msb: boxed(msb),
                lsb: boxed(lsb),
                signed,
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: boxed(expr),
                width,
                signed,
            },
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee,
                args: args.into_iter().map(|arg| self.const_expr(arg)).collect(),
                site,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op,
                expr: boxed(expr),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: boxed(left),
                op,
                right: boxed(right),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: boxed(condition),
                then_expr: boxed(then_expr),
                else_expr: boxed(else_expr),
            },
        }
    }
}

/// Position `position` of an element that starts at `base`.
fn offset(base: &ConstExpr, position: ConstExpr) -> ConstExpr {
    ConstExpr::Binary {
        left: Box::new(base.clone()),
        op: sv::ir::BinaryOp::Add,
        right: Box::new(position),
    }
}

/// `expr` with `f` applied to each operand expression.
fn map_operands(expr: Expr, f: &mut dyn FnMut(Expr) -> Expr) -> Expr {
    let mut boxed = |expr: Box<Expr>| Box::new(f(*expr));
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: boxed(expr),
            msb,
            lsb,
            signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.into_iter().map(f).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts.into_iter().map(f).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: boxed(expr),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: boxed(expr),
        },
        Expr::Binary { left, op, right } => {
            let left = boxed(left);
            Expr::Binary {
                left,
                op,
                right: boxed(right),
            }
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            let condition = boxed(condition);
            let then_expr = boxed(then_expr);
            Expr::Mux {
                condition,
                then_expr,
                else_expr: boxed(else_expr),
            }
        }
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            callee,
            args: args.into_iter().map(|arg| arg.map(&mut *f)).collect(),
            arg_order,
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: boxed(expr),
            items: items
                .iter()
                .map(|item| item.map(&mut |e| f(e.clone())))
                .collect(),
        },
    }
}

/// `position` with every variable it reads replaced by 0.
fn sampled(position: &ConstExpr) -> ConstExpr {
    let boxed = |expr: &ConstExpr| Box::new(sampled(expr));
    match position {
        ConstExpr::Ident(_) => ConstExpr::Literal("0".into()),
        ConstExpr::Literal(_) => position.clone(),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: boxed(expr),
            bit: boxed(bit),
        },
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: boxed(expr),
            msb: boxed(msb),
            lsb: boxed(lsb),
            signed: *signed,
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: boxed(expr),
            width: *width,
            signed: *signed,
        },
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee: callee.clone(),
            args: args.iter().map(sampled).collect(),
            site: *site,
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op: *op,
            expr: boxed(expr),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: boxed(left),
            op: *op,
            right: boxed(right),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: boxed(condition),
            then_expr: boxed(then_expr),
            else_expr: boxed(else_expr),
        },
    }
}
