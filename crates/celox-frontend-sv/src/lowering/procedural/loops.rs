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
    let (var, start) = match init {
        [
            sv::ir::Stmt::Local {
                name,
                init: Some(start),
            },
        ] => (name.clone(), start.clone()),
        [
            sv::ir::Stmt::Assign {
                lhs: sv::ir::LValue::Ident(name),
                rhs,
                nonblocking: false,
            },
        ] => (name.clone(), rhs.clone()),
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
    if *step_target != var || !matches!(&**step_left, sv::ir::Expr::Ident(name) if *name == var) {
        return None;
    }
    let step = literal_usize(step_right)?;
    let (step_op, decreasing) = match step_binary {
        sv::ir::BinaryOp::Add => (celox_slt::SLTStepOp::Add, false),
        sv::ir::BinaryOp::Sub => (celox_slt::SLTStepOp::Add, true),
        sv::ir::BinaryOp::Mul => (celox_slt::SLTStepOp::Mul, false),
        sv::ir::BinaryOp::Shl => (celox_slt::SLTStepOp::Shl, false),
        sv::ir::BinaryOp::BitOr => (celox_slt::SLTStepOp::BitOr, false),
        sv::ir::BinaryOp::BitXor => (celox_slt::SLTStepOp::BitXor, false),
        _ => return None,
    };
    if step == 0 && step_op == celox_slt::SLTStepOp::Add {
        return None;
    }
    let sv::ir::Expr::Binary { left, op, right } = condition? else {
        return None;
    };
    let is_var = |expr: &sv::ir::Expr| matches!(expr, sv::ir::Expr::Ident(name) if *name == var);
    let (compare, end) = if is_var(left) {
        (*op, (**right).clone())
    } else if is_var(right) {
        let flipped = match op {
            sv::ir::BinaryOp::Lt => sv::ir::BinaryOp::Gt,
            sv::ir::BinaryOp::Le => sv::ir::BinaryOp::Ge,
            sv::ir::BinaryOp::Gt => sv::ir::BinaryOp::Lt,
            sv::ir::BinaryOp::Ge => sv::ir::BinaryOp::Le,
            _ => return None,
        };
        (flipped, (**left).clone())
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
    valid.then_some(CanonicalLoop {
        var,
        start,
        end,
        compare,
        step,
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

/// The values that the variables of the `for` loops around a statement
/// take, innermost last (see [`LoopBounds::walk`]).
pub(in crate::lowering) type LoopValues = Vec<(String, Vec<i128>)>;

/// What the bounds of `for` loops are evaluated with, to find the values
/// their variables take.
pub(in crate::lowering) struct LoopBounds<'a> {
    pub constants: &'a HashMap<String, i128>,
    pub parameter_types: &'a HashMap<String, (usize, bool)>,
    /// The width and signedness of an integral variable that a loop
    /// declares.
    pub variable_type: &'a dyn Fn(&str) -> Option<(usize, bool)>,
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
    /// body does not write it, and its bounds are constant: the loop runs
    /// them in order whatever other processes do (IEEE 1800-2023 12.7.1).
    /// The values wrap as the variable's type does.
    fn values(
        &self,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Option<(String, Vec<i128>)> {
        let [sv::ir::Stmt::Local { .. }] = init else {
            return None;
        };
        let canonical = canonical_for_loop(init, condition, step)?;
        let mut written = HashSet::default();
        written_names(body, &mut written);
        if written.contains(&canonical.var) {
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
        Some((canonical.var, values))
    }
}

/// The bounds `msb` and `lsb` of a select at each combination of the values
/// the loop variables of `loops` it reads take, when they are constant
/// then; `None` if it reads none of them, or no iteration runs.
pub(in crate::lowering) fn unrolled_select_bounds(
    msb: &sv::ir::ConstExpr,
    lsb: &sv::ir::ConstExpr,
    loops: &LoopValues,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> Option<Vec<(i128, i128)>> {
    /// `constants` with the loop variables at the values of one iteration.
    struct Iteration<'a> {
        constants: &'a HashMap<String, i128>,
        values: &'a [(&'a str, i128)],
    }
    impl sv::typecheck::Constants for Iteration<'_> {
        fn constant(&self, name: &str) -> Option<&i128> {
            self.values
                .iter()
                .find(|(variable, _)| *variable == name)
                .map(|(_, value)| value)
                .or_else(|| self.constants.get(name))
        }
    }
    // The innermost loop of each variable the bounds read.
    let mut read: Vec<(&str, &[i128])> = Vec::new();
    for (name, values) in loops.iter().rev() {
        if (msb.reads(name) || lsb.reads(name))
            && !read.iter().any(|(seen, _)| *seen == name.as_str())
        {
            read.push((name.as_str(), values.as_slice()));
        }
    }
    let total = read
        .iter()
        .try_fold(1usize, |total, (_, values)| total.checked_mul(values.len()))?;
    if read.is_empty() || total == 0 || total > MAX_UNROLLED_ITERATIONS {
        return None;
    }
    let mut bounds = Vec::with_capacity(total);
    let mut indices = vec![0; read.len()];
    let mut values = Vec::with_capacity(read.len());
    loop {
        values.clear();
        values.extend(
            read.iter()
                .zip(&indices)
                .map(|((name, values), &index)| (*name, values[index])),
        );
        let iteration = Iteration {
            constants,
            values: &values,
        };
        bounds.push((
            sv::typecheck::eval_const_expr_with_types(msb, &iteration, parameter_types)?,
            sv::typecheck::eval_const_expr_with_types(lsb, &iteration, parameter_types)?,
        ));
        // The next combination, the innermost loop first.
        let mut digit = 0;
        loop {
            let Some(index) = indices.get_mut(digit) else {
                return Some(bounds);
            };
            *index += 1;
            if *index < read[digit].1.len() {
                break;
            }
            *index = 0;
            digit += 1;
        }
    }
}
