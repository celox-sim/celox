//! Types of expressions: strings, reals, and the signedness and width of
//! expressions that call user functions.

use super::*;

impl ProcModule<'_> {
    /// Whether `expr` has type `string`: a string variable, or an operation
    /// or function that returns one.
    pub fn expr_is_string(&self, expr: &sv::ir::Expr) -> bool {
        match expr {
            sv::ir::Expr::Call {
                callee:
                    sv::ir::Callee::System(sv::ir::SystemFunction::Process(
                        celox_design::ProcessOp::GetRandstate,
                    )),
                ..
            } => true,
            sv::ir::Expr::Ident(name) => self
                .name_to_id
                .get(name)
                .and_then(|id| self.variables.get(id))
                .is_some_and(|variable| {
                    variable.value_kind == celox_design::ValueKind::StrRef
                        && variable.array_dims.is_empty()
                }),
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::String(op),
                ..
            } => op.returns_string(),
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                ..
            } if self.dpi_imports.contains_key(name) => {
                self.dpi_imports[name].return_type() == Some(sv::ir::DpiType::String)
            }
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(op),
                ..
            } => {
                matches!(**op, sv::ir::ObjectOp::Load(field)
                if field.handle == Some(sv::ir::HandleProperty::String))
                    || matches!(**op, sv::ir::ObjectOp::Container { op: sv::ir::ContainerOp::Get | sv::ir::ContainerOp::PopFront | sv::ir::ContainerOp::PopBack, layout } if layout.handle == Some(sv::ir::HandleProperty::String))
                    || matches!(
                        **op,
                        sv::ir::ObjectOp::Pattern(_) | sv::ir::ObjectOp::GetRandstate
                    )
                    || matches!(
                        **op,
                        sv::ir::ObjectOp::Stream(
                            sv::ir::StreamOp::ToString | sv::ir::StreamOp::TakeString { .. }
                        )
                    )
            }
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                ..
            } => self.subroutines.get(name).is_some_and(|subroutine| {
                subroutine
                    .return_type
                    .as_ref()
                    .is_some_and(|ty| ty.kind() == sv::ir::TypeKind::String)
            }),
            sv::ir::Expr::Mux {
                then_expr,
                else_expr,
                ..
            } => self.expr_is_string(then_expr) || self.expr_is_string(else_expr),
            _ => false,
        }
    }

    pub fn expr_float_precision(&self, expr: &sv::ir::Expr) -> Option<celox_sir::FloatPrecision> {
        use celox_sir::FloatPrecision as P;
        use sv::ir::{BinaryOp, Expr};
        let combine = |left, right| match (left, right) {
            (Some(P::F64), _) | (_, Some(P::F64)) => Some(P::F64),
            (Some(P::F32), _) | (_, Some(P::F32)) => Some(P::F32),
            _ => None,
        };
        match expr {
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                ..
            } => self
                .subroutines
                .get(name)?
                .return_type
                .as_ref()
                .and_then(|ty| match ty.kind() {
                    sv::ir::TypeKind::Real => Some(P::F64),
                    sv::ir::TypeKind::Shortreal => Some(P::F32),
                    _ => None,
                }),
            Expr::Binary {
                left,
                op: BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Pow,
                right,
            } => combine(
                self.expr_float_precision(left),
                self.expr_float_precision(right),
            ),
            Expr::Unary {
                op: sv::ir::UnaryOp::Plus | sv::ir::UnaryOp::Minus | sv::ir::UnaryOp::ToTwoState,
                expr,
            } => self.expr_float_precision(expr),
            Expr::Mux {
                then_expr,
                else_expr,
                ..
            } => combine(
                self.expr_float_precision(then_expr),
                self.expr_float_precision(else_expr),
            ),
            // A `real` or `shortreal` class property (6.12).
            Expr::Call {
                callee: sv::ir::Callee::Object(op),
                ..
            } => match &**op {
                sv::ir::ObjectOp::Load(field) if field.real => {
                    Some(if field.width == 32 { P::F32 } else { P::F64 })
                }
                _ => None,
            },
            _ => floating::precision(expr, self.variables, self.name_to_id),
        }
    }

    /// Whether an expression is signed, with each user function call typed
    /// by its declared return type.
    pub fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        let probe = if self.calls(expr) {
            self.typed_calls(expr)
        } else {
            expr.clone()
        };
        sv_expr_is_signed_with_parameters(
            &probe,
            self.variables,
            self.name_to_id,
            self.parameter_types,
        )
    }

    /// The width at which `left` and `right` are compared: the larger of
    /// their self-determined widths (IEEE 1800-2023 11.6.1 and 11.8.2), with
    /// each user function call typed by its declared return type.
    pub fn comparison_width(&self, left: &sv::ir::Expr, right: &sv::ir::Expr) -> Option<usize> {
        let typed = |expr: &sv::ir::Expr| {
            if self.calls(expr) {
                self.typed_calls(expr)
            } else {
                expr.clone()
            }
        };
        sv_comparison_operand_width(
            &typed(left),
            &typed(right),
            self.variables,
            self.name_to_id,
            self.constants,
            self.parameter_types,
        )
    }

    /// `expr` with each user function call replaced by a literal of its
    /// return type.
    pub fn typed_calls(&self, expr: &sv::ir::Expr) -> sv::ir::Expr {
        use sv::ir::Expr;
        let go = |expr: &Expr| self.typed_calls(expr);
        match expr {
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                ..
            } if self.dpi_imports.contains_key(name) => {
                let (width, signed) = self.dpi_imports[name]
                    .return_type()
                    .map_or((1, false), |r#type| (r#type.width(), r#type.is_signed()));
                Expr::Literal(typed_literal(&BigUint::zero(), width, signed))
            }
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                ..
            } if self.subroutines.contains_key(name) => {
                let (width, signed) = self
                    .subroutines
                    .get(name)
                    .and_then(|subroutine| subroutine.return_type.as_ref())
                    .and_then(|r#type| self.type_shape(r#type).ok())
                    .map_or((1, false), |(width, signed, _)| (width, signed));
                Expr::Literal(typed_literal(&BigUint::zero(), width, signed))
            }
            Expr::Ident(_) | Expr::Literal(_) => expr.clone(),
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: Box::new(go(expr)),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.clone(),
                parts: parts.iter().map(go).collect(),
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new(go(expr)),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(go(expr)),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(go(left)),
                op: *op,
                right: Box::new(go(right)),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new(go(condition)),
                then_expr: Box::new(go(then_expr)),
                else_expr: Box::new(go(else_expr)),
            },
            Expr::Call {
                callee,
                args,
                arg_order,
            } => Expr::Call {
                arg_order: arg_order.clone(),
                callee: callee.clone(),
                args: args.iter().map(|arg| arg.as_ref().map(go)).collect(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new(go(expr)),
                items: items
                    .iter()
                    .map(|item| item.map(&mut |operand| go(operand)))
                    .collect(),
            },
        }
    }

    /// The width, signedness, and state count of a declared type.
    pub fn type_shape(
        &self,
        r#type: &sv::ir::Type,
    ) -> Result<(usize, bool, bool), sv::AnalyzerError> {
        let info = signal_type_from_sv(r#type, self.constants, self.parameter_types)?;
        Ok((info.width, info.signed, info.is_4state))
    }
}

/// A sized literal with the given width, signedness, and bits.
pub(in crate::lowering) fn typed_literal(value: &BigUint, width: usize, signed: bool) -> String {
    format!(
        "{width}'{}h{}",
        if signed { "s" } else { "" },
        value.to_str_radix(16)
    )
}
