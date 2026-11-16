//! Loop shapes: the counted loops that lower to a fold, and the jumps that
//! leave a loop body early.

use super::*;

/// The `for` loop shapes that lower to a counted SLT fold:
/// `for (v = start; v < end; v += step)` and its relatives.
#[derive(Debug, Clone)]
pub(in crate::lowering) struct CanonicalLoop {
    pub var: String,
    pub start: sv::ir::Expr,
    pub end: sv::ir::Expr,
    pub compare: sv::ir::BinaryOp,
    pub step: usize,
    pub step_op: celox_slt::SLTStepOp,
    pub decreasing: bool,
}

pub(in crate::lowering) fn literal_usize(expr: &sv::ir::Expr) -> Option<usize> {
    let sv::ir::Expr::Literal(text) = expr else {
        return None;
    };
    let literal = sv::typecheck::parse_integral_literal(text)?;
    if !literal.mask.is_zero() {
        return None;
    }
    literal.value.to_usize()
}

pub(in crate::lowering) fn canonical_for_loop(
    init: &[sv::ir::Stmt],
    condition: Option<&sv::ir::Expr>,
    step: &[sv::ir::Stmt],
) -> Option<CanonicalLoop> {
    let shape = counted_for_loop(init, condition, step)?;
    let step = shape.integral_step()?;
    Some(CanonicalLoop {
        var: shape.var.to_owned(),
        start: shape.start.clone(),
        end: shape.end.clone(),
        compare: shape.compare,
        step,
        step_op: shape.step_op,
        decreasing: shape.decreasing,
    })
}

pub(in crate::lowering) struct CountedForLoop<'a> {
    pub var: &'a str,
    start: &'a sv::ir::Expr,
    end: &'a sv::ir::Expr,
    compare: sv::ir::BinaryOp,
    step: &'a sv::ir::Expr,
    step_op: celox_slt::SLTStepOp,
    decreasing: bool,
}

impl CountedForLoop<'_> {
    pub fn integral_step(&self) -> Option<usize> {
        let step = literal_usize(self.step)?;
        (step != 0 || self.step_op != celox_slt::SLTStepOp::Add).then_some(step)
    }

    pub fn has_real_step(&self) -> bool {
        self.step_op == celox_slt::SLTStepOp::Add
            && matches!(self.step, sv::ir::Expr::Literal(text)
                if text.replace('_', "").parse::<f64>()
                    .is_ok_and(|step| step.is_finite() && step > 0.0))
    }
}

pub(in crate::lowering) fn counted_for_loop<'a>(
    init: &'a [sv::ir::Stmt],
    condition: Option<&'a sv::ir::Expr>,
    step: &'a [sv::ir::Stmt],
) -> Option<CountedForLoop<'a>> {
    let (var, start) = match init {
        [
            sv::ir::Stmt::Local {
                name,
                init: Some(start),
            },
        ] => (name, start),
        [
            sv::ir::Stmt::Assign {
                lhs: sv::ir::LValue::Ident(name),
                rhs,
                nonblocking: false,
            },
        ] => (name, rhs),
        _ => return None,
    };
    let [
        sv::ir::Stmt::Assign {
            lhs: sv::ir::LValue::Ident(step_target),
            rhs:
                sv::ir::Expr::Binary {
                    left: step_left,
                    op: step_binary,
                    right: step_right,
                },
            nonblocking: false,
        },
    ] = step
    else {
        return None;
    };
    if step_target != var || !matches!(&**step_left, sv::ir::Expr::Ident(name) if name == var) {
        return None;
    }
    let (step_op, decreasing) = match step_binary {
        sv::ir::BinaryOp::Add => (celox_slt::SLTStepOp::Add, false),
        sv::ir::BinaryOp::Sub => (celox_slt::SLTStepOp::Add, true),
        sv::ir::BinaryOp::Mul => (celox_slt::SLTStepOp::Mul, false),
        sv::ir::BinaryOp::Shl => (celox_slt::SLTStepOp::Shl, false),
        sv::ir::BinaryOp::BitOr => (celox_slt::SLTStepOp::BitOr, false),
        sv::ir::BinaryOp::BitXor => (celox_slt::SLTStepOp::BitXor, false),
        _ => return None,
    };
    let sv::ir::Expr::Binary { left, op, right } = condition? else {
        return None;
    };
    let is_var = |expr: &sv::ir::Expr| matches!(expr, sv::ir::Expr::Ident(name) if name == var);
    let (compare, end) = if is_var(left) {
        (*op, &**right)
    } else if is_var(right) {
        let flipped = match op {
            sv::ir::BinaryOp::Lt => sv::ir::BinaryOp::Gt,
            sv::ir::BinaryOp::Le => sv::ir::BinaryOp::Ge,
            sv::ir::BinaryOp::Gt => sv::ir::BinaryOp::Lt,
            sv::ir::BinaryOp::Ge => sv::ir::BinaryOp::Le,
            _ => return None,
        };
        (flipped, &**left)
    } else {
        return None;
    };
    let valid = match compare {
        sv::ir::BinaryOp::Lt | sv::ir::BinaryOp::Le => !decreasing,
        sv::ir::BinaryOp::Gt | sv::ir::BinaryOp::Ge => {
            decreasing && step_op == celox_slt::SLTStepOp::Add
        }
        _ => false,
    };
    valid.then_some(CountedForLoop {
        var,
        start,
        end,
        compare,
        step: step_right,
        step_op,
        decreasing,
    })
}

