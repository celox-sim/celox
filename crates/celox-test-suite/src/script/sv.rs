//! Translate a script case into a self-checking SystemVerilog testbench.
//!
//! The testbench instantiates the case's top module as `dut`, drives its
//! input ports from testbench variables and reads every signal through a
//! hierarchical reference. Script values become `logic signed` variables wide
//! enough that no computation in the case overflows, so the unbounded script
//! arithmetic carries over unchanged. A failed assertion prints a line that
//! starts with `@suite assert` and ends the run with `$fatal`; success reaches
//! `$finish` (after `$celox_suite_finish` under Icarus).
//!
//! The timing follows the process adapters: a write takes effect at the next
//! settle, which advances time by one step; a tick drives the event port to
//! its inactive and then its active level, settling after each, and restores
//! a level that was neither.

use super::ast::{Actual, Expr, Op, ScriptCase, Sequence, Stmt, StmtKind, Value};
use super::sexpr::Pos;
use crate::SignalPath;
use num_traits::{Signed, ToPrimitive};
use std::collections::BTreeMap;
use std::fmt::Write;

/// A top-level input port of the design under test.
#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    /// The width of the port, or of one element of an array port.
    pub width: usize,
    /// The declared bounds of a one-dimensional unpacked array port.
    pub array: Option<Bounds>,
}

/// The declared `[left:right]` bounds of an unpacked array.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub left: i64,
    pub right: i64,
}

impl Bounds {
    /// `[0:count-1]`, as Veryl declares an array of `count` elements.
    pub fn ascending(count: usize) -> Self {
        Self {
            left: 0,
            right: count as i64 - 1,
        }
    }

    /// The indices from left to right. A script value holds the leftmost
    /// element in its least significant bits.
    pub fn indices(self) -> Vec<i64> {
        if self.left <= self.right {
            (self.left..=self.right).collect()
        } else {
            (self.right..=self.left).rev().collect()
        }
    }

    pub fn count(self) -> usize {
        self.left.abs_diff(self.right) as usize + 1
    }
}

/// What the generator needs to know about the compiled design.
#[derive(Clone, Debug)]
pub struct DesignInfo {
    pub top: String,
    pub inputs: Vec<Port>,
    /// Output ports, left unconnected.
    pub outputs: Vec<String>,
    /// Event ports and whether they are active on a rising edge.
    pub edges: BTreeMap<String, bool>,
    /// The widest signal anywhere in the design.
    pub max_width: usize,
    /// Values of the top module's parameters, set on the instance.
    pub parameters: Vec<(String, u64)>,
    /// Top-level unpacked arrays: element width and bounds. A script reads
    /// one as a packed value with element 0 in the least significant bits.
    pub arrays: BTreeMap<String, (usize, Bounds)>,
}

/// The name of the generated testbench module.
pub const TESTBENCH_TOP: &str = "celox_suite_tb";

/// Starts the line a generated testbench prints for `expect_output`, after a
/// newline of its own: the hex-encoded expected text, then its location.
const OUTPUT_MARKER: &str = "@suite output ";

fn hex_encode(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

/// Check the design output in a generated testbench's stdout: the text
/// before each `expect_output` marker, since the previous one, must equal the
/// text the marker encodes. Text after the last marker, such as a
/// simulator's `$finish` report, is not design output the script checks.
pub fn check_output(log: &str) -> Result<(), String> {
    let separator = format!("\n{OUTPUT_MARKER}");
    let mut rest = log;
    while let Some(at) = rest.find(&separator) {
        let output = &rest[..at];
        let marker = &rest[at + separator.len()..];
        let end = marker.find('\n').ok_or("unterminated output marker")?;
        let (hex, location) = marker[..end]
            .split_once(' ')
            .ok_or("output marker without a location")?;
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|d| u8::from_str_radix(d, 16).ok())
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("malformed output marker")?;
        let expected = String::from_utf8_lossy(&bytes);
        if output != expected {
            return Err(format!(
                "{location}: expect_output: expected {expected:?}, got {output:?}"
            ));
        }
        rest = &marker[end + 1..];
    }
    Ok(())
}

/// Why a case has no generated testbench.
#[derive(Debug)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

