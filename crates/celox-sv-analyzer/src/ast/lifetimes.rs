//! Variable lifetimes (IEEE 1800-2023 6.21).
//!
//! A variable declared in a static task, function or block has a static
//! lifetime: it is initialized once, before simulation starts, and keeps its
//! value from one call or entry to the next. A procedural local is lowered as
//! a variable of the module that its declaration (re)initializes on every
//! entry, which is the automatic lifetime. A static local whose value on
//! entry is never read gives the same results that way. Any other static
//! local of a procedural block keeps its value instead: its declaration no
//! longer initializes it, and its initializer runs once at time zero. Static
//! locals of subroutines that keep their value are not supported.

use super::*;

/// The argument directions of each subroutine, by the names calls use.
pub(super) type Directions = HashMap<String, Vec<crate::procedural::ParamDirection>>;

/// How a static local behaves when its declaration reinitializes it.
#[derive(Debug, PartialEq, Eq)]
enum Entry {
    /// No statement reads the value it holds on entry.
    Unobserved,
    /// A statement may read the value of an earlier call or entry.
    Observed,
}

/// Give the static locals `statics` of the procedural blocks `bodies` a
/// static lifetime. Returns the locals that keep their value, and their
/// time-zero initializations.
/// `directions` gives the argument directions of the subroutines a body
/// may call.
pub(super) fn keep_static_locals<'b, T>(
    statics: &HashMap<String, T>,
    directions: &Directions,
    bodies: impl IntoIterator<Item = &'b mut Vec<Stmt>>,
) -> Result<(Vec<String>, Vec<Stmt>), AnalyzerError> {
    let mut kept = Vec::new();
    let mut initializers = Vec::new();
    if statics.is_empty() {
        return Ok((kept, initializers));
    }
    for body in bodies {
        keep_in(body, statics, directions, &mut kept, &mut initializers)?;
    }
    Ok((kept, initializers))
}

fn keep_in<T>(
    stmts: &mut Vec<Stmt>,
    statics: &HashMap<String, T>,
    directions: &Directions,
    kept: &mut Vec<String>,
    initializers: &mut Vec<Stmt>,
) -> Result<(), AnalyzerError> {
    let mut index = 0;
    while index < stmts.len() {
        if let Stmt::Local { name, init } = &stmts[index]
            && statics.contains_key(name)
            && entry(name, init.as_ref(), &stmts[index + 1..], directions)? == Entry::Observed
        {
            let Stmt::Local { name, init } = stmts.remove(index) else {
                unreachable!("the statement is a local declaration");
            };
            kept.push(name.clone());
            if let Some(init) = init {
                initializers.push(Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs: init,
                    nonblocking: false,
                });
            }
            continue;
        }
        match &mut stmts[index] {
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                keep_in(then_body, statics, directions, kept, initializers)?;
                keep_in(else_body, statics, directions, kept, initializers)?;
            }
            Stmt::Case { items, default, .. } => {
                for item in items {
                    keep_in(&mut item.body, statics, directions, kept, initializers)?;
                }
                if let Some(default) = default {
                    keep_in(default, statics, directions, kept, initializers)?;
                }
            }
            Stmt::Loop {
                init, step, body, ..
            } => {
                keep_in(init, statics, directions, kept, initializers)?;
                keep_in(step, statics, directions, kept, initializers)?;
                keep_in(body, statics, directions, kept, initializers)?;
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

/// Reject a static local of `subroutine` that keeps its value between calls.
pub(super) fn check_subroutine_statics(
    subroutine: &Subroutine,
    statics: &HashSet<String>,
    directions: &Directions,
) -> Result<(), AnalyzerError> {
    if statics.is_empty() {
        return Ok(());
    }
    let mut result = Ok(());
    let mut check = |stmts: &[Stmt]| {
        for (index, stmt) in stmts.iter().enumerate() {
            if let Stmt::Local { name, init } = stmt
                && statics.contains(name)
                && result.is_ok()
            {
                result =
                    entry(name, init.as_ref(), &stmts[index + 1..], directions).and_then(|entry| {
                        match entry {
                            Entry::Unobserved => Ok(()),
                            Entry::Observed => Err(AnalyzerError::Unsupported(format!(
                                "static variable `{}` of subroutine `{}` that keeps its value \
                             between calls (declare it `automatic`)",
                                source_name(name),
                                subroutine.name
                            ))),
                        }
                    });
            }
        }
    };
    check(&subroutine.body);
    for stmt in &subroutine.body {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                check(then_body);
                check(else_body);
            }
            Stmt::Case { items, default, .. } => {
                for item in items {
                    check(&item.body);
                }
                if let Some(default) = default {
                    check(default);
                }
            }
            Stmt::Loop {
                init, step, body, ..
            } => {
                check(init);
                check(step);
                check(body);
            }
            _ => {}
        });
    }
    result
}

