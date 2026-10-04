//! Script interpreter over the backend-independent [`Simulator`].

use super::ast::{Actual, Expr, Op, ScriptCase, Sequence, Stmt, StmtKind, Value};
use super::sexpr::Pos;
use crate::{Signal, SignalPath, Simulator};
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{One, Signed, ToPrimitive, Zero};
use std::collections::BTreeMap;

/// Shift amounts, widths and exponents above this are script errors rather
/// than requests for gigantic numbers.
const LIMIT: usize = 1 << 16;

pub(crate) struct Failure {
    pub pos: Pos,
    pub message: String,
}

type Outcome<T> = Result<T, Failure>;

fn fail<T>(pos: Pos, message: impl Into<String>) -> Outcome<T> {
    Err(Failure {
        pos,
        message: message.into(),
    })
}

/// Hex rendering with `x`/`z` digits where a whole nibble is unknown, and an
/// explicit mask otherwise.
pub(crate) fn render(value: &Value) -> String {
    if value.is_known() {
        return if value.payload.is_negative() {
            format!("-0x{:x}", -&value.payload)
        } else {
            format!("0x{:x}", value.payload)
        };
    }
    let payload = value.payload.magnitude();
    let digits = (payload.bits().max(value.mask.bits()) as usize)
        .div_ceil(4)
        .max(1);
    let mut text = String::from("0x");
    let mut mixed = false;
    for digit in (0..digits).rev() {
        let nibble = |v: &BigUint| ((v >> (digit * 4)) & BigUint::from(15u8)).to_u8().unwrap();
        match (nibble(payload), nibble(&value.mask)) {
            (p, 0) => text.push(char::from_digit(u32::from(p), 16).unwrap()),
            (15, 15) => text.push('x'),
            (0, 15) => text.push('z'),
            _ => {
                mixed = true;
                text.push('?');
            }
        }
    }
    if mixed {
        format!("{{payload 0x{payload:x}, mask 0x{:x}}}", value.mask)
    } else {
        text
    }
}

struct Interpreter<'a> {
    sim: &'a mut Simulator,
    scopes: Vec<BTreeMap<String, Value>>,
    signals: BTreeMap<String, Signal>,
}

fn path_key(path: &SignalPath) -> String {
    let mut key = String::new();
    for instance in &path.instances {
        key.push_str(&instance.name);
        if let Some(index) = instance.index {
            key.push_str(&format!("[{index}]"));
        }
        key.push('.');
    }
    key.push_str(&path.name);
    key
}

fn unsigned(value: &BigInt) -> Option<BigUint> {
    value.to_biguint()
}

fn small(value: &Value, pos: Pos, what: &str) -> Outcome<usize> {
    if !value.is_known() {
        return fail(pos, format!("{what} has unknown bits"));
    }
    match value.payload.to_usize() {
        Some(n) if n <= LIMIT => Ok(n),
        _ => fail(pos, format!("{what} {} is out of range", value.payload)),
    }
}

fn mask_of(width: usize) -> BigUint {
    (BigUint::one() << width) - 1u8
}

/// The bit pattern of a value for positional operations: a known negative
/// number has infinitely many leading ones, so it needs an explicit width.
fn pattern(value: &Value, width: usize) -> (BigUint, BigUint) {
    let payload = if value.payload.is_negative() {
        let modulus = BigInt::one() << width;
        ((&value.payload % &modulus) + &modulus).magnitude().clone() & mask_of(width)
    } else {
        value.payload.magnitude() & mask_of(width)
    };
    (payload, &value.mask & mask_of(width))
}

fn from_pattern(payload: BigUint, mask: BigUint) -> Value {
    Value {
        payload: BigInt::from_biguint(Sign::Plus, payload),
        mask,
    }
}

fn truth(value: bool) -> Value {
    Value::known(u8::from(value))
}