/// The counted loop a `while` or `do`-`while` loop is when its condition
/// compares a variable with a bound and its last statement steps that
/// variable, as the step of a [`canonical_for_loop`] would; the loop starts
/// from the variable's current value. Also returns the statements before
/// the step, which must not write the variable or the bound.
pub(in crate::lowering) fn counted_while_loop<'s>(
    condition: Option<&sv::ir::Expr>,
    body: &'s [sv::ir::Stmt],
) -> Option<(CanonicalLoop, &'s [sv::ir::Stmt])> {
    let (step, rest) = body.split_last()?;
    let sv::ir::Stmt::Assign {
        lhs: sv::ir::LValue::Ident(var),
        ..
    } = step
    else {
        return None;
    };
    let init = [sv::ir::Stmt::Assign {
        lhs: sv::ir::LValue::Ident(var.clone()),
        rhs: sv::ir::Expr::Ident(var.clone()),
        nonblocking: false,
    }];
    let canonical = canonical_for_loop(&init, condition, std::slice::from_ref(step))?;
    let mut written = HashSet::default();
    written_names(rest, &mut written);
    let mut bound = HashSet::default();
    expr_idents(&canonical.end, &mut bound);
    (!written.contains(&canonical.var) && written.is_disjoint(&bound)).then_some((canonical, rest))
}

/// Whether a statement list may leave its enclosing block early: a `break`
/// or `continue` of the innermost loop, or a `return`.
pub(in crate::lowering) fn stmts_may_jump(stmts: &[sv::ir::Stmt], in_loop: bool) -> bool {
    stmts.iter().any(|stmt| stmt_may_jump(stmt, in_loop))
}

pub(in crate::lowering) fn stmt_may_jump(stmt: &sv::ir::Stmt, in_loop: bool) -> bool {
    match stmt {
        sv::ir::Stmt::Break | sv::ir::Stmt::Continue => !in_loop,
        sv::ir::Stmt::Return(_) | sv::ir::Stmt::DisableBlock(_) => true,
        sv::ir::Stmt::Block { body, .. } => stmts_may_jump(body, in_loop),
        sv::ir::Stmt::If {
            then_body,
            else_body,
            ..
        } => stmts_may_jump(then_body, in_loop) || stmts_may_jump(else_body, in_loop),
        sv::ir::Stmt::Case { items, default, .. } => {
            items.iter().any(|item| stmts_may_jump(&item.body, in_loop))
                || default
                    .as_ref()
                    .is_some_and(|body| stmts_may_jump(body, in_loop))
        }
        // A `break` or `continue` inside a nested loop belongs to it.
        sv::ir::Stmt::Loop { body, .. } => stmts_may_jump(body, true),
        _ => false,
    }
}

