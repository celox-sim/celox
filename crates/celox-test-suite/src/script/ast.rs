//! The test script language: cases, statements and expressions.
//!
//! A script file holds one group:
//!
//! ```text
//! (group operators (category operators))
//! (case test_ternary_operator
//!   (source "test.veryl" #"module Top (...) { ... }"#)
//!   (top Top)
//!   (modify (set a 0xaa) (set b 0xbb))
//!   (for sel (list 0 1)
//!     (modify (set sel sel))
//!     (assert_eq o (? sel 0xaa 0xbb))))
//! ```
//!
//! Case clauses come first: `(source PATH TEXT)` (one or more), `(top NAME)`,
//! and optionally `(expect reject)`, `(four_state)`, `(tags TAG...)`,
//! `(parameter NAME VALUE)` (the top module's parameter values) and
//! `(category NAME)`. The statements that follow drive the design.
//!
//! Script values are integers of unbounded width; a value read from a signal
//! is its unsigned bit pattern, and may carry unknown (X/Z) bits. Arithmetic
//! on unknown bits is an error, so expectations never depend on the
//! simulator's own width or X rules. See the module documentation of
//! [`super`] for the statement and expression reference.

use super::sexpr::{Pos, Sexpr};
use crate::{Category, Expectation, Instance, SignalPath, TestTag};
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{Num, Zero};
use std::fmt;
use std::path::PathBuf;

/// A script value: `payload` with unknown bits flagged in `mask`, using the
/// backend encoding 0=(0,0), 1=(1,0), X=(1,1), Z=(0,1). Known values may be
/// negative; a value with unknown bits is a non-negative bit pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    pub payload: BigInt,
    pub mask: BigUint,
}

impl Value {
    pub fn known(payload: impl Into<BigInt>) -> Self {
        Self {
            payload: payload.into(),
            mask: BigUint::zero(),
        }
    }

