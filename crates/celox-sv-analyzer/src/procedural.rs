//! Procedural statements shared by the analyzer AST and IR.
//!
//! `always`, `initial`, and subroutine bodies keep their statement structure
//! (IEEE 1800-2023 clause 12) instead of being flattened into guarded
//! assignments, so loops, jumps, subroutine calls, and system tasks reach the
//! simulator lowering with their sequential semantics intact. The types are
//! generic over the expression and lvalue representation so the AST and the IR
//! share one definition.

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
    Break,
    Continue,
    Return(Option<E>),
    /// A user subroutine call statement. A function's result is discarded.
    /// Arguments are positional; `None` is an omitted argument.
    Call {
        name: String,
        args: Vec<Option<E>>,
    },
    /// A system function called as a statement, such as `$countones(a);`:
    /// the expression is evaluated like any other operand and its value is
    /// discarded.
    Eval(E),
    /// A system task call statement such as `$display`.
    SystemTask {
        name: String,
        args: Vec<SystemTaskArg<E>>,
    },
    /// The declaration point of a local variable: it is (re)initialized here,
    /// to `init` or to the default value of its type. A `static` one keeps
    /// its value between activations instead, and `init` runs once, before
    /// time zero (IEEE 1800-2023 6.21).
    Local {
        name: String,
        init: Option<E>,
        r#static: bool,
    },
    /// `#amount`: suspend the process for `amount` time units.
    Delay(E),
    /// `@(items)`: suspend the process until one of the events occurs.
    WaitEvent(Vec<EventItemBase<E>>),
    /// `wait (condition)`: suspend the process until the condition is true.
    Wait(E),
}

/// One item of an event control, `[edge] expr`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventItemBase<E> {
    pub edge: EventEdge,
    pub expr: E,
}

/// Which transitions of an event expression are events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventEdge {
    /// Any change of the value.
    Any,
    /// `posedge`: the least significant bit goes from 0 to 1, from 0 to
    /// unknown, or from unknown to 1 (IEEE 1800-2023 9.4.2).
    Pos,
    /// `negedge`: the least significant bit goes from 1 to 0, from 1 to
    /// unknown, or from unknown to 0.
    Neg,
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
            StmtBase::Break => StmtBase::Break,
            StmtBase::Continue => StmtBase::Continue,
            StmtBase::Return(value) => StmtBase::Return(value.map(fe)),
            StmtBase::Call { name, args } => StmtBase::Call {
                name,
                args: args.into_iter().map(|arg| arg.map(&mut *fe)).collect(),
            },
            StmtBase::Eval(expr) => StmtBase::Eval(fe(expr)),
            StmtBase::SystemTask { name, args } => StmtBase::SystemTask {
                name,
                args: args
                    .into_iter()
                    .map(|arg| match arg {
                        SystemTaskArg::Expr(expr) => SystemTaskArg::Expr(fe(expr)),
                        SystemTaskArg::Str(text) => SystemTaskArg::Str(text),
                        SystemTaskArg::Empty => SystemTaskArg::Empty,
                    })
                    .collect(),
            },
            StmtBase::Local {
                name,
                init,
                r#static,
            } => StmtBase::Local {
                r#static,
                name,
                init: init.map(fe),
            },
            StmtBase::Delay(amount) => StmtBase::Delay(fe(amount)),
            StmtBase::WaitEvent(items) => StmtBase::WaitEvent(
                items
                    .into_iter()
                    .map(|item| EventItemBase {
                        edge: item.edge,
                        expr: fe(item.expr),
                    })
                    .collect(),
            ),
            StmtBase::Wait(condition) => StmtBase::Wait(fe(condition)),
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
            StmtBase::Break | StmtBase::Continue => {}
            StmtBase::Return(value) => {
                if let Some(value) = value {
                    fe(value);
                }
            }
            StmtBase::Call { name, args } => {
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
            StmtBase::Local { name, init, .. } => {
                fname(name);
                if let Some(init) = init {
                    fe(init);
                }
            }
            StmtBase::Delay(amount) => fe(amount),
            StmtBase::WaitEvent(items) => {
                for item in items {
                    fe(&mut item.expr);
                }
            }
            StmtBase::Wait(condition) => fe(condition),
        }
    }

    /// Visit every statement of this tree, outermost first.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a StmtBase<E, L>)) {
        f(self);
        let mut visit = |stmts: &'a [StmtBase<E, L>]| {
            for stmt in stmts {
                stmt.walk(f);
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

/// A function or task without timing control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubroutineBase<E, L, T> {
    pub name: String,
    pub is_task: bool,
    /// Declared (or inherited from the module) `automatic` lifetime: each
    /// activation has its own formals and locals. A static subroutine
    /// shares them between activations (IEEE 1800-2023 13.3.1).
    pub automatic: bool,
    /// `None` for a task or a `void` function.
    pub return_type: Option<T>,
    pub params: Vec<SubroutineParamBase<E, T>>,
    /// The local that holds a function's result: the function name used as
    /// a variable inside its body. `None` for a task or a `void` function.
    pub return_var: Option<String>,
    pub body: Vec<StmtBase<E, L>>,
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
            automatic: self.automatic,
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
        }
    }
}