/// Whether `body` writes variable `name`, by an assignment or as a written
/// argument of a call, in a statement or an expression: of a system
/// subroutine such as `$sscanf`, of a user subroutine (an output, inout or
/// ref formal) or of a DPI-C import (IEEE 1800-2023 13.5, 21.3.4.3). A loop
/// whose body writes its variable does not take the values its step gives.
pub(in crate::lowering) fn body_writes(
    calls: &(impl CallDefinitions + ?Sized),
    body: &[sv::ir::Stmt],
    name: &str,
) -> bool {
    let mut written = HashSet::default();
    written_names(body, &mut written);
    written.contains(name) || kernel_writes_name(calls, body, name, &mut HashSet::default(), 0)
}

/// The variables of the `for` loops around a statement, innermost last
/// (see [`LoopBounds::walk`]).
pub(in crate::lowering) type LoopValues = Vec<LoopVariable>;

/// The variable of a `for` loop, its type and the values it takes in the
/// iterations of the loop's body.
pub(in crate::lowering) struct LoopVariable {
    name: String,
    width: usize,
    signed: bool,
    values: Vec<i128>,
}

impl LoopVariable {
    /// `value` as a literal of the variable's type, which an expression
    /// that reads the variable sizes as it sizes the variable.
    fn literal(&self, value: i128) -> Option<sv::ir::ConstExpr> {
        let bits = u128::try_from(value.rem_euclid(1i128 << self.width)).ok()?;
        Some(sv::ir::ConstExpr::Literal(typed_literal(
            &BigUint::from(bits),
            self.width,
            self.signed,
        )))
    }
}

/// What the bounds of `for` loops are evaluated with, to find the values
/// their variables take.
pub(in crate::lowering) struct LoopBounds<'a> {
    pub constants: &'a HashMap<String, i128>,
    pub parameter_types: &'a HashMap<String, (usize, bool)>,
    /// The width and signedness of an integral variable that a loop
    /// declares.
    pub variable_type: &'a dyn Fn(&str) -> Option<(usize, bool)>,
    /// The subroutines and DPI-C imports that calls in a loop's body may
    /// write its variable through.
    pub calls: &'a dyn CallDefinitions,
}

