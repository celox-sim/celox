//! Native testbench execution for Veryl `#[test]` modules.

use crate::backend::traits::SimBackend;
use crate::ir::{AbsoluteAddr, SignalRef};
use crate::simulator::Simulator;
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    Expression, Factor, ForRange, Op, Statement, SystemFunctionKind, TbMethod, TbMethodCall, VarId,
};
use veryl_parser::resource_table::StrId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestResult {
    Pass,
    Fail(String),
}

pub enum TestbenchStatement<B: SimBackend> {
    ClockNext { clock_event: B::Event, count: u64 },
    ResetAssert { _reset_event: B::Event, reset_signal: SignalRef, clock_event: B::Event, duration: u64 },
    Assert { evaluator: ExprEvaluator<B>, message: Option<String> },
    If { evaluator: ExprEvaluator<B>, then_block: Vec<TestbenchStatement<B>>, else_block: Vec<TestbenchStatement<B>> },
    For { loop_var: Option<(SignalRef, usize)>, start: usize, end: usize, step: usize, reverse: bool, body: Vec<TestbenchStatement<B>> },
    Assign { dst: SignalRef, evaluator: ExprEvaluator<B> },
    Finish,
}

pub struct ExprEvaluator<B: SimBackend> { kind: ExprEvalKind<B> }
enum ExprEvalKind<B: SimBackend> {
    Constant(BigUint), Signal(SignalRef),
    Unary(Op, Box<ExprEvaluator<B>>), Binary(Box<ExprEvaluator<B>>, Op, Box<ExprEvaluator<B>>),
    Ternary(Box<ExprEvaluator<B>>, Box<ExprEvaluator<B>>, Box<ExprEvaluator<B>>),
    _Phantom(std::marker::PhantomData<B>),
}

impl<B: SimBackend> ExprEvaluator<B> {
    pub fn eval(&self, sim: &mut Simulator<B>) -> BigUint {
        match &self.kind {
            ExprEvalKind::Constant(v) => v.clone(),
            ExprEvalKind::Signal(sig) => sim.get(*sig),
            ExprEvalKind::Unary(op, inner) => eval_unary(*op, inner.eval(sim)),
            ExprEvalKind::Binary(l, op, r) => eval_binary(l.eval(sim), *op, r.eval(sim)),
            ExprEvalKind::Ternary(c, t, e) => if c.eval(sim) != BigUint::ZERO { t.eval(sim) } else { e.eval(sim) },
            ExprEvalKind::_Phantom(_) => unreachable!(),
        }
    }
    fn constant(v: BigUint) -> Self { Self { kind: ExprEvalKind::Constant(v) } }
    fn signal(s: SignalRef) -> Self { Self { kind: ExprEvalKind::Signal(s) } }
    fn unary(op: Op, i: Self) -> Self { Self { kind: ExprEvalKind::Unary(op, Box::new(i)) } }
    fn binary(l: Self, op: Op, r: Self) -> Self { Self { kind: ExprEvalKind::Binary(Box::new(l), op, Box::new(r)) } }
    fn ternary(c: Self, t: Self, e: Self) -> Self { Self { kind: ExprEvalKind::Ternary(Box::new(c), Box::new(t), Box::new(e)) } }
}

fn eval_unary(op: Op, val: BigUint) -> BigUint {
    match op { Op::LogicNot | Op::BitNot => bool_to_bu(val == BigUint::ZERO), _ => val }
}
fn eval_binary(l: BigUint, op: Op, r: BigUint) -> BigUint {
    match op {
        Op::Add => l + r, Op::Sub => if l >= r { l - r } else { BigUint::ZERO },
        Op::Mul => l * r, Op::Div => if r == BigUint::ZERO { BigUint::ZERO } else { l / r },
        Op::Rem => if r == BigUint::ZERO { BigUint::ZERO } else { l % r },
        Op::BitAnd => l & r, Op::BitOr => l | r, Op::BitXor => l ^ r,
        Op::LogicShiftL => { let s: u64 = (&r).try_into().unwrap_or(64); l << s }
        Op::LogicShiftR => { let s: u64 = (&r).try_into().unwrap_or(64); l >> s }
        Op::Eq => bool_to_bu(l == r), Op::Ne => bool_to_bu(l != r),
        Op::Less => bool_to_bu(l < r), Op::LessEq => bool_to_bu(l <= r),
        Op::Greater => bool_to_bu(l > r), Op::GreaterEq => bool_to_bu(l >= r),
        Op::LogicAnd => bool_to_bu(l != BigUint::ZERO && r != BigUint::ZERO),
        Op::LogicOr => bool_to_bu(l != BigUint::ZERO || r != BigUint::ZERO),
        _ => BigUint::ZERO,
    }
}
fn bool_to_bu(b: bool) -> BigUint { if b { BigUint::from(1u32) } else { BigUint::ZERO } }