/// The name a unique local name `x@N` was declared with.
fn source_name(name: &str) -> &str {
    name.rsplit_once('@').map_or(name, |(source, _)| source)
}

/// Whether the statements after the declaration of the static local `name`
/// read the value it holds on entry.
fn entry(
    name: &str,
    init: Option<&Expr>,
    after: &[Stmt],
    directions: &Directions,
) -> Result<Entry, AnalyzerError> {
    // A static initializer runs once, before simulation starts; one that is
    // not constant could only run on every entry instead.
    if init.is_some_and(|init| !expr_is_constant(init)) {
        return Err(AnalyzerError::Unsupported(format!(
            "static variable `{}` with an initializer that is not constant (declare it \
             `automatic`)",
            source_name(name)
        )));
    }
    let flow = Flow { name, directions };
    let observed = flow.block(after, false).is_err();
    Ok(match init {
        // Initialized once or on every entry, a local that is never written
        // holds its constant initial value either way.
        Some(_) if !observed || !flow.writes(after) => Entry::Unobserved,
        _ if observed => Entry::Observed,
        _ => Entry::Unobserved,
    })
}

/// A statement that may read the value a local holds on entry.
struct Observed;

/// Follows whether one local is written on every path, from its declaration
/// on.
struct Flow<'n> {
    name: &'n str,
    directions: &'n Directions,
}

/// Whether the local is written on every path that leaves some statements
/// in each way, or `None` when no path does.
#[derive(Default)]
struct Exits {
    /// After the statements.
    next: Option<bool>,
    /// At a `break`.
    breaks: Option<bool>,
    /// At a `continue`.
    continues: Option<bool>,
}

impl Exits {
    fn next(written: bool) -> Self {
        Self {
            next: Some(written),
            ..Self::default()
        }
    }

    /// The paths of `self` and of `other`.
    fn merge(self, other: Self) -> Self {
        Self {
            next: merge(self.next, other.next),
            breaks: merge(self.breaks, other.breaks),
            continues: merge(self.continues, other.continues),
        }
    }
}

/// Whether the local is written on every path of two sets of paths.
fn merge(left: Option<bool>, right: Option<bool>) -> Option<bool> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left && right),
        (one, None) | (None, one) => one,
    }
}

