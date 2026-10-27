//! Procedural statements shared by the analyzer AST and IR.
//!
//! `always`, `initial`, and subroutine bodies keep their statement structure
//! (IEEE 1800-2023 clause 12) instead of being flattened into guarded
//! assignments, so loops, jumps, subroutine calls, and system tasks reach the
//! simulator lowering with their sequential semantics intact. The types are
//! generic over the expression and lvalue representation so the AST and the IR
//! share one definition.

use crate::system_functions::SystemTask;

/// Where a construct is written: its file and byte range in the source text
/// before preprocessing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceSpan {
    pub path: std::path::PathBuf,
    pub start: usize,
    pub end: usize,
    /// The file and one-based line of the start as `__FILE__` and
    /// `__LINE__` give them, after `` `line `` directives (IEEE 1800-2023
    /// 22.12, 22.13).
    pub logical_path: std::path::PathBuf,
    pub line: usize,
}

/// Where a process is written, for diagnostics about the process as a
/// whole.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessSite {
    /// The construct the process comes from: `initial`, `always`,
    /// `always_ff`, or `declaration initializer`.
    pub construct: &'static str,
    pub path: std::path::PathBuf,
    /// One-based physical line.
    pub line: usize,
    /// One-based byte column.
    pub column: usize,
}

impl std::fmt::Display for ProcessSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.path.display(), self.line, self.column)
    }
}

/// One procedural statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StmtBase<E, L> {
    /// `lhs = rhs` (`nonblocking == false`) or `lhs <= rhs`.
    Assign {
        lhs: L,
        rhs: E,
        nonblocking: bool,
    },
    /// An assignment to a concatenation, `{a, b} = rhs`. The parts are listed
    /// from the most significant to the least significant.
    AssignConcat {
        parts: Vec<L>,
        rhs: E,
        nonblocking: bool,
    },
    If {
        condition: E,
        then_body: Vec<StmtBase<E, L>>,
        else_body: Vec<StmtBase<E, L>>,
    },
    Case {
        kind: CaseKind,
        selector: E,
        items: Vec<CaseItemBase<E, L>>,
        default: Option<Vec<StmtBase<E, L>>>,
    },
    /// Every loop form. `for` keeps its initialization and step statements;
    /// `while`, `do`-`while`, `repeat`, and `forever` have empty ones.
    Loop {
        kind: LoopKind<E>,
        init: Vec<StmtBase<E, L>>,
        condition: Option<E>,
        step: Vec<StmtBase<E, L>>,
        body: Vec<StmtBase<E, L>>,
    },
    /// Concurrent child processes (IEEE 1800-2023 9.3.2).
    Fork {
        join: celox_design::ForkJoin,
        branches: Vec<Vec<StmtBase<E, L>>>,
    },
    Break,
    Continue,
    Return(Option<E>),
    /// A user subroutine call statement. A function's result is discarded.
    /// Arguments are positional; `None` is an omitted argument.
    Call {
        name: String,
        args: Vec<Option<E>>,
        arg_order: Vec<usize>,
    },
    /// A system function called as a statement, such as `$countones(a);`:
    /// the expression is evaluated like any other operand and its value is
    /// discarded.
    Eval(E),
    /// A system task call statement such as `$display`. `site` is the call,
    /// unless the analyzer created the statement.
    SystemTask {
        task: SystemTask,
        args: Vec<SystemTaskArg<E>>,
        site: Option<SourceSpan>,
        /// For a severity task in an action block of an assertion, that
        /// assertion (IEEE 1800-2023 16.3, 20.10).
        assertion: Option<celox_design::AssertionSite>,
    },
    /// The declaration point of a local variable: it is (re)initialized here,
    /// to `init` or to the default value of its type.
    Local {
        name: String,
        init: Option<E>,
    },
    /// `#amount`: suspend the process for `amount` time units (IEEE
    /// 1800-2023 9.4.1).
    Delay {
        amount: E,
        /// Global-precision ticks in one time unit of the declaring scope.
        ticks_per_unit: u64,
        /// Global-precision ticks in one time precision step of the
        /// declaring scope, to which a real amount is rounded (3.14.1).
        ticks_per_precision: u64,
    },
    /// `@(items)`: suspend the process until one of the events occurs
    /// (9.4.2).
    WaitEvent(Vec<EventItemBase<E>>),
    /// `@*` (9.4.2.2): suspend the process until a variable that the
    /// statements read changes. They are the statements the control covers,
    /// branches that constant folding removed included, and only describe
    /// the reads: they do not execute here, and [`StmtBase::walk`] does not
    /// visit them.
    WaitImplicit(Vec<StmtBase<E, L>>),
    WaitFork,
    DisableFork,
    /// `wait (condition)`: suspend the process until the condition is true
    /// (9.4.3).
    Wait(E),
}