pub struct TestbenchBuilder<'a, B: SimBackend> {
    sim: &'a Simulator<B>,
    event_map: std::collections::HashMap<StrId, B::Event>,
    signal_map: std::collections::HashMap<StrId, SignalRef>,
    default_reset_duration: u64,
}

impl<'a, B: SimBackend> TestbenchBuilder<'a, B> {
    pub fn new(sim: &'a Simulator<B>) -> Self {
        Self { sim, event_map: Default::default(), signal_map: Default::default(), default_reset_duration: 3 }
    }

    pub fn build_event_map(&mut self, stmts: &[Statement]) {
        let mut clock_insts: Vec<StrId> = Vec::new();
        let mut reset_insts: Vec<StrId> = Vec::new();
        Self::scan_tb_methods(stmts, &mut clock_insts, &mut reset_insts);
        let program = self.sim.program();
        for inst in clock_insts.iter().chain(reset_insts.iter()) {
            let name = veryl_parser::resource_table::get_str_value(*inst).unwrap_or_default();
            if let Ok(addr) = program.get_addr(&[], &[&name]) {
                if let Some(event) = self.sim.backend_ref().resolve_event_opt(&addr) {
                    self.event_map.insert(*inst, event);
                }
                self.signal_map.insert(*inst, self.sim.backend_ref().resolve_signal(&addr));
            }
        }
    }

    fn scan_tb_methods(stmts: &[Statement], clks: &mut Vec<StrId>, rsts: &mut Vec<StrId>) {
        for stmt in stmts {
            match stmt {
                Statement::TbMethodCall(tb) => match &tb.method {
                    TbMethod::ClockNext { .. } => { if !clks.contains(&tb.inst) { clks.push(tb.inst); } }
                    TbMethod::ResetAssert { clock, .. } => {
                        if !rsts.contains(&tb.inst) { rsts.push(tb.inst); }
                        if !clks.contains(clock) { clks.push(*clock); }
                    }
                },
                Statement::If(s) => { Self::scan_tb_methods(&s.true_side, clks, rsts); Self::scan_tb_methods(&s.false_side, clks, rsts); }
                Statement::IfReset(s) => { Self::scan_tb_methods(&s.true_side, clks, rsts); Self::scan_tb_methods(&s.false_side, clks, rsts); }
                Statement::For(s) => { Self::scan_tb_methods(&s.body, clks, rsts); }
                _ => {}
            }
        }
    }

    pub fn convert(&self, stmts: &[Statement]) -> Vec<TestbenchStatement<B>> {
        stmts.iter().filter_map(|s| self.convert_stmt(s)).collect()
    }