impl Flow<'_> {
    /// How the paths through `stmts` leave them, from `written` on entry.
    fn block(&self, stmts: &[Stmt], written: bool) -> Result<Exits, Observed> {
        let mut exits = Exits::next(written);
        for stmt in stmts {
            let Some(written) = exits.next else {
                break;
            };
            let after = self.stmt(stmt, written)?;
            exits = Exits {
                next: after.next,
                breaks: merge(exits.breaks, after.breaks),
                continues: merge(exits.continues, after.continues),
            };
        }
        Ok(exits)
    }

    fn read(&self, expr: &Expr, written: bool) -> Result<(), Observed> {
        if written || !expr_reads(expr, self.name) {
            Ok(())
        } else {
            Err(Observed)
        }
    }

    /// The local after a write of `lhs`, whose operands are read first.
    fn write(&self, lhs: &LValue, nonblocking: bool, written: bool) -> Result<bool, Observed> {
        if let LValue::Select {
            msb,
            lsb,
            array_slice_width,
            ..
        } = lhs
            && !written
            && [Some(msb), Some(lsb), array_slice_width.as_ref()]
                .into_iter()
                .flatten()
                .any(|bound| const_expr_reads(bound, self.name))
        {
            return Err(Observed);
        }
        if lhs.name() != self.name {
            return Ok(written);
        }
        // A nonblocking assignment updates the local after the block, so a
        // later entry reads it.
        if nonblocking {
            return Err(Observed);
        }
        Ok(written || matches!(lhs, LValue::Ident(_)))
    }

    /// Whether argument `index` of a call of `name` is an output, which the
    /// call writes without reading.
    fn is_output(&self, name: &str, index: usize) -> bool {
        self.directions
            .get(name)
            .and_then(|directions| directions.get(index))
            .is_some_and(|direction| *direction == crate::procedural::ParamDirection::Output)
    }

    fn stmt(&self, stmt: &Stmt, written: bool) -> Result<Exits, Observed> {
        match stmt {
            Stmt::Assign {
                lhs,
                rhs,
                nonblocking,
            } => {
                self.read(rhs, written)?;
                self.write(lhs, *nonblocking, written).map(Exits::next)
            }
            Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking,
            } => {
                self.read(rhs, written)?;
                let mut now = written;
                for part in parts {
                    now = self.write(part, *nonblocking, now)?;
                }
                Ok(Exits::next(now))
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                self.read(condition, written)?;
                Ok(self
                    .block(then_body, written)?
                    .merge(self.block(else_body, written)?))
            }
            Stmt::Case {
                selector,
                items,
                default,
                ..
            } => {
                self.read(selector, written)?;
                let mut exits = match default {
                    Some(default) => self.block(default, written)?,
                    None => Exits::next(written),
                };
                for item in items {
                    for label in &item.labels {
                        match label {
                            crate::procedural::CaseLabel::Value(value) => {
                                self.read(value, written)?
                            }
                            crate::procedural::CaseLabel::Range { low, high } => {
                                self.read(low, written)?;
                                self.read(high, written)?;
                            }
                        }
                    }
                    exits = exits.merge(self.block(&item.body, written)?);
                }
                Ok(exits)
            }
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => {
                let Some(written) = self.block(init, written)?.next else {
                    return Ok(Exits::default());
                };
                if let crate::procedural::LoopKind::Repeat(count) = kind {
                    self.read(count, written)?;
                }
                let do_while = matches!(kind, crate::procedural::LoopKind::DoWhile);
                if !do_while && let Some(condition) = condition {
                    self.read(condition, written)?;
                }
                // Later iterations only add writes, so the first one decides
                // what the loop reads of the entry value.
                let body = self.block(body, written)?;
                let stepped = merge(body.next, body.continues);
                if let Some(stepped) = stepped {
                    self.block(step, stepped)?;
                }
                if do_while {
                    // The body runs at least once: the loop ends at its
                    // condition or at a `break`.
                    if let (Some(condition), Some(stepped)) = (condition, stepped) {
                        self.read(condition, stepped)?;
                    }
                    Ok(Exits {
                        next: merge(stepped, body.breaks),
                        ..Exits::default()
                    })
                } else {
                    // The body may not run at all.
                    Ok(Exits::next(written))
                }
            }
            Stmt::Break => Ok(Exits {
                breaks: Some(written),
                ..Exits::default()
            }),
            Stmt::Continue => Ok(Exits {
                continues: Some(written),
                ..Exits::default()
            }),
            Stmt::Return(value) => {
                if let Some(value) = value {
                    self.read(value, written)?;
                }
                Ok(Exits::default())
            }
            Stmt::Call { name, args } => {
                let mut outputs = false;
                for (index, arg) in args.iter().enumerate() {
                    let Some(arg) = arg else {
                        continue;
                    };
                    if self.is_output(name, index)
                        && matches!(arg, Expr::Ident(ident) if ident == self.name)
                    {
                        outputs = true;
                    } else {
                        self.read(arg, written)?;
                    }
                }
                Ok(Exits::next(written || outputs))
            }
            Stmt::Eval(expr) => {
                self.read(expr, written)?;
                Ok(Exits::next(written))
            }
            Stmt::SystemTask { args, .. } => {
                for arg in args {
                    if let crate::procedural::SystemTaskArg::Expr(expr) = arg {
                        self.read(expr, written)?;
                    }
                }
                Ok(Exits::next(written))
            }
            Stmt::Local { init, .. } => {
                if let Some(init) = init {
                    self.read(init, written)?;
                }
                Ok(Exits::next(written))
            }
        }
    }

    /// Whether any statement of `stmts` writes the local.
    fn writes(&self, stmts: &[Stmt]) -> bool {
        let mut writes = false;
        for stmt in stmts {
            stmt.walk(&mut |stmt| match stmt {
                Stmt::Assign { lhs, .. } => writes |= lhs.name() == self.name,
                Stmt::AssignConcat { parts, .. } => {
                    writes |= parts.iter().any(|part| part.name() == self.name)
                }
                // A task may write an argument.
                Stmt::Call { args, .. } => {
                    writes |= args.iter().flatten().any(|arg| expr_reads(arg, self.name))
                }
                _ => {}
            });
        }
        writes
    }
}