    pub fn is_known(&self) -> bool {
        self.mask.is_zero()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    And,
    Or,
    Xor,
    Not,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    LogicNot,
    LogicAnd,
    LogicOr,
    Cond,
    Min,
    Max,
    /// `(trunc W v)`: `v` modulo 2^W.
    Trunc,
    /// `(sext W v)`: the low W bits of `v` as a signed number.
    Sext,
    /// `(slice v HI LO)`: bits HI..=LO of `v`.
    Slice,
    /// `(bit v I)`: bit I of `v`.
    Bit,
    /// `(cat W1 v1 W2 v2 ...)`: concatenation, most significant first.
    Cat,
    /// `(rep N W v)`: N copies of the low W bits of `v`.
    Rep,
    /// `(fourstate P M)`: a value with payload P and unknown mask M.
    FourState,
    /// `(payload v)`, `(xmask v)`: the two halves of a value.
    Payload,
    XMask,
    /// `(known v)`: 1 when `v` has no unknown bits.
    Known,
    /// `(popcount v)`: the number of one bits of a non-negative value.
    Popcount,
    /// `(bitlen v)`: the bits needed for a non-negative value (0 for 0).
    Bitlen,
}

impl Op {
    fn from_name(name: &str) -> Option<(Op, usize, Option<usize>)> {
        // (operator, minimum operands, maximum operands)
        Some(match name {
            "+" => (Op::Add, 2, None),
            "-" => (Op::Sub, 1, Some(2)),
            "*" => (Op::Mul, 2, None),
            "/" => (Op::Div, 2, Some(2)),
            "%" => (Op::Rem, 2, Some(2)),
            "**" => (Op::Pow, 2, Some(2)),
            "&" => (Op::And, 2, None),
            "|" => (Op::Or, 2, None),
            "^" => (Op::Xor, 2, None),
            "~" => (Op::Not, 1, Some(1)),
            "<<" => (Op::Shl, 2, Some(2)),
            ">>" => (Op::Shr, 2, Some(2)),
            "==" => (Op::Eq, 2, Some(2)),
            "!=" => (Op::Ne, 2, Some(2)),
            "<" => (Op::Lt, 2, Some(2)),
            "<=" => (Op::Le, 2, Some(2)),
            ">" => (Op::Gt, 2, Some(2)),
            ">=" => (Op::Ge, 2, Some(2)),
            "!" => (Op::LogicNot, 1, Some(1)),
            "&&" => (Op::LogicAnd, 2, None),
            "||" => (Op::LogicOr, 2, None),
            "?" => (Op::Cond, 3, Some(3)),
            "min" => (Op::Min, 2, None),
            "max" => (Op::Max, 2, None),
            "trunc" => (Op::Trunc, 2, Some(2)),
            "sext" => (Op::Sext, 2, Some(2)),
            "slice" => (Op::Slice, 3, Some(3)),
            "bit" => (Op::Bit, 2, Some(2)),
            "cat" => (Op::Cat, 2, None),
            "rep" => (Op::Rep, 3, Some(3)),
            "fourstate" => (Op::FourState, 2, Some(2)),
            "payload" => (Op::Payload, 1, Some(1)),
            "xmask" => (Op::XMask, 1, Some(1)),
            "known" => (Op::Known, 1, Some(1)),
            "popcount" => (Op::Popcount, 1, Some(1)),
            "bitlen" => (Op::Bitlen, 1, Some(1)),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    Literal(Value),
    Var(String),
    Get(SignalPath),
    Op(Op, Vec<Expr>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sequence {
    /// `(range START END [STEP])`, END exclusive.
    Range(Expr, Expr, Option<Expr>),
    /// `(list e...)`, or `(list (e1 e2) ...)` for a tuple of loop variables.
    List(Vec<Vec<Expr>>),
}

/// What `assert_eq` compares: a bare signal name, or any expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actual {
    Signal(SignalPath),
    Expr(Expr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub pos: Pos,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StmtKind {
    /// `(set SIGNAL EXPR)`: stage a write; `eval`, a read or a tick settles it.
    Set(SignalPath, Expr),
    /// `(eval)`: settle staged writes.
    Eval,
    /// `(modify STMT...)`: run the statements, then `eval`.
    Modify(Vec<Stmt>),
    /// `(tick EVENT [COUNT])`
    Tick(String, Expr),
    /// `(assert_eq ACTUAL EXPR [MESSAGE])`: ACTUAL equals EXPR exactly,
    /// including unknown bits. A bare name as ACTUAL is a signal.
    AssertEq(Actual, Expr, Option<String>),
    /// `(assert EXPR [MESSAGE])`
    Assert(Expr, Option<String>),
    /// `(let NAME EXPR)`: a variable for the rest of the enclosing block.
    Let(String, Expr),
    /// `(set! NAME EXPR)`: update a variable.
    Update(String, Expr),
    /// `(for NAME SEQUENCE STMT...)` or `(for (NAME...) (list (E...)...) STMT...)`
    For(Vec<String>, Sequence, Vec<Stmt>),
    /// `(if EXPR STMT [STMT])`
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    /// `(do STMT...)`
    Block(Vec<Stmt>),
    /// `(run_testbench)`: run the design's native testbench to `$finish`.
    RunTestbench,
    /// `(expect_output TEXT)`: the design printed exactly TEXT with
    /// `$display`/`$write` since the start or the previous `expect_output`.
    ExpectOutput(String),
}

/// Part of a source file: text, or a file of the Veryl standard library
/// (`(std "fifo/fifo.veryl")`) read when the design is built.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourcePart {
    Text(String),
    Std(String),
}

/// One test case of a script group.
#[derive(Clone, Debug)]
pub struct ScriptCase {
    /// `group::name`
    pub name: String,
    pub category: Category,
    pub expectation: Expectation,
    pub tags: Vec<TestTag>,
    pub four_state: bool,
    /// Each source file is its parts joined by newlines.
    pub sources: Vec<(PathBuf, Vec<SourcePart>)>,
    pub top: String,
    /// `(parameter NAME VALUE)`: values of the top module's parameters.
    pub parameters: Vec<(String, u64)>,
    pub body: Vec<Stmt>,
    pub pos: Pos,
}

#[derive(Debug)]
pub struct ScriptError {
    pub pos: Pos,
    pub message: String,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.pos, self.message)
    }
}

impl std::error::Error for ScriptError {}

fn error<T>(pos: Pos, message: impl Into<String>) -> Result<T, ScriptError> {
    Err(ScriptError {
        pos,
        message: message.into(),
    })
}

fn category(name: &str, pos: Pos) -> Result<Category, ScriptError> {
    Ok(match name {
        "combinational" => Category::Combinational,
        "operators" => Category::Operators,
        "types" => Category::Types,
        "arrays" => Category::Arrays,
        "hierarchy" => Category::Hierarchy,
        "sequential" => Category::Sequential,
        "four_state" => Category::FourState,
        "functions" => Category::Functions,
        "control_flow" => Category::ControlFlow,
        "standard_library" => Category::StandardLibrary,
        "regression" => Category::Regression,
        _ => return error(pos, format!("unknown category `{name}`")),
    })
}

fn tag(name: &str, pos: Pos) -> Result<TestTag, ScriptError> {
    Ok(match name {
        "evaluation_order" => TestTag::EvaluationOrder,
        "assignment_pattern_evaluation" => TestTag::AssignmentPatternEvaluation,
        "deferred_function_effects" => TestTag::DeferredFunctionEffects,
        "eager_assertion_messages" => TestTag::EagerAssertionMessages,
        "two_state_zero_division" => TestTag::TwoStateZeroDivision,
        "two_state_initialization" => TestTag::TwoStateInitialization,
        _ => return error(pos, format!("unknown tag `{name}`")),
    })
}

/// Parse an integer literal: decimal, `0x`/`0o`/`0b` prefixed, or sized
/// `N'b...` / `N'o...` / `N'h...` / `N'd...`, where binary, octal and hex
/// digits may be `x` or `z`. `_` separates digits.
pub fn literal(text: &str) -> Option<Value> {
    let text = text.replace('_', "");
    if let Some((width, rest)) = text.split_once('\'') {
        let width: usize = width.parse().ok()?;
        let mut chars = rest.chars();
        let (bits_per_digit, radix) = match chars.next()?.to_ascii_lowercase() {
            'b' => (1, 2),
            'o' => (3, 8),
            'h' => (4, 16),
            'd' => {
                let value = BigUint::from_str_radix(chars.as_str(), 10).ok()?;
                return (value.bits() as usize <= width).then(|| Value::known(value));
            }
            _ => return None,
        };
        let digits = chars.as_str();
        if digits.is_empty() {
            return None;
        }
        let mut payload = BigUint::zero();
        let mut mask = BigUint::zero();
        let all = (BigUint::from(1u8) << bits_per_digit) - 1u8;
        for digit in digits.chars() {
            payload <<= bits_per_digit;
            mask <<= bits_per_digit;
            match digit.to_ascii_lowercase() {
                'x' => {
                    payload |= &all;
                    mask |= &all;
                }
                'z' => mask |= &all,
                digit => payload |= BigUint::from(digit.to_digit(radix)?),
            }
        }
        let limit = BigUint::from(1u8) << width;
        if payload >= limit || mask >= limit {
            return None;
        }
        return Some(Value {
            payload: BigInt::from_biguint(Sign::Plus, payload),
            mask,
        });
    }
    let (negative, digits) = match text.strip_prefix('-') {
        Some(digits) => (true, digits),
        None => (false, text.as_str()),
    };
    let (radix, digits) = if let Some(d) = digits.strip_prefix("0x") {
        (16, d)
    } else if let Some(d) = digits.strip_prefix("0o") {
        (8, d)
    } else if let Some(d) = digits.strip_prefix("0b") {
        (2, d)
    } else {
        (10, digits)
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let value = BigInt::from_str_radix(digits, radix).ok()?;
    Some(Value::known(if negative { -value } else { value }))
}

/// Parse `a.b[1].c` into instances `a`, `b[1]` and the signal `c`.
pub fn signal_path(text: &str, pos: Pos) -> Result<SignalPath, ScriptError> {
    let mut segments: Vec<&str> = text.split('.').collect();
    let name = segments.pop().unwrap_or_default();
    let valid = |s: &str| {
        !s.is_empty()
            && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !s.starts_with(|c: char| c.is_ascii_digit())
    };
    if !valid(name) {
        return error(pos, format!("invalid signal name `{text}`"));
    }
    let mut instances = Vec::new();
    for segment in segments {
        let (instance_name, index) = match segment.split_once('[') {
            Some((instance_name, index)) => {
                let Some(index) = index.strip_suffix(']').and_then(|i| i.parse().ok()) else {
                    return error(pos, format!("invalid instance index in `{text}`"));
                };
                (instance_name, Some(index))
            }
            None => (segment, None),
        };
        if !valid(instance_name) {
            return error(pos, format!("invalid instance name in `{text}`"));
        }
        instances.push(Instance {
            name: instance_name.into(),
            index,
        });
    }
    Ok(SignalPath {
        instances,
        name: name.into(),
    })
}

fn atom(form: &Sexpr, what: &str) -> Result<String, ScriptError> {
    match form.atom() {
        Some(atom) => Ok(atom.to_string()),
        None => error(form.pos(), format!("expected {what}")),
    }
}

fn identifier(form: &Sexpr, what: &str) -> Result<String, ScriptError> {
    let name = atom(form, what)?;
    if name.is_empty()
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || name.starts_with(|c: char| c.is_ascii_digit())
    {
        return error(form.pos(), format!("invalid {what} `{name}`"));
    }
    Ok(name)
}

fn string(form: &Sexpr, what: &str) -> Result<String, ScriptError> {
    match form {
        Sexpr::Str(text, _) => Ok(text.clone()),
        _ => error(form.pos(), format!("expected {what} string")),
    }
}

fn arity(items: &[Sexpr], pos: Pos, min: usize, max: Option<usize>) -> Result<(), ScriptError> {
    let operands = items.len() - 1;
    if operands < min || max.is_some_and(|max| operands > max) {
        let head = items[0].atom().unwrap_or("form");
        return error(pos, format!("wrong number of operands for `{head}`"));
    }
    Ok(())
}

pub fn expr(form: &Sexpr) -> Result<Expr, ScriptError> {
    match form {
        Sexpr::Atom(text, pos) => {
            if let Some(value) = literal(text) {
                Ok(Expr::Literal(value))
            } else if text.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
                error(*pos, format!("invalid number `{text}`"))
            } else {
                Ok(Expr::Var(identifier(form, "variable")?))
            }
        }
        Sexpr::Str(_, pos) => error(*pos, "unexpected string in expression"),
        Sexpr::List(items, pos) => {
            let Some(head) = items.first().and_then(Sexpr::atom) else {
                return error(*pos, "expected an operator");
            };
            if head == "get" {
                arity(items, *pos, 1, Some(1))?;
                return Ok(Expr::Get(signal_path(
                    &atom(&items[1], "signal")?,
                    items[1].pos(),
                )?));
            }
            let Some((op, min, max)) = Op::from_name(head) else {
                return error(*pos, format!("unknown operator `{head}`"));
            };
            arity(items, *pos, min, max)?;
            if op == Op::Cat && (items.len() - 1) % 2 != 0 {
                return error(*pos, "`cat` takes width/value pairs");
            }
            let operands = items[1..].iter().map(expr).collect::<Result<_, _>>()?;
            Ok(Expr::Op(op, operands))
        }
    }
}

fn sequence(form: &Sexpr, arity_of_tuple: Option<usize>) -> Result<Sequence, ScriptError> {
    let pos = form.pos();
    let Some(items) = form.list() else {
        return error(pos, "expected `(range ...)` or `(list ...)`");
    };
    match form.head() {
        Some("range") => {
            if arity_of_tuple.is_some() {
                return error(pos, "a tuple of loop variables needs `(list ...)`");
            }
            arity(items, pos, 2, Some(3))?;
            Ok(Sequence::Range(
                expr(&items[1])?,
                expr(&items[2])?,
                items.get(3).map(expr).transpose()?,
            ))
        }
        Some("list") => Ok(Sequence::List(
            items[1..]
                .iter()
                .map(|item| match arity_of_tuple {
                    None => Ok(vec![expr(item)?]),
                    Some(count) => match item.list() {
                        Some(values) if values.len() == count => values.iter().map(expr).collect(),
                        _ => error(item.pos(), format!("expected a tuple of {count} values")),
                    },
                })
                .collect::<Result<_, _>>()?,
        )),
        _ => error(pos, "expected `(range ...)` or `(list ...)`"),
    }
}

fn message(items: &[Sexpr], index: usize) -> Result<Option<String>, ScriptError> {
    items.get(index).map(|m| string(m, "message")).transpose()
}

pub fn stmt(form: &Sexpr) -> Result<Stmt, ScriptError> {
    let pos = form.pos();
    let Some(items) = form.list() else {
        return error(pos, "expected a statement");
    };
    let Some(head) = form.head() else {
        return error(pos, "expected a statement");
    };
    let signal = |index: usize| signal_path(&atom(&items[index], "signal")?, items[index].pos());
    let block = |from: usize| {
        items[from..]
            .iter()
            .map(stmt)
            .collect::<Result<Vec<_>, _>>()
    };
    let kind = match head {
        "set" => {
            arity(items, pos, 2, Some(2))?;
            StmtKind::Set(signal(1)?, expr(&items[2])?)
        }
        "eval" => {
            arity(items, pos, 0, Some(0))?;
            StmtKind::Eval
        }
        "modify" => StmtKind::Modify(block(1)?),
        "tick" => {
            arity(items, pos, 1, Some(2))?;
            let count = match items.get(2) {
                Some(count) => expr(count)?,
                None => Expr::Literal(Value::known(1)),
            };
            StmtKind::Tick(identifier(&items[1], "event")?, count)
        }
        "assert_eq" => {
            arity(items, pos, 2, Some(3))?;
            let actual = match &items[1] {
                Sexpr::Atom(..) => Actual::Signal(signal(1)?),
                other => Actual::Expr(expr(other)?),
            };
            StmtKind::AssertEq(actual, expr(&items[2])?, message(items, 3)?)
        }
        "assert" => {
            arity(items, pos, 1, Some(2))?;
            StmtKind::Assert(expr(&items[1])?, message(items, 2)?)
        }
        "let" | "set!" => {
            arity(items, pos, 2, Some(2))?;
            let name = identifier(&items[1], "variable")?;
            let value = expr(&items[2])?;
            if head == "let" {
                StmtKind::Let(name, value)
            } else {
                StmtKind::Update(name, value)
            }
        }
        "for" => {
            arity(items, pos, 2, None)?;
            let (names, tuple) = match &items[1] {
                Sexpr::List(names, _) => (
                    names
                        .iter()
                        .map(|name| identifier(name, "loop variable"))
                        .collect::<Result<Vec<_>, _>>()?,
                    Some(names.len()),
                ),
                name => (vec![identifier(name, "loop variable")?], None),
            };
            StmtKind::For(names, sequence(&items[2], tuple)?, block(3)?)
        }
        "if" => {
            arity(items, pos, 2, Some(3))?;
            StmtKind::If(
                expr(&items[1])?,
                Box::new(stmt(&items[2])?),
                items.get(3).map(stmt).transpose()?.map(Box::new),
            )
        }
        "do" => StmtKind::Block(block(1)?),
        "expand" => StmtKind::Block(expand(items, pos)?),
        "run_testbench" => {
            arity(items, pos, 0, Some(0))?;
            StmtKind::RunTestbench
        }
        "expect_output" => {
            arity(items, pos, 1, Some(1))?;
            StmtKind::ExpectOutput(string(&items[1], "output")?)
        }
        _ => return error(pos, format!("unknown statement `{head}`")),
    };
    Ok(Stmt { kind, pos })
}

/// Replace `{NAME}` in atoms and strings with the bound text.
fn substitute(form: &Sexpr, bindings: &[(String, String)]) -> Sexpr {
    let replace = |text: &str| {
        let mut text = text.to_string();
        for (name, value) in bindings {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    };
    match form {
        Sexpr::Atom(text, pos) => Sexpr::Atom(replace(text), *pos),
        Sexpr::Str(text, pos) => Sexpr::Str(replace(text), *pos),
        Sexpr::List(items, pos) => Sexpr::List(
            items
                .iter()
                .map(|item| substitute(item, bindings))
                .collect(),
            *pos,
        ),
    }
}

/// `(expand NAME (ITEM...) STMT...)` or `(expand (NAME...) ((ITEM...)...) STMT...)`:
/// the statements once per item, with `{NAME}` replaced by the item's text in
/// every atom and string. Signal names stay static, which a generated
/// testbench needs.
fn expand(items: &[Sexpr], pos: Pos) -> Result<Vec<Stmt>, ScriptError> {
    arity(items, pos, 2, None)?;
    let (names, tuple) = match &items[1] {
        Sexpr::List(names, _) => (
            names
                .iter()
                .map(|name| identifier(name, "template variable"))
                .collect::<Result<Vec<_>, _>>()?,
            true,
        ),
        name => (vec![identifier(name, "template variable")?], false),
    };
    let Some(values) = items[2].list() else {
        return error(items[2].pos(), "expected a list of items");
    };
    let text = |form: &Sexpr| match form {
        Sexpr::Atom(text, _) | Sexpr::Str(text, _) => Ok(text.clone()),
        Sexpr::List(_, pos) => error(*pos, "a template item is an atom or a string"),
    };
    let mut stmts = Vec::new();
    for value in values {
        let row: Vec<String> = if tuple {
            match value.list() {
                Some(row) if row.len() == names.len() => {
                    row.iter().map(text).collect::<Result<_, _>>()?
                }
                _ => return error(value.pos(), format!("expected {} items", names.len())),
            }
        } else {
            vec![text(value)?]
        };
        let bindings: Vec<(String, String)> = names.iter().cloned().zip(row).collect();
        for form in &items[3..] {
            stmts.push(stmt(&substitute(form, &bindings))?);
        }
    }
    Ok(stmts)
}

/// Parse a group file: `(group NAME (category C))` followed by cases.
pub fn group(text: &str) -> Result<(String, Vec<ScriptCase>), ScriptError> {
    let forms = super::sexpr::parse(text).map_err(|e| ScriptError {
        pos: e.pos,
        message: e.message,
    })?;
    let Some((header, cases)) = forms.split_first() else {
        return error(Pos { line: 1, column: 1 }, "empty script");
    };
    let pos = header.pos();
    let items = match header.list() {
        Some(items) if header.head() == Some("group") => items,
        _ => return error(pos, "a script starts with `(group NAME (category C))`"),
    };
    arity(items, pos, 2, Some(2))?;
    let group_name = identifier(&items[1], "group name")?;
    let group_category = match items[2].list() {
        Some([head, value]) if head.atom() == Some("category") => {
            category(&atom(value, "category")?, value.pos())?
        }
        _ => return error(items[2].pos(), "expected `(category C)`"),
    };
    let mut parsed: Vec<ScriptCase> = Vec::new();
    for form in cases {
        let pos = form.pos();
        let items = match form.list() {
            Some(items) if form.head() == Some("case") && items.len() >= 2 => items,
            _ => return error(pos, "expected `(case NAME ...)`"),
        };
        let name = format!("{group_name}::{}", identifier(&items[1], "case name")?);
        if parsed.iter().any(|case| case.name == name) {
            return error(pos, format!("duplicate case `{name}`"));
        }
        let mut case = ScriptCase {
            name,
            category: group_category,
            expectation: Expectation::Simulation,
            tags: Vec::new(),
            four_state: false,
            sources: Vec::new(),
            top: String::new(),
            parameters: Vec::new(),
            body: Vec::new(),
            pos,
        };
        let mut rest = &items[2..];
        while let Some((clause, tail)) = rest.split_first() {
            let clause_pos = clause.pos();
            let clause_items = clause.list().unwrap_or_default();
            match clause.head() {
                Some("source") => {
                    arity(clause_items, clause_pos, 2, None)?;
                    let parts = clause_items[2..]
                        .iter()
                        .map(|part| match part {
                            Sexpr::List(items, pos) => match items.as_slice() {
                                [head, path] if head.atom() == Some("std") => {
                                    Ok(SourcePart::Std(string(path, "standard library path")?))
                                }
                                _ => error(*pos, "expected source text or `(std PATH)`"),
                            },
                            text => Ok(SourcePart::Text(string(text, "source text")?)),
                        })
                        .collect::<Result<_, _>>()?;
                    case.sources
                        .push((string(&clause_items[1], "source path")?.into(), parts));
                }
                Some("top") => {
                    arity(clause_items, clause_pos, 1, Some(1))?;
                    case.top = identifier(&clause_items[1], "top module")?;
                }
                Some("expect") => {
                    arity(clause_items, clause_pos, 1, Some(1))?;
                    if clause_items[1].atom() != Some("reject") {
                        return error(clause_pos, "expected `(expect reject)`");
                    }
                    case.expectation = Expectation::CompilationError;
                }
                Some("four_state") => {
                    arity(clause_items, clause_pos, 0, Some(0))?;
                    case.four_state = true;
                }
                Some("parameter") => {
                    arity(clause_items, clause_pos, 2, Some(2))?;
                    let name = identifier(&clause_items[1], "parameter")?;
                    let value = atom(&clause_items[2], "parameter value")?;
                    let Some(value) = literal(&value)
                        .filter(Value::is_known)
                        .and_then(|value| u64::try_from(value.payload).ok())
                    else {
                        return error(clause_pos, "a parameter value is a known 64-bit literal");
                    };
                    case.parameters.push((name, value));
                }
                Some("tags") => {
                    for item in &clause_items[1..] {
                        case.tags.push(tag(&atom(item, "tag")?, item.pos())?);
                    }
                }
                Some("category") => {
                    arity(clause_items, clause_pos, 1, Some(1))?;
                    case.category = category(&atom(&clause_items[1], "category")?, clause_pos)?;
                }
                _ => break,
            }
            rest = tail;
        }
        if case.sources.is_empty() || case.top.is_empty() {
            return error(pos, "a case needs `(source ...)` and `(top ...)`");
        }
        case.body = rest.iter().map(stmt).collect::<Result<_, _>>()?;
        if case.expectation == Expectation::CompilationError && !case.body.is_empty() {
            return error(pos, "a rejected case has no statements");
        }
        parsed.push(case);
    }
    Ok((group_name, parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_cover_radixes_sizes_and_unknown_digits() {
        assert_eq!(literal("42"), Some(Value::known(42)));
        assert_eq!(literal("-0x10"), Some(Value::known(-16)));
        assert_eq!(literal("0b1010_0101"), Some(Value::known(0xa5)));
        assert_eq!(literal("8'hA5"), Some(Value::known(0xa5)));
        assert_eq!(
            literal("4'b1x0z"),
            Some(Value {
                payload: BigInt::from(0b1100),
                mask: BigUint::from(0b0101u8),
            })
        );
        assert_eq!(literal("4'h1F"), None);
        assert_eq!(literal("abc"), None);
    }

    #[test]
    fn signal_paths_split_instances_and_indices() {
        let pos = Pos { line: 1, column: 1 };
        let path = signal_path("u_sub[1].inner.o_data", pos).unwrap();
        assert_eq!(path.name, "o_data");
        assert_eq!(path.instances[0].index, Some(1));
        assert_eq!(path.instances[1].index, None);
        assert!(signal_path("a..b", pos).is_err());
    }

    #[test]
    fn expand_substitutes_names_and_messages() {
        let forms = super::super::sexpr::parse(
            r#"(expand (p v) ((c 1) (f 2)) (assert_eq {p}_out {v} "{p} output"))"#,
        )
        .unwrap();
        let parsed = stmt(&forms[0]).unwrap();
        let StmtKind::Block(stmts) = parsed.kind else {
            panic!("expected a block");
        };
        assert_eq!(stmts.len(), 2);
        let StmtKind::AssertEq(Actual::Signal(path), Expr::Literal(value), Some(message)) =
            &stmts[1].kind
        else {
            panic!("expected assert_eq");
        };
        assert_eq!(path.name, "f_out");
        assert_eq!(value, &Value::known(2));
        assert_eq!(message, "f output");
    }

    #[test]
    fn parameter_clauses_set_top_parameters() {
        let header = "(group g (category operators))";
        let (_, cases) = group(&format!(
            r#"{header} (case t (source "top.sv" "") (top Top) (parameter N 4) (parameter P 0xcd) (eval))"#
        ))
        .unwrap();
        assert_eq!(
            cases[0].parameters,
            [("N".to_string(), 4), ("P".to_string(), 0xcd)]
        );
        // A value with unknown bits cannot set a parameter.
        assert!(
            group(&format!(
                r#"{header} (case t (source "top.sv" "") (top Top) (parameter N 4'bx))"#
            ))
            .is_err()
        );
    }

    #[test]
    fn groups_parse_clauses_and_statements() {
        let (name, cases) = group(
            r##"(group operators (category operators))
            (case t (source "test.veryl" #"module Top {}"#) (top Top) (tags evaluation_order)
              (for v (list 1 2) (modify (set a v)) (assert_eq o (+ v 1) "o follows a")))
            (case bad (source "test.veryl" "x") (top Top) (expect reject))"##,
        )
        .unwrap();
        assert_eq!(name, "operators");
        assert_eq!(cases[0].name, "operators::t");
        assert_eq!(cases[0].tags, [TestTag::EvaluationOrder]);
        assert!(matches!(cases[0].body[0].kind, StmtKind::For(..)));
        assert_eq!(cases[1].expectation, Expectation::CompilationError);
    }
}