    fn convert_stmt(&self, stmt: &Statement) -> Option<TestbenchStatement<B>> {
        match stmt {
            Statement::TbMethodCall(tb) => self.convert_tb_method(tb),
            Statement::SystemFunctionCall(sf) => match &sf.kind {
                SystemFunctionKind::Assert(cond, msg) => Some(TestbenchStatement::Assert {
                    evaluator: self.compile_expr(&cond.0),
                    message: msg.as_ref().map(|m| format!("{}", m.0)),
                }),
                SystemFunctionKind::Finish => Some(TestbenchStatement::Finish),
                _ => None,
            },
            Statement::If(s) => Some(TestbenchStatement::If {
                evaluator: self.compile_expr(&s.cond),
                then_block: self.convert(&s.true_side),
                else_block: self.convert(&s.false_side),
            }),
            Statement::For(s) => {
                let body = self.convert(&s.body);
                let lv = self.resolve_loop_var(&s.var_id);
                match &s.range {
                    ForRange::Forward { start, end, step } => Some(TestbenchStatement::For { loop_var: lv, start: *start, end: *end, step: *step, reverse: false, body }),
                    ForRange::Reverse { start, end, step } => Some(TestbenchStatement::For { loop_var: lv, start: *start, end: *end, step: *step, reverse: true, body }),
                    ForRange::Stepped { start, end, step, .. } => Some(TestbenchStatement::For { loop_var: lv, start: *start, end: *end, step: *step, reverse: false, body }),
                }
            }
            Statement::Assign(a) => {
                let ev = self.compile_expr(&a.expr);
                a.dst.first().and_then(|d| self.resolve_var_signal(&d.id)).map(|dst| TestbenchStatement::Assign { dst, evaluator: ev })
            }
            _ => None,
        }
    }

    fn convert_tb_method(&self, tb: &TbMethodCall) -> Option<TestbenchStatement<B>> {
        match &tb.method {
            TbMethod::ClockNext { count, .. } => {
                let ev = self.event_map.get(&tb.inst).copied()?;
                let n = count.as_ref().and_then(|e| self.try_eval_const(e)).unwrap_or(1);
                Some(TestbenchStatement::ClockNext { clock_event: ev, count: n })
            }
            TbMethod::ResetAssert { clock, duration } => {
                let reset_event = self.event_map.get(&tb.inst).copied()?;
                let reset_signal = self.signal_map.get(&tb.inst).copied()?;
                let clock_event = self.event_map.get(clock).copied()?;
                let dur = duration.as_ref().and_then(|e| self.try_eval_const(e)).unwrap_or(self.default_reset_duration);
                Some(TestbenchStatement::ResetAssert { _reset_event: reset_event, reset_signal, clock_event, duration: dur })
            }
        }
    }

    fn compile_expr(&self, expr: &Expression) -> ExprEvaluator<B> {
        match expr {
            Expression::Term(f) => self.compile_factor(f),
            Expression::Unary(op, i, _) => ExprEvaluator::unary(*op, self.compile_expr(i)),
            Expression::Binary(l, op, r, _) => ExprEvaluator::binary(self.compile_expr(l), *op, self.compile_expr(r)),
            Expression::Ternary(c, t, e, _) => ExprEvaluator::ternary(self.compile_expr(c), self.compile_expr(t), self.compile_expr(e)),
            _ => ExprEvaluator::constant(BigUint::ZERO),
        }
    }

    fn compile_factor(&self, factor: &Factor) -> ExprEvaluator<B> {
        match factor {
            Factor::Variable(var_id, _, _, _comptime) => {
                if let Some(sig) = self.resolve_var_signal(var_id) {
                    ExprEvaluator::signal(sig)
                } else {
                    ExprEvaluator::constant(BigUint::ZERO)
                }
            }
            Factor::Value(comptime) => {
                if let Ok(val) = comptime.get_value() {
                    ExprEvaluator::constant(val.payload().into_owned())
                } else {
                    ExprEvaluator::constant(BigUint::ZERO)
                }
            }
            _ => ExprEvaluator::constant(BigUint::ZERO),
        }
    }

    fn resolve_var_signal(&self, var_id: &VarId) -> Option<SignalRef> {
        let p = self.sim.program();
        let rid = p.instance_ids.get(&crate::ir::InstancePath(Vec::new()))?;
        let mid = p.instance_module.get(rid)?;
        let vars = p.module_variables.get(mid)?;
        let _ = vars.get(var_id)?;
        Some(self.sim.backend_ref().resolve_signal(&AbsoluteAddr { instance_id: *rid, var_id: *var_id }))
    }