impl Interpreter<'_> {
    fn signal(&mut self, path: &SignalPath) -> Signal {
        let key = path_key(path);
        if let Some(signal) = self.signals.get(&key) {
            return *signal;
        }
        let instances: Vec<(&str, Option<usize>)> = path
            .instances
            .iter()
            .map(|instance| (instance.name.as_str(), instance.index))
            .collect();
        let signal = self.sim.child_signal(&instances, &path.name);
        self.signals.insert(key, signal);
        signal
    }

    fn read(&mut self, path: &SignalPath) -> Value {
        let signal = self.signal(path);
        let (payload, mask) = self.sim.get_four_state(signal);
        from_pattern(payload, mask)
    }

    fn lookup(&self, name: &str, pos: Pos) -> Outcome<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(value) = scope.get(name) {
                return Ok(value.clone());
            }
        }
        fail(pos, format!("unknown variable `{name}`"))
    }

    fn known(&mut self, expr: &Expr, pos: Pos, what: &str) -> Outcome<BigInt> {
        let value = self.eval(expr, pos)?;
        if !value.is_known() {
            return fail(pos, format!("{what} has unknown bits: {}", render(&value)));
        }
        Ok(value.payload)
    }

    fn eval(&mut self, expr: &Expr, pos: Pos) -> Outcome<Value> {
        match expr {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Var(name) => self.lookup(name, pos),
            Expr::Get(path) => Ok(self.read(path)),
            Expr::Op(op, operands) => self.op(*op, operands, pos),
        }
    }

    fn op(&mut self, op: Op, operands: &[Expr], pos: Pos) -> Outcome<Value> {
        let operand = |me: &mut Self, index: usize| me.known(&operands[index], pos, "operand");
        Ok(match op {
            Op::Add | Op::Mul | Op::And | Op::Or | Op::Xor | Op::Min | Op::Max => {
                let mut result = operand(self, 0)?;
                for index in 1..operands.len() {
                    let value = operand(self, index)?;
                    result = match op {
                        Op::Add => result + value,
                        Op::Mul => result * value,
                        Op::And => result & value,
                        Op::Or => result | value,
                        Op::Xor => result ^ value,
                        Op::Min => result.min(value),
                        _ => result.max(value),
                    };
                }
                Value::known(result)
            }
            Op::Sub if operands.len() == 1 => Value::known(-operand(self, 0)?),
            Op::Sub => Value::known(operand(self, 0)? - operand(self, 1)?),
            Op::Div | Op::Rem => {
                let (left, right) = (operand(self, 0)?, operand(self, 1)?);
                if right.is_zero() {
                    return fail(pos, "division by zero");
                }
                // Both truncate toward zero, like SystemVerilog.
                Value::known(if op == Op::Div {
                    left / right
                } else {
                    left % right
                })
            }
            Op::Pow => {
                let base = operand(self, 0)?;
                let exponent = small(&self.eval(&operands[1], pos)?, pos, "exponent")?;
                Value::known(num_traits::pow(base, exponent))
            }
            Op::Not => Value::known(!operand(self, 0)?),
            Op::Shl | Op::Shr => {
                let value = operand(self, 0)?;
                let amount = small(&self.eval(&operands[1], pos)?, pos, "shift amount")?;
                // `>>` floors, like an arithmetic shift of a wide signed number.
                Value::known(if op == Op::Shl {
                    value << amount
                } else {
                    value >> amount
                })
            }
            Op::Eq | Op::Ne => {
                let (left, right) = (self.eval(&operands[0], pos)?, self.eval(&operands[1], pos)?);
                truth((left == right) == (op == Op::Eq))
            }
            Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                let (left, right) = (operand(self, 0)?, operand(self, 1)?);
                truth(match op {
                    Op::Lt => left < right,
                    Op::Le => left <= right,
                    Op::Gt => left > right,
                    _ => left >= right,
                })
            }
            Op::LogicNot => truth(operand(self, 0)?.is_zero()),
            Op::LogicAnd => {
                for index in 0..operands.len() {
                    if operand(self, index)?.is_zero() {
                        return Ok(truth(false));
                    }
                }
                truth(true)
            }
            Op::LogicOr => {
                for index in 0..operands.len() {
                    if !operand(self, index)?.is_zero() {
                        return Ok(truth(true));
                    }
                }
                truth(false)
            }
            Op::Cond => {
                if operand(self, 0)?.is_zero() {
                    self.eval(&operands[2], pos)?
                } else {
                    self.eval(&operands[1], pos)?
                }
            }
            Op::Trunc => {
                let width = small(&self.eval(&operands[0], pos)?, pos, "width")?;
                let (payload, mask) = pattern(&self.eval(&operands[1], pos)?, width);
                from_pattern(payload, mask)
            }
            Op::Sext => {
                let width = small(&self.eval(&operands[0], pos)?, pos, "width")?;
                if width == 0 {
                    return fail(pos, "`sext` needs a positive width");
                }
                let (payload, _) = pattern(&Value::known(operand(self, 1)?), width);
                let mut value = BigInt::from_biguint(Sign::Plus, payload);
                if value.bit(width as u64 - 1) {
                    value -= BigInt::one() << width;
                }
                Value::known(value)
            }
            Op::Slice | Op::Bit => {
                let value = self.eval(&operands[0], pos)?;
                let high = small(&self.eval(&operands[1], pos)?, pos, "bit index")?;
                let low = if op == Op::Slice {
                    small(&self.eval(&operands[2], pos)?, pos, "bit index")?
                } else {
                    high
                };
                if low > high {
                    return fail(pos, "`slice` needs HI >= LO");
                }
                let (payload, mask) = pattern(&value, high + 1);
                from_pattern(payload >> low, mask >> low)
            }
            Op::Cat => {
                let mut payload = BigUint::zero();
                let mut mask = BigUint::zero();
                for pair in operands.chunks(2) {
                    let width = small(&self.eval(&pair[0], pos)?, pos, "width")?;
                    let (p, m) = pattern(&self.eval(&pair[1], pos)?, width);
                    payload = (payload << width) | p;
                    mask = (mask << width) | m;
                }
                from_pattern(payload, mask)
            }
            Op::Rep => {
                let count = small(&self.eval(&operands[0], pos)?, pos, "count")?;
                let width = small(&self.eval(&operands[1], pos)?, pos, "width")?;
                if count * width > LIMIT {
                    return fail(pos, "replication is too wide");
                }
                let (p, m) = pattern(&self.eval(&operands[2], pos)?, width);
                let mut payload = BigUint::zero();
                let mut mask = BigUint::zero();
                for _ in 0..count {
                    payload = (payload << width) | &p;
                    mask = (mask << width) | &m;
                }
                from_pattern(payload, mask)
            }
            Op::FourState => {
                let (payload, mask) = (operand(self, 0)?, operand(self, 1)?);
                let (Some(payload), Some(mask)) = (unsigned(&payload), unsigned(&mask)) else {
                    return fail(pos, "`fourstate` needs non-negative operands");
                };
                // A Z bit has payload 0; force the encoding to be canonical.
                from_pattern(payload, mask)
            }
            Op::Payload => {
                let value = self.eval(&operands[0], pos)?;
                Value::known(value.payload)
            }
            Op::XMask => {
                let value = self.eval(&operands[0], pos)?;
                Value::known(BigInt::from_biguint(Sign::Plus, value.mask))
            }
            Op::Known => truth(self.eval(&operands[0], pos)?.is_known()),
            Op::Popcount | Op::Bitlen => {
                let Some(value) = unsigned(&operand(self, 0)?) else {
                    return fail(pos, "operand must be non-negative");
                };
                Value::known(if op == Op::Popcount {
                    value.count_ones()
                } else {
                    value.bits()
                })
            }
        })
    }

    fn block(&mut self, stmts: &[Stmt]) -> Outcome<()> {
        self.scopes.push(BTreeMap::new());
        let result = stmts.iter().try_for_each(|stmt| self.stmt(stmt));
        self.scopes.pop();
        result
    }

    fn write_value(&mut self, path: &SignalPath, value: Value, pos: Pos) -> Outcome<()> {
        let Some(payload) = unsigned(&value.payload) else {
            return fail(
                pos,
                format!(
                    "cannot write negative {} to `{}`; use (trunc WIDTH v)",
                    value.payload,
                    path_key(path)
                ),
            );
        };
        let signal = self.signal(path);
        self.sim.set_four_state(signal, payload, value.mask);
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Outcome<()> {
        let pos = stmt.pos;
        match &stmt.kind {
            StmtKind::Set(path, value) => {
                let value = self.eval(value, pos)?;
                self.write_value(path, value, pos)?;
            }
            StmtKind::Eval => self.settle(pos)?,
            StmtKind::Modify(stmts) => {
                self.block(stmts)?;
                self.settle(pos)?;
            }
            StmtKind::Tick(event, count) => {
                let count = small(&self.eval(count, pos)?, pos, "tick count")?;
                let event = self.sim.event(event);
                for _ in 0..count {
                    if let Err(error) = self.sim.tick(event) {
                        panic!("tick: {error}");
                    }
                }
            }
            StmtKind::AssertEq(actual, expected, message) => {
                let expected = self.eval(expected, pos)?;
                if expected.payload.is_negative() {
                    return fail(
                        pos,
                        format!(
                            "expected value {} is negative; use (trunc WIDTH v)",
                            expected.payload
                        ),
                    );
                }
                let (what, actual) = match actual {
                    Actual::Signal(path) => (path_key(path), self.read(path)),
                    Actual::Expr(expr) => ("expression".to_string(), self.eval(expr, pos)?),
                };
                if actual != expected {
                    let note = message
                        .as_deref()
                        .map(|m| format!(" ({m})"))
                        .unwrap_or_default();
                    return fail(
                        pos,
                        format!(
                            "assert_eq {what}: expected {}, got {}{note}",
                            render(&expected),
                            render(&actual)
                        ),
                    );
                }
            }
            StmtKind::Assert(condition, message) => {
                // An equality reports both sides.
                if let Expr::Op(Op::Eq, operands) = condition {
                    let left = self.eval(&operands[0], pos)?;
                    let right = self.eval(&operands[1], pos)?;
                    if left != right {
                        let message = message.as_deref().unwrap_or("assertion failed");
                        return fail(
                            pos,
                            format!("{message}: {} != {}", render(&left), render(&right)),
                        );
                    }
                } else if self.known(condition, pos, "assertion")?.is_zero() {
                    let message = message.as_deref().unwrap_or("assertion failed");
                    return fail(pos, message.to_string());
                }
            }
            StmtKind::Let(name, value) => {
                let value = self.eval(value, pos)?;
                self.scopes.last_mut().unwrap().insert(name.clone(), value);
            }
            StmtKind::Update(name, value) => {
                let value = self.eval(value, pos)?;
                let Some(slot) = self
                    .scopes
                    .iter_mut()
                    .rev()
                    .find_map(|scope| scope.get_mut(name))
                else {
                    return fail(pos, format!("unknown variable `{name}`"));
                };
                *slot = value;
            }
            StmtKind::For(names, sequence, body) => {
                let values: Vec<Vec<Value>> = match sequence {
                    Sequence::List(items) => items
                        .iter()
                        .map(|tuple| tuple.iter().map(|item| self.eval(item, pos)).collect())
                        .collect::<Outcome<_>>()?,
                    Sequence::Range(start, end, step) => {
                        let start = self.known(start, pos, "range start")?;
                        let end = self.known(end, pos, "range end")?;
                        let step = match step {
                            Some(step) => self.known(step, pos, "range step")?,
                            None => BigInt::one(),
                        };
                        if step.is_zero() {
                            return fail(pos, "range step is zero");
                        }
                        let mut values = Vec::new();
                        let mut value = start;
                        while (step.is_positive() && value < end)
                            || (step.is_negative() && value > end)
                        {
                            if values.len() > LIMIT * 16 {
                                return fail(pos, "range is too long");
                            }
                            values.push(vec![Value::known(value.clone())]);
                            value += &step;
                        }
                        values
                    }
                };
                for tuple in values {
                    self.scopes
                        .push(names.iter().cloned().zip(tuple).collect::<BTreeMap<_, _>>());
                    let result = self.block(body);
                    self.scopes.pop();
                    result?;
                }
            }
            StmtKind::If(condition, then, otherwise) => {
                if !self.known(condition, pos, "condition")?.is_zero() {
                    self.block(std::slice::from_ref(then))?;
                } else if let Some(otherwise) = otherwise {
                    self.block(std::slice::from_ref(otherwise))?;
                }
            }
            StmtKind::Block(stmts) => self.block(stmts)?,
            StmtKind::RunTestbench => {
                if let Err(error) = self.sim.run_testbench() {
                    panic!("run_testbench: {error}");
                }
            }
        }
        Ok(())
    }

    fn settle(&mut self, _pos: Pos) -> Outcome<()> {
        if let Err(error) = self.sim.eval_comb() {
            panic!("eval: {error}");
        }
        Ok(())
    }
}

/// Run a case's statements. Panics on a failed assertion, a script error or
/// an adapter error, like the Rust cases.
pub(crate) fn run(case: &ScriptCase, sim: &mut Simulator) {
    let mut interpreter = Interpreter {
        sim,
        scopes: vec![BTreeMap::new()],
        signals: BTreeMap::new(),
    };
    if let Err(failure) = interpreter.block(&case.body) {
        panic!("{} at {}: {}", case.name, failure.pos, failure.message);
    }
}