/// One item of an event control, `[edge] expr [iff condition]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventItemBase<E> {
    pub edge: EventEdge,
    pub expr: E,
    /// The `iff` condition: an occurrence of `expr` is an event only if it
    /// holds when the occurrence happens (9.4.2.3).
    pub iff: Option<E>,
}

/// Which transitions of an event expression are events (IEEE 1800-2023
/// 9.4.2, Table 9-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventEdge {
    /// Any change of the value.
    Any,
    /// `posedge`: the least significant bit rises.
    Pos,
    /// `negedge`: the least significant bit falls.
    Neg,
    /// `edge`: the least significant bit rises or falls.
    Both,
}

/// How a case statement compares its selector with the item labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseKind {
    /// `case`: case equality (`===`).
    Exact,
    /// `casez`: `z` and `?` bits are wildcards.
    Z,
    /// `casex`: `x`, `z`, and `?` bits are wildcards.
    X,
    /// `case ... inside`: the `inside` operator.
    Inside,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseItemBase<E, L> {
    pub labels: Vec<CaseLabel<E>>,
    pub body: Vec<StmtBase<E, L>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseLabel<E> {
    Value(E),
    /// An inclusive `[low:high]` range of a `case ... inside` item.
    Range {
        low: E,
        high: E,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopKind<E> {
    For,
    While,
    DoWhile,
    Repeat(E),
    Forever,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemTaskArg<E> {
    Expr(E),
    /// A string literal, without its quotes and with escapes left as written.
    Str(String),
    /// An omitted argument, as in `$display(a,,b)`.
    Empty,
}

/// How a subroutine argument is passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamDirection {
    Input,
    Output,
    Inout,
}

impl ParamDirection {
    /// Whether the call copies the argument's value in.
    pub fn is_read(self) -> bool {
        !matches!(self, ParamDirection::Output)
    }

    /// Whether the call writes the actual argument back.
    pub fn is_written(self) -> bool {
        !matches!(self, ParamDirection::Input)
    }
}

impl<E, L> StmtBase<E, L> {
    /// Convert the expressions and lvalues of this statement tree.
    pub fn map<E2, L2>(
        self,
        fe: &mut impl FnMut(E) -> E2,
        fl: &mut impl FnMut(L) -> L2,
    ) -> StmtBase<E2, L2> {
        fn body<E, L, E2, L2>(
            stmts: Vec<StmtBase<E, L>>,
            fe: &mut impl FnMut(E) -> E2,
            fl: &mut impl FnMut(L) -> L2,
        ) -> Vec<StmtBase<E2, L2>> {
            stmts.into_iter().map(|stmt| stmt.map(fe, fl)).collect()
        }
        match self {
            StmtBase::Assign {
                lhs,
                rhs,
                nonblocking,
            } => StmtBase::Assign {
                lhs: fl(lhs),
                rhs: fe(rhs),
                nonblocking,
            },
            StmtBase::AssignConcat {
                parts,
                rhs,
                nonblocking,
            } => StmtBase::AssignConcat {
                parts: parts.into_iter().map(&mut *fl).collect(),
                rhs: fe(rhs),
                nonblocking,
            },
            StmtBase::If {
                condition,
                then_body,
                else_body,
            } => StmtBase::If {
                condition: fe(condition),
                then_body: body(then_body, fe, fl),
                else_body: body(else_body, fe, fl),
            },
            StmtBase::Case {
                kind,
                selector,
                items,
                default,
            } => StmtBase::Case {
                kind,
                selector: fe(selector),
                items: items
                    .into_iter()
                    .map(|item| CaseItemBase {
                        labels: item
                            .labels
                            .into_iter()
                            .map(|label| match label {
                                CaseLabel::Value(value) => CaseLabel::Value(fe(value)),
                                CaseLabel::Range { low, high } => CaseLabel::Range {
                                    low: fe(low),
                                    high: fe(high),
                                },
                            })
                            .collect(),
                        body: body(item.body, fe, fl),
                    })
                    .collect(),
                default: default.map(|stmts| body(stmts, fe, fl)),
            },
            StmtBase::Loop {
                kind,
                init,
                condition,
                step,
                body: loop_body,
            } => StmtBase::Loop {
                kind: match kind {
                    LoopKind::For => LoopKind::For,
                    LoopKind::While => LoopKind::While,
                    LoopKind::DoWhile => LoopKind::DoWhile,
                    LoopKind::Repeat(count) => LoopKind::Repeat(fe(count)),
                    LoopKind::Forever => LoopKind::Forever,
                },
                init: body(init, fe, fl),
                condition: condition.map(&mut *fe),
                step: body(step, fe, fl),
                body: body(loop_body, fe, fl),
            },
            StmtBase::Fork { join, branches } => StmtBase::Fork {
                join,
                branches: branches.into_iter().map(|b| body(b, fe, fl)).collect(),
            },
            StmtBase::Break => StmtBase::Break,
            StmtBase::Continue => StmtBase::Continue,
            StmtBase::Return(value) => StmtBase::Return(value.map(fe)),
            StmtBase::Call {
                name,
                args,
                arg_order,
            } => StmtBase::Call {
                arg_order: arg_order.clone(),
                name,
                args: args.into_iter().map(|arg| arg.map(&mut *fe)).collect(),
            },
            StmtBase::Eval(expr) => StmtBase::Eval(fe(expr)),
            StmtBase::SystemTask {
                task,
                args,
                site,
                assertion,
            } => StmtBase::SystemTask {
                task,
                site,
                assertion,
                args: args
                    .into_iter()
                    .map(|arg| match arg {
                        SystemTaskArg::Expr(expr) => SystemTaskArg::Expr(fe(expr)),
                        SystemTaskArg::Str(text) => SystemTaskArg::Str(text),
                        SystemTaskArg::Empty => SystemTaskArg::Empty,
                    })
                    .collect(),
            },
            StmtBase::Local { name, init } => StmtBase::Local {
                name,
                init: init.map(fe),
            },
            StmtBase::Delay {
                amount,
                ticks_per_unit,
                ticks_per_precision,
            } => StmtBase::Delay {
                amount: fe(amount),
                ticks_per_unit,
                ticks_per_precision,
            },
            StmtBase::WaitEvent(items) => StmtBase::WaitEvent(
                items
                    .into_iter()
                    .map(|item| EventItemBase {
                        edge: item.edge,
                        expr: fe(item.expr),
                        iff: item.iff.map(&mut *fe),
                    })
                    .collect(),
            ),
            StmtBase::WaitFork => StmtBase::WaitFork,
            StmtBase::DisableFork => StmtBase::DisableFork,
            StmtBase::Wait(condition) => StmtBase::Wait(fe(condition)),
            StmtBase::WaitImplicit(reads) => StmtBase::WaitImplicit(body(reads, fe, fl)),
        }
    }

    /// Visit every expression and lvalue of this statement tree in place.
    /// Local declaration and call names are passed to `fname`.
    pub fn visit_mut(
        &mut self,
        fe: &mut impl FnMut(&mut E),
        fl: &mut impl FnMut(&mut L),
        fname: &mut impl FnMut(&mut String),
    ) {
        fn body<E, L>(
            stmts: &mut [StmtBase<E, L>],
            fe: &mut impl FnMut(&mut E),
            fl: &mut impl FnMut(&mut L),
            fname: &mut impl FnMut(&mut String),
        ) {
            for stmt in stmts {
                stmt.visit_mut(fe, fl, fname);
            }
        }
        match self {
            StmtBase::Assign { lhs, rhs, .. } => {
                fl(lhs);
                fe(rhs);
            }
            StmtBase::AssignConcat { parts, rhs, .. } => {
                parts.iter_mut().for_each(&mut *fl);
                fe(rhs);
            }
            StmtBase::If {
                condition,
                then_body,
                else_body,
            } => {
                fe(condition);
                body(then_body, fe, fl, fname);
                body(else_body, fe, fl, fname);
            }
            StmtBase::Case {
                selector,
                items,
                default,
                ..
            } => {
                fe(selector);
                for item in items {
                    for label in &mut item.labels {
                        match label {
                            CaseLabel::Value(value) => fe(value),
                            CaseLabel::Range { low, high } => {
                                fe(low);
                                fe(high);
                            }
                        }
                    }
                    body(&mut item.body, fe, fl, fname);
                }
                if let Some(default) = default {
                    body(default, fe, fl, fname);
                }
            }
            StmtBase::Loop {
                kind,
                init,
                condition,
                step,
                body: loop_body,
            } => {
                if let LoopKind::Repeat(count) = kind {
                    fe(count);
                }
                body(init, fe, fl, fname);
                if let Some(condition) = condition {
                    fe(condition);
                }
                body(step, fe, fl, fname);
                body(loop_body, fe, fl, fname);
            }
            StmtBase::Fork { branches, .. } => {
                for branch in branches {
                    body(branch, fe, fl, fname);
                }
            }
            StmtBase::Break | StmtBase::Continue => {}
            StmtBase::Return(value) => {
                if let Some(value) = value {
                    fe(value);
                }
            }
            StmtBase::Call { name, args, .. } => {
                fname(name);
                for arg in args.iter_mut().flatten() {
                    fe(arg);
                }
            }
            StmtBase::Eval(expr) => fe(expr),
            StmtBase::SystemTask { args, .. } => {
                for arg in args {
                    if let SystemTaskArg::Expr(expr) = arg {
                        fe(expr);
                    }
                }
            }
            StmtBase::Local { name, init } => {
                fname(name);
                if let Some(init) = init {
                    fe(init);
                }
            }
            StmtBase::Delay { amount, .. } => fe(amount),
            StmtBase::WaitEvent(items) => {
                for item in items {
                    fe(&mut item.expr);
                    if let Some(iff) = &mut item.iff {
                        fe(iff);
                    }
                }
            }
            StmtBase::WaitFork | StmtBase::DisableFork => {}
            StmtBase::Wait(condition) => fe(condition),
            StmtBase::WaitImplicit(reads) => body(reads, fe, fl, fname),
        }
    }

    /// Visit every statement of this tree in place, outermost first.
    pub fn walk_mut(&mut self, f: &mut impl FnMut(&mut StmtBase<E, L>)) {
        f(self);
        let mut visit = |stmts: &mut [StmtBase<E, L>]| {
            for stmt in stmts {
                stmt.walk_mut(f);
            }
        };
        match self {
            StmtBase::If {
                then_body,
                else_body,
                ..
            } => {
                visit(then_body);
                visit(else_body);
            }
            StmtBase::Case { items, default, .. } => {
                for item in items {
                    visit(&mut item.body);
                }
                if let Some(default) = default {
                    visit(default);
                }
            }
            StmtBase::Loop {
                init, step, body, ..
            } => {
                visit(init);
                visit(step);
                visit(body);
            }
            StmtBase::Fork { branches, .. } => {
                for branch in branches {
                    visit(branch);
                }
            }
            _ => {}
        }
    }

    /// Visit every statement of this tree, outermost first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a StmtBase<E, L>)) {
        self.walk_scoped(f, true);
    }

    /// Visit every statement except detached fork children.
    pub(crate) fn walk_without_detached_children<'a>(
        &'a self,
        f: &mut impl FnMut(&'a StmtBase<E, L>),
    ) {
        self.walk_scoped(f, false);
    }

    fn walk_scoped<'a>(&'a self, f: &mut impl FnMut(&'a StmtBase<E, L>), detached: bool) {
        f(self);
        let mut visit = |stmts: &'a [StmtBase<E, L>]| {
            for stmt in stmts {
                stmt.walk_scoped(f, detached);
            }
        };
        match self {
            StmtBase::If {
                then_body,
                else_body,
                ..
            } => {
                visit(then_body);
                visit(else_body);
            }
            StmtBase::Case { items, default, .. } => {
                for item in items {
                    visit(&item.body);
                }
                if let Some(default) = default {
                    visit(default);
                }
            }
            StmtBase::Loop {
                init, step, body, ..
            } => {
                visit(init);
                visit(step);
                visit(body);
            }
            StmtBase::Fork {
                join: celox_design::ForkJoin::None,
                ..
            } if !detached => {}
            StmtBase::Fork { branches, .. } => {
                for branch in branches {
                    visit(branch);
                }
            }
            _ => {}
        }
    }
}

/// A variable declared inside a procedural block or a subroutine. Its `name`
/// is unique within the module; `source_name` is the name as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVariableBase<T> {
    pub name: String,
    pub source_name: String,
    pub r#type: T,
}