    fn resolve_loop_var(&self, var_id: &VarId) -> Option<(SignalRef, usize)> {
        let p = self.sim.program();
        let rid = p.instance_ids.get(&crate::ir::InstancePath(Vec::new()))?;
        let mid = p.instance_module.get(rid)?;
        let vars = p.module_variables.get(mid)?;
        let info = vars.get(var_id)?;
        Some((self.sim.backend_ref().resolve_signal(&AbsoluteAddr { instance_id: *rid, var_id: *var_id }), info.width))
    }

    fn try_eval_const(&self, expr: &Expression) -> Option<u64> {
        match expr {
            Expression::Term(f) => match f.as_ref() {
                Factor::Value(c) => c.get_value().ok().map(|v| v.payload_u64()),
                Factor::Variable(_, _, _, c) => c.get_value().ok().map(|v| v.payload_u64()),
                _ => None,
            },
            _ => None,
        }
    }
}

enum ExecResult { Continue, Finished, Fail(String) }
impl ExecResult { fn should_stop(&self) -> bool { !matches!(self, ExecResult::Continue) } }
impl From<ExecResult> for TestResult {
    fn from(r: ExecResult) -> Self { match r { ExecResult::Continue | ExecResult::Finished => TestResult::Pass, ExecResult::Fail(m) => TestResult::Fail(m) } }
}

pub fn run_testbench<B: SimBackend>(sim: &mut Simulator<B>, stmts: &[TestbenchStatement<B>]) -> TestResult {
    exec(sim, stmts).into()
}

fn exec<B: SimBackend>(sim: &mut Simulator<B>, stmts: &[TestbenchStatement<B>]) -> ExecResult {
    for stmt in stmts { let r = exec_one(sim, stmt); if r.should_stop() { return r; } }
    ExecResult::Continue
}

fn exec_one<B: SimBackend>(sim: &mut Simulator<B>, stmt: &TestbenchStatement<B>) -> ExecResult {
    match stmt {
        TestbenchStatement::ClockNext { clock_event, count } => {
            for _ in 0..*count { if let Err(e) = sim.tick(*clock_event) { return ExecResult::Fail(format!("tick: {e}")); } }
            ExecResult::Continue
        }
        TestbenchStatement::ResetAssert { reset_signal, clock_event, duration, .. } => {
            // Assert reset (active-low: set to 0), tick clock for duration, then deassert (set to 1)
            sim.set(*reset_signal, 0u8);
            for _ in 0..*duration { if let Err(e) = sim.tick(*clock_event) { return ExecResult::Fail(format!("reset: {e}")); } }
            sim.set(*reset_signal, 1u8);
            ExecResult::Continue
        }
        TestbenchStatement::Assert { evaluator, message } => {
            if let Err(e) = sim.eval_comb() { return ExecResult::Fail(format!("eval_comb: {e}")); }
            let val = evaluator.eval(sim);
            if val == BigUint::ZERO { ExecResult::Fail(message.as_deref().unwrap_or("assertion failed").to_string()) }
            else { ExecResult::Continue }
        }
        TestbenchStatement::If { evaluator, then_block, else_block } => {
            if let Err(e) = sim.eval_comb() { return ExecResult::Fail(format!("eval_comb: {e}")); }
            if evaluator.eval(sim) != BigUint::ZERO { exec(sim, then_block) } else { exec(sim, else_block) }
        }
        TestbenchStatement::For { loop_var, start, end, step, reverse, body } => {
            if *reverse {
                let mut i = *end;
                while i > *start { i -= step; if let Some((sig, _)) = loop_var { sim.set(*sig, i as u64); } let r = exec(sim, body); if r.should_stop() { return r; } }
            } else {
                let mut i = *start;
                while i < *end { if let Some((sig, _)) = loop_var { sim.set(*sig, i as u64); } let r = exec(sim, body); if r.should_stop() { return r; } i += step; }
            }
            ExecResult::Continue
        }
        TestbenchStatement::Assign { dst, evaluator } => {
            if let Err(e) = sim.eval_comb() { return ExecResult::Fail(format!("eval_comb: {e}")); }
            let val = evaluator.eval(sim);
            sim.set_wide(*dst, val);
            ExecResult::Continue
        }
        TestbenchStatement::Finish => ExecResult::Finished,
    }
}