/// Whether the case only runs the design's own native testbench, which needs
/// no generated one.
pub fn is_native_testbench(case: &ScriptCase) -> bool {
    matches!(case.body.as_slice(), [stmt] if stmt.kind == StmtKind::RunTestbench)
}

struct Generator<'a> {
    case: &'a ScriptCase,
    design: &'a DesignInfo,
    width: usize,
    out: String,
    indent: usize,
    temps: usize,
}

fn path_text(path: &SignalPath) -> String {
    let mut text = String::from("dut");
    for instance in &path.instances {
        text.push('.');
        text.push_str(&instance.name);
        if let Some(index) = instance.index {
            let _ = write!(text, "[{index}]");
        }
    }
    let _ = write!(text, ".{}", path.name);
    text
}

fn path_label(path: &SignalPath) -> String {
    path_text(path)["dut.".len()..].to_string()
}

/// Escape text for use inside a `$display` format string.
fn literal_text(text: &str) -> String {
    text.replace('%', "%%")
}

fn sv_string(text: &str) -> String {
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn walk_expr(expr: &Expr, visit: &mut impl FnMut(&Expr)) {
    visit(expr);
    if let Expr::Op(_, operands) = expr {
        for operand in operands {
            walk_expr(operand, visit);
        }
    }
}

fn walk_stmt(stmt: &Stmt, visit: &mut impl FnMut(&Expr)) {
    match &stmt.kind {
        StmtKind::Set(_, e) | StmtKind::Let(_, e) | StmtKind::Update(_, e) => walk_expr(e, visit),
        StmtKind::Tick(_, e) | StmtKind::Assert(e, _) => walk_expr(e, visit),
        StmtKind::AssertEq(actual, e, _) => {
            if let Actual::Expr(actual) = actual {
                walk_expr(actual, visit);
            }
            walk_expr(e, visit);
        }
        StmtKind::Modify(body) | StmtKind::Block(body) => {
            body.iter().for_each(|s| walk_stmt(s, visit))
        }
        StmtKind::For(_, sequence, body) => {
            match sequence {
                Sequence::Range(a, b, c) => {
                    walk_expr(a, visit);
                    walk_expr(b, visit);
                    if let Some(c) = c {
                        walk_expr(c, visit);
                    }
                }
                Sequence::List(items) => items.iter().flatten().for_each(|e| walk_expr(e, visit)),
            }
            body.iter().for_each(|s| walk_stmt(s, visit));
        }
        StmtKind::If(c, then, otherwise) => {
            walk_expr(c, visit);
            walk_stmt(then, visit);
            if let Some(otherwise) = otherwise {
                walk_stmt(otherwise, visit);
            }
        }
        StmtKind::Eval | StmtKind::RunTestbench | StmtKind::ExpectOutput(_) => {}
    }
}

fn reads(exprs: &[&Expr]) -> bool {
    let mut found = false;
    for expr in exprs {
        walk_expr(expr, &mut |e| found |= matches!(e, Expr::Get(_)));
    }
    found
}

/// The widest testbench value: Verilator multiplies signed values of at
/// most 4096 bits (`VL_MULS_MAX_WORDS`).
const MAX_VALUE_WIDTH: usize = 4096;

/// The width reserved for values built by shifts of run-time amounts, so
/// that twice it (plus a margin) stays within `MAX_VALUE_WIDTH`.
const DYNAMIC_SHIFT_WIDTH: usize = (MAX_VALUE_WIDTH - 64) / 2;

/// A width that holds every value the case computes: twice the widest
/// signal or literal (so a product of two fits), plus constant shifts.
fn value_width(case: &ScriptCase, design: &DesignInfo) -> Result<usize, Unsupported> {
    let literal = |e: &Expr| match e {
        Expr::Literal(value) => value.payload.to_usize().map(|v| v.min(1 << 14)),
        _ => None,
    };
    let mut widest = design.max_width.max(64);
    let mut multiplies = false;
    for stmt in &case.body {
        walk_stmt(stmt, &mut |e| match e {
            Expr::Op(Op::Mul | Op::Pow, _) => multiplies = true,
            Expr::Literal(value) => {
                widest = widest.max(value.payload.bits() as usize + 1);
                widest = widest.max(value.mask.bits() as usize + 1);
            }
            // Constant shifts, concatenations and replications widen the
            // widest value by their own amount.
            Expr::Op(Op::Shl, operands) => match literal(&operands[1]) {
                Some(amount) => widest = widest.max(design.max_width.max(64) + amount),
                // A shift by a run-time amount can build a value much wider
                // than any signal (a case may pack a sequence into one value).
                None => widest = widest.max(DYNAMIC_SHIFT_WIDTH),
            },
            // An unpacked array read whole is one wide value.
            Expr::Get(path) if path.instances.is_empty() => {
                if let Some((width, bounds)) = design.arrays.get(&path.name) {
                    widest = widest.max(width * bounds.count());
                }
            }
            Expr::Op(Op::Cat, operands) => {
                let total: usize = operands
                    .chunks(2)
                    .filter_map(|pair| literal(&pair[0]))
                    .sum();
                widest = widest.max(total);
            }
            Expr::Op(Op::Rep, operands) => {
                if let (Some(count), Some(width)) = (literal(&operands[0]), literal(&operands[1])) {
                    widest = widest.max(count * width);
                }
            }
            _ => {}
        });
    }
    // Twice the widest value when the case multiplies, so that a product
    // of two still fits.
    let factor = if multiplies { 2 } else { 1 };
    let width = (factor * widest + 64).div_ceil(64) * 64;
    if width > MAX_VALUE_WIDTH {
        return Err(Unsupported(format!(
            "{} needs {width}-bit testbench values; the limit is {MAX_VALUE_WIDTH}",
            case.name
        )));
    }
    Ok(width)
}

impl Generator<'_> {
    fn line(&mut self, text: impl AsRef<str>) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(text.as_ref());
        self.out.push('\n');
    }

    fn temp(&mut self) -> String {
        self.temps += 1;
        format!("t{}", self.temps)
    }

    fn where_(&self, pos: Pos) -> String {
        format!("{} at {pos}", self.case.name)
    }

    fn literal(&self, value: &Value) -> String {
        let n = self.width;
        if value.is_known() {
            let magnitude = value.payload.magnitude();
            if value.payload.is_negative() {
                format!("(-{n}'sh{magnitude:x})")
            } else {
                format!("{n}'sh{magnitude:x}")
            }
        } else {
            let bits = value.payload.bits().max(value.mask.bits()).max(1);
            // A leading 0: a leftmost x or z digit would extend to the width.
            let mut text = format!("{n}'sb0");
            for bit in (0..bits).rev() {
                text.push(match (value.payload.bit(bit), value.mask.bit(bit)) {
                    (false, false) => '0',
                    (true, false) => '1',
                    (true, true) => 'x',
                    (false, true) => 'z',
                });
            }
            text
        }
    }

    fn boolean(&self, condition: String) -> String {
        format!("$signed({}'({condition}))", self.width)
    }

    fn mask(&self, width: &str) -> String {
        format!("(({}'sd1 <<< ({width})) - {}'sd1)", self.width, self.width)
    }

    fn expr(&mut self, expr: &Expr) -> String {
        let n = self.width;
        match expr {
            Expr::Literal(value) => self.literal(value),
            Expr::Var(name) => format!("v_{name}"),
            Expr::Get(path) => match self.design.arrays.get(&path.name) {
                // Element 0 in the least significant bits.
                Some((_, bounds)) if path.instances.is_empty() => {
                    let base = path_text(path);
                    let elements: Vec<String> = bounds
                        .indices()
                        .into_iter()
                        .rev()
                        .map(|i| format!("{base}[{i}]"))
                        .collect();
                    format!("$signed({n}'({{{}}}))", elements.join(", "))
                }
                _ => format!("$signed({n}'({{{}}}))", path_text(path)),
            },
            Expr::Op(op, operands) => {
                let args: Vec<String> = operands.iter().map(|e| self.expr(e)).collect();
                let fold = |symbol: &str| format!("({})", args.join(&format!(" {symbol} ")));
                match op {
                    Op::Add => fold("+"),
                    Op::Sub if args.len() == 1 => format!("(-{})", args[0]),
                    Op::Sub => fold("-"),
                    Op::Mul => fold("*"),
                    Op::Div => fold("/"),
                    Op::Rem => fold("%"),
                    Op::Pow => fold("**"),
                    Op::And => fold("&"),
                    Op::Or => fold("|"),
                    Op::Xor => fold("^"),
                    Op::Not => format!("(~{})", args[0]),
                    Op::Shl => format!("({} <<< {})", args[0], args[1]),
                    Op::Shr => format!("({} >>> {})", args[0], args[1]),
                    Op::Eq => self.boolean(format!("{} === {}", args[0], args[1])),
                    Op::Ne => self.boolean(format!("{} !== {}", args[0], args[1])),
                    Op::Lt => self.boolean(format!("{} < {}", args[0], args[1])),
                    Op::Le => self.boolean(format!("{} <= {}", args[0], args[1])),
                    Op::Gt => self.boolean(format!("{} > {}", args[0], args[1])),
                    Op::Ge => self.boolean(format!("{} >= {}", args[0], args[1])),
                    Op::LogicNot => self.boolean(format!("{} == 0", args[0])),
                    Op::LogicAnd | Op::LogicOr => {
                        let symbol = if *op == Op::LogicAnd { "&&" } else { "||" };
                        let terms: Vec<String> =
                            args.iter().map(|a| format!("({a} != 0)")).collect();
                        self.boolean(terms.join(&format!(" {symbol} ")))
                    }
                    Op::Cond => format!("(({} != 0) ? {} : {})", args[0], args[1], args[2]),
                    Op::Min | Op::Max => {
                        let symbol = if *op == Op::Min { "<" } else { ">" };
                        args[1..].iter().fold(args[0].clone(), |acc, a| {
                            format!("(({acc} {symbol} {a}) ? {acc} : {a})")
                        })
                    }
                    Op::Trunc => format!("({} & {})", args[1], self.mask(&args[0])),
                    Op::Sext => {
                        let sign = format!("({n}'sd1 <<< (({}) - 1))", args[0]);
                        format!(
                            "((({} & {}) ^ {sign}) - {sign})",
                            args[1],
                            self.mask(&args[0])
                        )
                    }
                    Op::Slice => format!(
                        "(({} >>> {}) & {})",
                        args[0],
                        args[2],
                        self.mask(&format!("({}) - ({}) + 1", args[1], args[2]))
                    ),
                    Op::Bit => format!("(({} >>> {}) & {n}'sd1)", args[0], args[1]),
                    Op::Cat => {
                        let mut result = format!("{n}'sd0");
                        for pair in args.chunks(2) {
                            result = format!(
                                "(({result} <<< ({})) | ({} & {}))",
                                pair[0],
                                pair[1],
                                self.mask(&pair[0])
                            );
                        }
                        result
                    }
                    Op::Rep => format!("rep({}, {}, {})", args[0], args[1], args[2]),
                    Op::FourState => format!("fourstate({}, {})", args[0], args[1]),
                    Op::Payload => format!("payload({})", args[0]),
                    Op::XMask => format!("xmask({})", args[0]),
                    Op::Known => self.boolean(format!("!$isunknown({})", args[0])),
                    Op::Popcount => format!("$signed({n}'($countones({})))", args[0]),
                    Op::Bitlen => format!("bitlen({})", args[0]),
                }
            }
        }
    }

    fn fail(&mut self, pos: Pos, format: &str, args: &[String]) {
        let message = format!(
            "@suite assert {}: {format}",
            literal_text(&self.where_(pos))
        );
        let mut call = sv_string(&message);
        for arg in args {
            call.push_str(", ");
            call.push_str(arg);
        }
        self.line(format!("$display({call});"));
        self.line(format!(
            "$fatal(1, {});",
            sv_string(&literal_text(&self.where_(pos)))
        ));
    }

    fn require_known(&mut self, value: &str, pos: Pos, what: &str) {
        self.line(format!("if ($isunknown({value})) begin"));
        self.indent += 1;
        self.fail(pos, &literal_text(&format!("{what} has unknown bits")), &[]);
        self.indent -= 1;
        self.line("end");
    }

    fn settle_for(&mut self, exprs: &[&Expr]) {
        if reads(exprs) {
            self.line("settle();");
        }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Result<(), Unsupported> {
        self.line("begin");
        self.indent += 1;
        let mut opened = 0;
        for stmt in stmts {
            if let StmtKind::Let(name, value) = &stmt.kind {
                // The value is computed before the new variable is in
                // scope: `(let v (+ v 1))` reads the outer `v`.
                self.settle_for(&[value]);
                let value = self.expr(value);
                let temp = self.temp();
                let w = self.width - 1;
                self.line("begin");
                self.indent += 1;
                self.line(format!("logic signed [{w}:0] {temp};"));
                self.line(format!("{temp} = {value};"));
                self.line("begin");
                self.indent += 1;
                opened += 2;
                self.line(format!("logic signed [{w}:0] v_{name};"));
                self.line(format!("v_{name} = {temp};"));
            } else {
                self.stmt(stmt)?;
            }
        }
        for _ in 0..opened {
            self.indent -= 1;
            self.line("end");
        }
        self.indent -= 1;
        self.line("end");
        Ok(())
    }

    fn write(&mut self, path: &SignalPath, value: &Expr, pos: Pos) -> Result<(), Unsupported> {
        self.settle_for(&[value]);
        let value = self.expr(value);
        let input = self
            .design
            .inputs
            .iter()
            .find(|port| path.instances.is_empty() && port.name == path.name)
            .cloned();
        let array = path.instances.is_empty() && self.design.arrays.contains_key(&path.name);
        if array && input.as_ref().is_none_or(|port| port.array.is_none()) {
            return Err(Unsupported(format!(
                "{}: writes to the unpacked array {}",
                self.where_(pos),
                path.name
            )));
        }
        let target = if input.is_some() {
            format!("i_{}", path.name)
        } else {
            path_text(path)
        };
        let temp = self.temp();
        self.line("begin");
        self.indent += 1;
        self.line(format!("logic signed [{}:0] {temp};", self.width - 1));
        self.line(format!("{temp} = {value};"));
        self.line(format!("if ({temp} < 0) begin"));
        self.indent += 1;
        self.fail(
            pos,
            &literal_text(&format!(
                "cannot write a negative value to {}",
                path_label(path)
            )),
            &[],
        );
        self.indent -= 1;
        self.line("end");
        match input.and_then(|port| port.array.map(|bounds| (port.width, bounds))) {
            Some((width, bounds)) => {
                for (position, index) in bounds.indices().into_iter().enumerate() {
                    self.line(format!(
                        "{target}[{index}] = {temp}[{}:{}];",
                        (position + 1) * width - 1,
                        position * width
                    ));
                }
            }
            None => self.line(format!("{target} = {temp};")),
        }
        self.line("settled = 0;");
        self.indent -= 1;
        self.line("end");
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Result<(), Unsupported> {
        let pos = stmt.pos;
        match &stmt.kind {
            StmtKind::Set(path, value) => self.write(path, value, pos)?,
            StmtKind::Eval => self.line("settle_now();"),
            StmtKind::Modify(body) => {
                self.block(body)?;
                self.line("settle_now();");
            }
            StmtKind::Tick(event, count) => {
                let Some(rising) = self.design.edges.get(event) else {
                    return Err(Unsupported(format!(
                        "{}: no event port `{event}`",
                        self.where_(pos)
                    )));
                };
                let rising = u8::from(*rising);
                self.settle_for(&[count]);
                let count = self.expr(count);
                self.require_known(&count, pos, "tick count");
                self.line(format!("repeat ({count}) begin"));
                self.indent += 1;
                self.line("settle();");
                self.line(format!("original = i_{event};"));
                self.line(format!("i_{event} = 1'b{};", 1 - rising));
                self.line("#1;");
                self.line(format!("i_{event} = 1'b{rising};"));
                self.line("#1;");
                self.line(format!("if (original !== 1'b{rising}) begin"));
                self.indent += 1;
                self.line(format!("i_{event} = original;"));
                self.line("#1;");
                self.indent -= 1;
                self.line("end");
                self.indent -= 1;
                self.line("end");
            }
            StmtKind::AssertEq(actual, expected, message) => {
                let actual_expr;
                let (label, actual) = match actual {
                    Actual::Signal(path) => {
                        actual_expr = Expr::Get(path.clone());
                        (path_label(path), &actual_expr)
                    }
                    Actual::Expr(expr) => ("expression".to_string(), expr),
                };
                self.settle_for(&[actual, expected]);
                let (actual, expected) = (self.expr(actual), self.expr(expected));
                let note = message
                    .as_deref()
                    .map(|m| format!(" ({})", literal_text(m)))
                    .unwrap_or_default();
                let label = literal_text(&label);
                self.line(format!("expected = {expected};"));
                self.line(format!("actual = {actual};"));
                self.line("if (expected < 0) begin");
                self.indent += 1;
                self.fail(pos, "expected value is negative; use (trunc WIDTH v)", &[]);
                self.indent -= 1;
                self.line("end");
                self.line("if (actual !== expected) begin");
                self.indent += 1;
                self.fail(
                    pos,
                    &format!("assert_eq {label}: expected 0x%0h, got 0x%0h{note}"),
                    &["expected".into(), "actual".into()],
                );
                self.indent -= 1;
                self.line("end");
            }
            StmtKind::Assert(Expr::Op(Op::Eq, operands), message) => {
                self.settle_for(&[&operands[0], &operands[1]]);
                let (left, right) = (self.expr(&operands[0]), self.expr(&operands[1]));
                self.line(format!("expected = {right};"));
                self.line(format!("actual = {left};"));
                self.line("if (actual !== expected) begin");
                self.indent += 1;
                let message = literal_text(message.as_deref().unwrap_or("assertion failed"));
                self.fail(
                    pos,
                    &format!("{message}: 0x%0h != 0x%0h"),
                    &["actual".into(), "expected".into()],
                );
                self.indent -= 1;
                self.line("end");
            }
            StmtKind::Assert(condition, message) => {
                self.settle_for(&[condition]);
                let condition = self.expr(condition);
                self.require_known(&condition, pos, "assertion");
                self.line(format!("if ({condition} == 0) begin"));
                self.indent += 1;
                self.fail(
                    pos,
                    &literal_text(message.as_deref().unwrap_or("assertion failed")),
                    &[],
                );
                self.indent -= 1;
                self.line("end");
            }
            StmtKind::Let(..) => unreachable!("handled by block"),
            StmtKind::Update(name, value) => {
                self.settle_for(&[value]);
                let value = self.expr(value);
                self.line(format!("v_{name} = {value};"));
            }
            StmtKind::For(names, Sequence::Range(start, end, step), body) => {
                let name = &names[0];
                let w = self.width - 1;
                let step = step.clone().unwrap_or(Expr::Literal(Value::known(1)));
                self.settle_for(&[start, end, &step]);
                let (start, end, step) = (self.expr(start), self.expr(end), self.expr(&step));
                let (start_var, end_var, step_var) = (self.temp(), self.temp(), self.temp());
                // The bounds are computed before the loop variable is in scope.
                self.line("begin");
                self.indent += 1;
                self.line(format!(
                    "logic signed [{w}:0] {start_var}, {end_var}, {step_var};"
                ));
                self.line(format!("{start_var} = {start};"));
                self.line(format!("{end_var} = {end};"));
                self.line(format!("{step_var} = {step};"));
                self.require_known(
                    &format!("{{{start_var}, {end_var}, {step_var}}}"),
                    pos,
                    "range",
                );
                self.line(format!("if ({step_var} == 0) begin"));
                self.indent += 1;
                self.fail(pos, "range step is zero", &[]);
                self.indent -= 1;
                self.line("end");
                self.line("begin");
                self.indent += 1;
                self.line(format!("logic signed [{w}:0] v_{name};"));
                self.line(format!(
                    "for (v_{name} = {start_var}; ({step_var} > 0) ? (v_{name} < {end_var}) : (v_{name} > {end_var}); v_{name} = v_{name} + {step_var})"
                ));
                self.block(body)?;
                self.indent -= 1;
                self.line("end");
                self.indent -= 1;
                self.line("end");
            }
            StmtKind::For(names, Sequence::List(items), body) => {
                // Every item is evaluated before the first iteration.
                let w = self.width - 1;
                self.line("begin");
                self.indent += 1;
                let mut temps = Vec::new();
                for tuple in items {
                    let mut row = Vec::new();
                    for _ in tuple {
                        let temp = self.temp();
                        self.line(format!("logic signed [{w}:0] {temp};"));
                        row.push(temp);
                    }
                    temps.push(row);
                }
                for (tuple, row) in items.iter().zip(&temps) {
                    let refs: Vec<&Expr> = tuple.iter().collect();
                    self.settle_for(&refs);
                    for (item, temp) in tuple.iter().zip(row) {
                        let value = self.expr(item);
                        self.line(format!("{temp} = {value};"));
                    }
                }
                for row in &temps {
                    self.line("begin");
                    self.indent += 1;
                    for (name, temp) in names.iter().zip(row) {
                        self.line(format!("logic signed [{w}:0] v_{name};"));
                        let _ = temp;
                    }
                    for (name, temp) in names.iter().zip(row) {
                        self.line(format!("v_{name} = {temp};"));
                    }
                    self.block(body)?;
                    self.indent -= 1;
                    self.line("end");
                }
                self.indent -= 1;
                self.line("end");
            }
            StmtKind::If(condition, then, otherwise) => {
                self.settle_for(&[condition]);
                let condition = self.expr(condition);
                self.require_known(&condition, pos, "condition");
                self.line(format!("if ({condition} != 0)"));
                self.block(std::slice::from_ref(then))?;
                if let Some(otherwise) = otherwise {
                    self.line("else");
                    self.block(std::slice::from_ref(otherwise))?;
                }
            }
            StmtKind::Block(body) => self.block(body)?,
            StmtKind::RunTestbench => {
                return Err(Unsupported(format!(
                    "{}: run_testbench inside a scripted case",
                    self.where_(pos)
                )));
            }
            StmtKind::ExpectOutput(text) => {
                // HDL cannot read back its own output, so the harness checks
                // it: the marker closes the output so far, which must equal
                // the hex-encoded text (see `check_output`).
                self.line(format!(
                    "$write(\"\\n{OUTPUT_MARKER}{} {}\\n\");",
                    hex_encode(text),
                    sv_string(&literal_text(&self.where_(pos))).trim_matches('"')
                ));
            }
        }
        Ok(())
    }
}

/// Generate the testbench for `case`. Every event the script ticks must be an
/// input port listed in `design.edges`.
pub fn testbench(case: &ScriptCase, design: &DesignInfo) -> Result<String, Unsupported> {
    if is_native_testbench(case) {
        return Err(Unsupported(format!(
            "{} runs the design's own testbench",
            case.name
        )));
    }
    let width = value_width(case, design)?;
    let mut generator = Generator {
        case,
        design,
        width,
        out: String::new(),
        indent: 2,
        temps: 0,
    };
    generator.block(&case.body)?;
    let body = generator.out;
    let w = width - 1;
    let mut out = String::new();
    let _ = writeln!(out, "// Generated from {} by celox-test-suite.", case.name);
    // Script values are deliberately wider than the signals they meet.
    let _ = writeln!(out, "/* verilator lint_off WIDTH */");
    let _ = writeln!(out, "module {TESTBENCH_TOP};");
    for port in &design.inputs {
        let range = port
            .array
            .map(|bounds| format!(" [{}:{}]", bounds.left, bounds.right))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "  logic [{}:0] i_{}{range};",
            port.width.max(1) - 1,
            port.name
        );
    }
    let connections: Vec<String> = design
        .inputs
        .iter()
        .map(|port| format!(".{0}(i_{0})", port.name))
        .chain(design.outputs.iter().map(|name| format!(".{name}()")))
        .collect();
    let parameters = if design.parameters.is_empty() {
        String::new()
    } else {
        let values: Vec<String> = design
            .parameters
            .iter()
            .map(|(name, value)| format!(".{name}({value})"))
            .collect();
        format!(" #({})", values.join(", "))
    };
    let _ = writeln!(
        out,
        "  {}{parameters} dut ({});",
        design.top,
        connections.join(", ")
    );
    // Two-state runs zero every variable at start-up, so "settled" must
    // mean 1: zero and X both ask for a settle before the first read.
    let _ = writeln!(out, "  logic settled;");
    let _ = writeln!(out, "  logic original;");
    let _ = writeln!(out, "  logic signed [{w}:0] expected, actual;");
    let _ = writeln!(
        out,
        r#"
  task automatic settle_now;
    #1;
    settled = 1;
  endtask

  task automatic settle;
    if (settled !== 1'b1) settle_now();
  endtask

  function automatic logic signed [{w}:0] fourstate(input logic signed [{w}:0] p, input logic signed [{w}:0] m);
    for (int i = 0; i < {width}; i++) fourstate[i] = m[i] ? (p[i] ? 1'bx : 1'bz) : p[i];
  endfunction

  function automatic logic signed [{w}:0] payload(input logic signed [{w}:0] v);
    for (int i = 0; i < {width}; i++) payload[i] = (v[i] === 1'bz) ? 1'b0 : ((v[i] === 1'bx) ? 1'b1 : v[i]);
  endfunction

  function automatic logic signed [{w}:0] xmask(input logic signed [{w}:0] v);
    for (int i = 0; i < {width}; i++) xmask[i] = (v[i] === 1'bx) || (v[i] === 1'bz);
  endfunction

  function automatic logic signed [{w}:0] bitlen(input logic signed [{w}:0] v);
    bitlen = 0;
    for (int i = 0; i < {width}; i++) if (v[i]) bitlen = i + 1;
  endfunction

  function automatic logic signed [{w}:0] rep(input logic signed [{w}:0] n, input logic signed [{w}:0] w, input logic signed [{w}:0] v);
    rep = 0;
    for (int i = 0; i < n; i++) rep = (rep <<< w) | (v & (({width}'sd1 <<< w) - 1));
  endfunction

  initial begin"#
    );
    out.push_str(&body);
    out.push_str("`ifdef CELOX_SUITE_ICARUS\n    $celox_suite_finish;\n`endif\n    $finish;\n  end\nendmodule\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(text: &str) -> ScriptCase {
        let source = format!(
            "(group g (category operators)) (case t (source \"test.veryl\" \"\") (top Top) {text})"
        );
        super::super::ast::group(&source).unwrap().1.remove(0)
    }

    fn design() -> DesignInfo {
        DesignInfo {
            top: "Top".into(),
            inputs: vec![
                Port {
                    name: "clk".into(),
                    width: 1,
                    array: None,
                },
                Port {
                    name: "a".into(),
                    width: 8,
                    array: None,
                },
            ],
            outputs: vec!["o".into()],
            edges: BTreeMap::from([("clk".into(), true)]),
            max_width: 8,
            parameters: Vec::new(),
            arrays: BTreeMap::new(),
        }
    }

    #[test]
    fn drives_inputs_ticks_events_and_checks_values() {
        let text = testbench(
            &case("(modify (set a 5)) (tick clk 2) (assert_eq o (+ 5 1))"),
            &design(),
        )
        .unwrap();
        assert!(text.contains("Top dut (.clk(i_clk), .a(i_a), .o());"));
        assert!(text.contains("i_a = t1;"));
        assert!(text.contains("repeat (128'sh2) begin"));
        assert!(text.contains("actual = $signed(128'({dut.o}));"));
        assert!(text.contains("@suite assert g::t at"));
        assert!(text.contains("$finish;"));
    }

    #[test]
    fn sets_the_top_parameters_on_the_instance() {
        let mut info = design();
        info.parameters = vec![("N".into(), 4), ("P".into(), 205)];
        let text = testbench(&case("(eval)"), &info).unwrap();
        assert!(text.contains("Top #(.N(4), .P(205)) dut ("));
    }

    #[test]
    fn checks_the_output_before_each_marker() {
        let text = testbench(&case(r#"(tick clk) (expect_output "a\nb")"#), &design()).unwrap();
        assert!(text.contains(r#"$write("\n@suite output 610a62 g::t at"#));
        let log = "a\nb\n@suite output 610a62 here\n\n@suite output  there\nfinish\n";
        assert_eq!(check_output(log), Ok(()));
        assert_eq!(
            check_output("a\n@suite output 610a62 here\n"),
            Err(r#"here: expect_output: expected "a\nb", got "a""#.into())
        );
    }

    #[test]
    fn rejects_unknown_events() {
        assert!(testbench(&case("(tick rst)"), &design()).is_err());
    }
}