/// A function or task; tasks may suspend their calling process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubroutineBase<E, L, T> {
    pub name: String,
    pub is_task: bool,
    /// Automatic storage that needs a private activation when a task suspends.
    pub automatic_locals: Vec<String>,
    /// Observable static storage shared by every call of this function.
    pub persistent_locals: Vec<String>,
    /// `None` for a task or a `void` function.
    pub return_type: Option<T>,
    pub params: Vec<SubroutineParamBase<E, T>>,
    /// The local that holds a function's result: the function name used as
    /// a variable inside its body. `None` for a task or a `void` function.
    pub return_var: Option<String>,
    pub body: Vec<StmtBase<E, L>>,
    /// Non-executable source retained for syntactic implicit sensitivity.
    pub sensitivity_body: Vec<StmtBase<E, L>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubroutineParamBase<E, T> {
    /// The module-unique local name of the formal argument.
    pub name: String,
    pub source_name: String,
    pub direction: ParamDirection,
    pub r#type: T,
    pub default: Option<E>,
}

impl<E, L, T> SubroutineBase<E, L, T> {
    pub fn map<E2, L2, T2>(
        self,
        fe: &mut impl FnMut(E) -> E2,
        fl: &mut impl FnMut(L) -> L2,
        ft: &mut impl FnMut(T) -> T2,
    ) -> SubroutineBase<E2, L2, T2> {
        SubroutineBase {
            name: self.name,
            is_task: self.is_task,
            automatic_locals: self.automatic_locals,
            persistent_locals: self.persistent_locals,
            return_type: self.return_type.map(&mut *ft),
            params: self
                .params
                .into_iter()
                .map(|param| SubroutineParamBase {
                    name: param.name,
                    source_name: param.source_name,
                    direction: param.direction,
                    r#type: ft(param.r#type),
                    default: param.default.map(&mut *fe),
                })
                .collect(),
            return_var: self.return_var,
            body: self.body.into_iter().map(|stmt| stmt.map(fe, fl)).collect(),
            sensitivity_body: self
                .sensitivity_body
                .into_iter()
                .map(|stmt| stmt.map(fe, fl))
                .collect(),
        }
    }
}