fn expr_reads(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Ident(ident) => ident == name,
        Expr::Literal(_) => false,
        Expr::Select { expr, msb, lsb, .. } => {
            expr_reads(expr, name) || const_expr_reads(msb, name) || const_expr_reads(lsb, name)
        }
        Expr::Concat(parts) => parts.iter().any(|part| expr_reads(part, name)),
        Expr::RepeatConcat { count, parts } => {
            const_expr_reads(count, name) || parts.iter().any(|part| expr_reads(part, name))
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => expr_reads(expr, name),
        Expr::Binary { left, right, .. } => expr_reads(left, name) || expr_reads(right, name),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_reads(condition, name)
                || expr_reads(then_expr, name)
                || expr_reads(else_expr, name)
        }
        Expr::Call { args, .. } => args.iter().any(|arg| expr_reads(arg, name)),
        Expr::Inside { expr, items } => {
            expr_reads(expr, name)
                || items.iter().any(|item| match item {
                    InsideItem::Value(value) => expr_reads(value, name),
                    InsideItem::Range { low, high } => {
                        expr_reads(low, name) || expr_reads(high, name)
                    }
                })
        }
    }
}

fn const_expr_reads(expr: &ConstExpr, name: &str) -> bool {
    match expr {
        ConstExpr::Ident(ident) => ident == name,
        ConstExpr::Literal(_) => false,
        ConstExpr::Select { expr, bit } => {
            const_expr_reads(expr, name) || const_expr_reads(bit, name)
        }
        ConstExpr::Function { args, .. } => args.iter().any(|arg| const_expr_reads(arg, name)),
        ConstExpr::Unary { expr, .. } => const_expr_reads(expr, name),
        ConstExpr::Binary { left, right, .. } => {
            const_expr_reads(left, name) || const_expr_reads(right, name)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            const_expr_reads(condition, name)
                || const_expr_reads(then_expr, name)
                || const_expr_reads(else_expr, name)
        }
    }
}

/// Whether `expr` has the same value wherever it is evaluated: it reads no
/// variable and calls nothing that is not constant.
fn expr_is_constant(expr: &Expr) -> bool {
    fn constant(expr: &ConstExpr) -> bool {
        match expr {
            ConstExpr::Ident(_) | ConstExpr::Function { .. } => false,
            ConstExpr::Literal(_) => true,
            ConstExpr::Select { expr, bit } => constant(expr) && constant(bit),
            ConstExpr::Unary { expr, .. } => constant(expr),
            ConstExpr::Binary { left, right, .. } => constant(left) && constant(right),
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => constant(condition) && constant(then_expr) && constant(else_expr),
        }
    }
    match expr {
        Expr::Ident(_) => false,
        // A call is constant when it folds, as a pure system function or a
        // constant function of constant arguments does.
        Expr::Call { args, .. } => {
            args.iter().all(expr_is_constant)
                && expr_to_const(expr.clone())
                    .and_then(|call| eval_ast_const_expr(&call, &HashMap::default()))
                    .is_some()
        }
        Expr::Literal(_) => true,
        Expr::Select { expr, msb, lsb, .. } => {
            expr_is_constant(expr) && constant(msb) && constant(lsb)
        }
        Expr::Concat(parts) => parts.iter().all(expr_is_constant),
        Expr::RepeatConcat { count, parts } => {
            constant(count) && parts.iter().all(expr_is_constant)
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => expr_is_constant(expr),
        Expr::Binary { left, right, .. } => expr_is_constant(left) && expr_is_constant(right),
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expr_is_constant(condition)
                && expr_is_constant(then_expr)
                && expr_is_constant(else_expr)
        }
        Expr::Inside { expr, items } => {
            expr_is_constant(expr)
                && items.iter().all(|item| match item {
                    InsideItem::Value(value) => expr_is_constant(value),
                    InsideItem::Range { low, high } => {
                        expr_is_constant(low) && expr_is_constant(high)
                    }
                })
        }
    }
}

/// Whether the variables of the subroutines and blocks of the module or
/// package `node` are automatic by default: `module automatic m;`.
pub(super) fn automatic_by_default(node: &RefNode<'_>) -> bool {
    let lifetime = match node {
        RefNode::ModuleDeclarationAnsi(module) => &module.nodes.0.nodes.2,
        RefNode::ModuleDeclarationNonansi(module) => &module.nodes.0.nodes.2,
        RefNode::PackageDeclaration(package) => &package.nodes.2,
        _ => return false,
    };
    matches!(lifetime, Some(sv_parser::Lifetime::Automatic(_)))
}