impl LoopBounds<'_> {
    /// Visit each statement of `stmts`, outermost first, as `Stmt::walk`
    /// does, with the values the variables of the `for` loops around it take
    /// (see [`Self::values`]) appended to `loops`.
    pub(in crate::lowering) fn walk<'s>(
        &self,
        stmts: &'s [sv::ir::Stmt],
        loops: &mut LoopValues,
        f: &mut dyn FnMut(&'s sv::ir::Stmt, &LoopValues),
    ) {
        for stmt in stmts {
            self.walk_stmt(stmt, loops, f);
        }
    }

    fn walk_stmt<'s>(
        &self,
        stmt: &'s sv::ir::Stmt,
        loops: &mut LoopValues,
        f: &mut dyn FnMut(&'s sv::ir::Stmt, &LoopValues),
    ) {
        f(stmt, loops);
        match stmt {
            sv::ir::Stmt::DelayedNonblocking {
                control,
                assignment,
            } => {
                self.walk_stmt(control, loops, f);
                self.walk_stmt(assignment, loops, f);
            }
            sv::ir::Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                self.walk(then_body, loops, f);
                self.walk(else_body, loops, f);
            }
            sv::ir::Stmt::Case { items, default, .. } => {
                for item in items {
                    self.walk(&item.body, loops, f);
                }
                if let Some(default) = default {
                    self.walk(default, loops, f);
                }
            }
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => {
                self.walk(init, loops, f);
                self.walk(step, loops, f);
                let values = matches!(kind, sv::ir::LoopKind::For)
                    .then(|| self.values(init, condition.as_ref(), step, body))
                    .flatten();
                let bound = values.is_some();
                loops.extend(values);
                self.walk(body, loops, f);
                if bound {
                    loops.pop();
                }
            }
            sv::ir::Stmt::Fork { branches, .. } => {
                for branch in branches {
                    self.walk(branch, loops, f);
                }
            }
            sv::ir::Stmt::Block { body, .. } => self.walk(body, loops, f),
            _ => {}
        }
    }

    /// The values the variable of a counted `for` loop takes in the
    /// iterations of its body, when the loop declares the variable, its
    /// body does not write it, also not as a written argument of a call,
    /// and its bounds are constant: the loop runs them in order whatever
    /// other processes do (IEEE 1800-2023 12.7.1). The values wrap as the
    /// variable's type does.
    fn values(
        &self,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Option<LoopVariable> {
        let [sv::ir::Stmt::Local { .. }] = init else {
            return None;
        };
        let canonical = canonical_for_loop(init, condition, step)?;
        if body_writes(self.calls, body, &canonical.var) {
            return None;
        }
        let (width, signed) = (self.variable_type)(&canonical.var)?;
        if width == 0 || width > 64 {
            return None;
        }
        let [sv::ir::Stmt::Assign { rhs: next, .. }] = step else {
            return None;
        };
        let condition = condition?;
        let modulus = 1i128 << width;
        let wrap = |value: i128| {
            let bits = value.rem_euclid(modulus);
            if signed && bits >= modulus / 2 {
                bits - modulus
            } else {
                bits
            }
        };
        let constant = |expr: &sv::ir::Expr| {
            sv::typecheck::eval_const_expr_with_types(
                &expr.to_const()?,
                self.constants,
                self.parameter_types,
            )
        };
        // The expression with the variable a literal of its type.
        let at = |expr: &sv::ir::Expr, value: i128| {
            let bits = u128::try_from(value.rem_euclid(modulus)).ok()?;
            let literal = typed_literal(&BigUint::from(bits), width, signed);
            constant(&expr.substitute(&canonical.var, &sv::ir::Expr::Literal(literal))?)
        };
        let mut value = wrap(constant(&canonical.start)?);
        let mut values = Vec::new();
        while at(condition, value)? != 0 {
            if values.len() == MAX_UNROLLED_ITERATIONS {
                return None;
            }
            values.push(value);
            value = wrap(at(next, value)?);
        }
        Some(LoopVariable {
            name: canonical.var,
            width,
            signed,
            values,
        })
    }
}

/// The bounds `msb` and `lsb` of a select at each combination of the values
/// the loop variables of `loops` it reads take, when they are constant
/// then; `None` if it reads none of them, or no iteration runs. Each
/// variable is a literal of its type there, so that the bounds wrap as
/// their operands' widths make them (IEEE 1800-2023 11.6.1).
pub(in crate::lowering) fn unrolled_select_bounds(
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    loops: &LoopValues,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<Vec<(i128, i128)>> {
    // The innermost loop of each variable the bounds read.
    let mut read: Vec<&LoopVariable> = Vec::new();
    for variable in loops.iter().rev() {
        if (msb.reads(&variable.name) || lsb.reads(&variable.name))
            && !read.iter().any(|seen| seen.name == variable.name)
        {
            read.push(variable);
        }
    }
    let total = read.iter().try_fold(1usize, |total, variable| {
        total.checked_mul(variable.values.len())
    })?;
    if read.is_empty() || total == 0 || total > MAX_UNROLLED_ITERATIONS {
        return None;
    }
    let mut bounds = Vec::with_capacity(total);
    let mut indices = vec![0; read.len()];
    loop {
        let (mut at_msb, mut at_lsb) = (msb.clone(), lsb.clone());
        for (variable, &index) in read.iter().zip(&indices) {
            let literal = variable.literal(variable.values[index])?;
            at_msb = at_msb.substitute(&variable.name, &literal);
            at_lsb = at_lsb.substitute(&variable.name, &literal);
        }
        bounds.push((
            sv::typecheck::eval_const_expr_with_types(&at_msb, constants, parameter_types)?,
            sv::typecheck::eval_const_expr_with_types(&at_lsb, constants, parameter_types)?,
        ));
        // The next combination, the innermost loop first.
        let mut digit = 0;
        loop {
            let Some(index) = indices.get_mut(digit) else {
                return Some(bounds);
            };
            *index += 1;
            if *index < read[digit].values.len() {
                break;
            }
            *index = 0;
            digit += 1;
        }
    }
}
