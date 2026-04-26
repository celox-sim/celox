use std::collections::VecDeque;

use crate::ir::{
    RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIRTerminator, TriggerSet, UnaryOp,
    VarAtomBase, WORKING_REGION,
};
use crate::{
    HashMap, HashSet,
    parser::{
        BuildConfig, LoweringPhase, ParserError,
        bitaccess::{eval_constexpr, get_access_width},
    },
};
use bit_set::BitSet;
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive;

use veryl_analyzer::ir::{
    Expression, Factor, FfDeclaration, FfReset, ForBound, ForRange, ForStatement, IfResetStatement,
    IfStatement, Module, Op, Statement, TypeKind, ValueVariant, VarId,
};
use veryl_analyzer::value::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopBoundStatus {
    FitsLoopType,
    ExclusiveUpperSentinel,
    OutOfRange,
}

#[cfg(test)]
mod loop_bound_status_tests {
    use super::{FfParser, LoopBoundStatus};
    use veryl_analyzer::ir::ForBound;

    #[test]
    fn allows_exclusive_upper_sentinel() {
        assert_eq!(
            FfParser::loop_bound_status(&ForBound::Const(255), 8, false),
            Some(LoopBoundStatus::FitsLoopType)
        );
        assert_eq!(
            FfParser::loop_bound_status(&ForBound::Const(256), 8, false),
            Some(LoopBoundStatus::ExclusiveUpperSentinel)
        );
        assert_eq!(
            FfParser::loop_bound_status(&ForBound::Const(257), 8, false),
            Some(LoopBoundStatus::OutOfRange)
        );
    }
}

mod expression;
mod function_call;

pub enum Domain {
    Ff, // TODO: add clock
}
impl Domain {
    pub fn region(&self) -> u32 {
        match self {
            Domain::Ff => WORKING_REGION,
        }
    }
}

pub struct FfParser<'a> {
    module: &'a Module,
    stack: VecDeque<RegisterId>,
    defined_ranges: HashMap<VarId, BitSet>,
    dynamic_defined_vars: HashSet<VarId>,
    local_working_vars: HashSet<VarId>,
    loop_exit_blocks: Vec<crate::ir::BlockId>,
    reset: Option<FfReset>,
    function_arg_stack: Vec<HashMap<VarId, Expression>>,
    config: BuildConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFlow {
    Continue,
    Break,
}

impl<'a> FfParser<'a> {
    pub fn new(module: &'a Module, config: BuildConfig) -> Self {
        Self {
            module,
            stack: VecDeque::new(),
            defined_ranges: HashMap::default(),
            dynamic_defined_vars: HashSet::default(),
            local_working_vars: HashSet::default(),
            loop_exit_blocks: Vec::new(),
            reset: None,
            function_arg_stack: Vec::new(),
            config,
        }
    }

    fn get_constant_value(&self, expr: &Expression) -> Option<u64> {
        eval_constexpr(expr)?.to_u64()
    }

    fn get_cast_target_info(&self, expr: &Expression) -> Option<(usize, bool, bool)> {
        let Expression::Term(factor) = expr else {
            return None;
        };
        let Factor::Value(comptime) = factor.as_ref() else {
            return None;
        };
        match &comptime.value {
            ValueVariant::Type(ty) => {
                let width = ty.total_width()?;
                let signed = ty.signed;
                let is_2state = ty.is_2state();
                Some((width, signed, is_2state))
            }
            ValueVariant::Numeric(v) => {
                let width = v.to_usize()?;
                // Numeric width cast is unsigned, 2-state (bit)
                Some((width, false, true))
            }
            _ => None,
        }
    }

    fn cast_reg_width_ext<A>(
        &self,
        ir_builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        target_width: usize,
        signed: bool,
    ) -> RegisterId {
        let src_width = ir_builder.register(&reg).width();
        if src_width == target_width {
            reg
        } else if src_width < target_width {
            let dest = ir_builder.alloc_bit(target_width, signed);
            if signed {
                let sign = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Slice(sign, reg, src_width - 1, 1));
                let pad_width = target_width - src_width;
                let pad = if pad_width == 1 {
                    sign
                } else {
                    let ext = ir_builder.alloc_bit(pad_width, true);
                    ir_builder.emit(SIRInstruction::Concat(
                        ext,
                        std::iter::repeat_n(sign, pad_width).collect(),
                    ));
                    ext
                };
                ir_builder.emit(SIRInstruction::Concat(dest, vec![pad, reg]));
            } else {
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::Ident, reg));
            }
            dest
        } else {
            let mask_val =
                (crate::BigUint::from(1u64) << target_width) - crate::BigUint::from(1u64);
            let mask = ir_builder.alloc_bit(target_width, false);
            ir_builder.emit(SIRInstruction::Imm(
                mask,
                crate::ir::SIRValue::new(mask_val),
            ));
            let dest = ir_builder.alloc_bit(target_width, signed);
            ir_builder.emit(SIRInstruction::Binary(
                dest,
                reg,
                crate::ir::BinaryOp::And,
                mask,
            ));
            dest
        }
    }

    fn get_expression_width(&self, expr: &Expression) -> usize {
        match expr {
            Expression::Binary(left, op, right, _) => {
                let lw = self.get_expression_width(left);
                let rw = self.get_expression_width(right);
                match op {
                    Op::Eq
                    | Op::Ne
                    | Op::Less
                    | Op::LessEq
                    | Op::Greater
                    | Op::GreaterEq
                    | Op::LogicAnd
                    | Op::LogicOr
                    | Op::LogicNot => 1,
                    // Shift/pow result width is determined by the LHS only
                    // (IEEE 1800-2023 §11.4.10).
                    Op::LogicShiftL
                    | Op::LogicShiftR
                    | Op::ArithShiftL
                    | Op::ArithShiftR
                    | Op::Pow => lw,
                    _ => lw.max(rw),
                }
            }
            Expression::Unary(op, expr, _) => match op {
                Op::LogicNot
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::BitXnor
                | Op::BitNand
                | Op::BitNor => 1,
                _ => self.get_expression_width(expr),
            },
            Expression::Term(factor) => self.get_factor_width(factor),
            Expression::Ternary(_, then, els, _) => self
                .get_expression_width(then)
                .max(self.get_expression_width(els)),
            Expression::Concatenation(exprs, _) => {
                let mut total = 0;
                for (expr, replication) in exprs {
                    let w = self.get_expression_width(expr);
                    let rep = if let Some(rep_expr) = replication {
                        self.get_constant_value(rep_expr).unwrap_or(1) as usize
                    } else {
                        1
                    };
                    total += w * rep;
                }
                total
            }
            _ => 64,
        }
    }

    fn get_factor_width(&self, factor: &Factor) -> usize {
        match factor {
            Factor::Variable(var_id, index, select, _) => {
                get_access_width(self.module, *var_id, index, select).unwrap_or(64)
            }
            Factor::Value(comptime) => {
                if let Ok(v) = comptime.get_value() {
                    v.width()
                } else {
                    64
                }
            }
            Factor::FunctionCall(call) => call.comptime.r#type.total_width().unwrap_or(64),
            _ => 64,
        }
    }

    // expression / function-call lowering is split into submodules:
    // - parser/ff/expression.rs
    // - parser/ff/function_call.rs
    fn parse_statement_list<A>(
        &mut self,
        stmts: &[Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        for stmt in stmts {
            let flow = self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            if matches!(flow, ControlFlow::Break) {
                return Ok(ControlFlow::Break);
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn parse_statement_refs<A>(
        &mut self,
        stmts: &[&Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        for stmt in stmts {
            let flow = self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            if matches!(flow, ControlFlow::Break) {
                return Ok(ControlFlow::Break);
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn parse_if_statement<A>(
        &mut self,
        stmt: &IfStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        // Constant folding: if condition is compile-time constant, inline the appropriate side
        if let Some(const_val) = self.get_constant_value(&stmt.cond) {
            let side = if const_val != 0 {
                &stmt.true_side
            } else {
                &stmt.false_side
            };
            return self.parse_statement_list(side, targets, domain, convert, sources, ir_builder);
        }

        // 1. Evaluate condition expression
        self.parse_expression(
            &stmt.cond, targets, domain, convert, sources, ir_builder, None,
        )?;
        let cond_reg = self.stack.pop_back().unwrap();

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // --- Create snapshot ---
        // Save both static (BitSet) and dynamic (HashSet) states
        let pre_if_defined = self.defined_ranges.clone();
        let pre_if_dynamic = self.dynamic_defined_vars.clone(); // 【追加】

        // 2. Terminate current block with Branch
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // 3. Then Path
        ir_builder.switch_to_block(then_bb);
        let then_flow = self.parse_statement_list(
            &stmt.true_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        // Collect state at the end of Then, and restore state at the beginning
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic); // 【追加】

        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 4. Else Path
        ir_builder.switch_to_block(else_bb);
        let else_flow = self.parse_statement_list(
            &stmt.false_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        // Collect state at the end of Else
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars); // 【追加】

        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 5. Merge logic
        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
                self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined_ranges = then_defined;
                self.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined_ranges = else_defined;
                self.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

    /// Helper to take intersection of dynamic defined variables
    fn intersect_dynamic_vars(
        &self,
        mut left: HashSet<VarId>,
        right: HashSet<VarId>,
    ) -> HashSet<VarId> {
        left.retain(|var_id| right.contains(var_id));
        left
    }

    /// Helper to take intersection of defined states of two paths
    fn intersect_defined_states(
        &self,
        mut left: HashMap<VarId, BitSet>,
        right: HashMap<VarId, BitSet>,
    ) -> HashMap<VarId, BitSet> {
        let mut result = HashMap::default();

        // Take bitwise AND only for variables existing in both
        for (var_id, left_bits) in left.drain() {
            if let Some(right_bits) = right.get(&var_id) {
                // If the result of AND is not empty, keep it as "defined" after merging
                if left_bits.intersection(right_bits).next().is_some() {
                    result.insert(var_id, left_bits);
                }
            }
        }
        result
    }
    fn parse_statement<A>(
        &mut self,
        stmt: &Statement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        match stmt {
            Statement::Assign(assign_statement) => {
                self.parse_assign_statement(
                    assign_statement,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            Statement::If(stmt) => {
                return self
                    .parse_if_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::IfReset(stmt) => {
                return self
                    .parse_if_reset_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::Null => {}
            Statement::SystemFunctionCall(call) => {
                return Err(ParserError::unsupported(
                    66,
                    LoweringPhase::FfLowering,
                    "system function call",
                    format!("{call}"),
                    Some(&call.comptime.token),
                ));
            }
            Statement::FunctionCall(call) => {
                self.parse_function_call_statement(
                    call, targets, domain, convert, sources, ir_builder,
                )?;
            }
            Statement::For(f) => {
                self.parse_for_statement(f, targets, domain, convert, sources, ir_builder)?;
            }
            Statement::Break => {
                let Some(exit_bb) = self.loop_exit_blocks.last().copied() else {
                    return Err(ParserError::illegal_context(
                        "statement in always_ff",
                        "break outside loop".to_string(),
                        None,
                    ));
                };
                ir_builder.seal_block(SIRTerminator::Jump(exit_bb, vec![]));
                return Ok(ControlFlow::Break);
            }
            Statement::TbMethodCall(_) | Statement::Unsupported(_) => {
                return Err(ParserError::illegal_context(
                    "statement in always_ff",
                    format!("{stmt}"),
                    None,
                ));
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn loop_bound_status(bound: &ForBound, width: usize, signed: bool) -> Option<LoopBoundStatus> {
        let value = match bound {
            ForBound::Const(v) => BigInt::from(*v),
            ForBound::Expression(expr) => {
                if !expr.comptime().is_const {
                    return None;
                }
                let value = expr.comptime().get_value().ok()?;
                match value {
                    Value::U64(v) => {
                        if v.signed {
                            BigInt::from(v.to_i64()?)
                        } else {
                            BigInt::from(v.to_u64()?)
                        }
                    }
                    Value::BigUint(v) => {
                        if v.signed {
                            v.to_bigint()?
                        } else {
                            BigInt::from_biguint(Sign::Plus, (*v.payload).clone())
                        }
                    }
                }
            }
        };

        if signed {
            let max = (BigInt::from(1u8) << (width.saturating_sub(1))) - BigInt::from(1u8);
            let min = -(BigInt::from(1u8) << (width.saturating_sub(1)));
            Some(if value >= min && value <= max {
                LoopBoundStatus::FitsLoopType
            } else if value == max + BigInt::from(1u8) {
                LoopBoundStatus::ExclusiveUpperSentinel
            } else {
                LoopBoundStatus::OutOfRange
            })
        } else {
            let max = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let max = BigInt::from_biguint(Sign::Plus, max);
            Some(if value.sign() != Sign::Minus && value <= max {
                LoopBoundStatus::FitsLoopType
            } else if value == max + BigInt::from(1u8) {
                LoopBoundStatus::ExclusiveUpperSentinel
            } else {
                LoopBoundStatus::OutOfRange
            })
        }
    }

    fn loop_bound_width(bound: &ForBound, signed: bool) -> Option<usize> {
        match bound {
            ForBound::Const(v) => {
                let value = BigInt::from(*v);
                Some(if signed {
                    if value.sign() == Sign::Minus {
                        let magnitude = (-value - BigInt::from(1u8)).to_biguint()?;
                        magnitude.bits() as usize + 1
                    } else {
                        value.to_biguint()?.bits() as usize + 1
                    }
                } else {
                    let magnitude = value.to_biguint()?;
                    (magnitude.bits() as usize).max(1)
                })
            }
            ForBound::Expression(expr) => expr.comptime().r#type.total_width(),
        }
    }

    fn step_math_width(base_width: usize, stepped_op: Option<Op>, step: usize) -> usize {
        match stepped_op {
            Some(Op::Mul) => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            Some(Op::LogicShiftL | Op::ArithShiftL) => base_width.saturating_add(step.max(1)),
            Some(Op::Add) | None => {
                if step <= 1 {
                    return base_width;
                }
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            Some(_) => base_width,
        }
    }
    fn parse_for_bound<A>(
        &mut self,
        bound: &ForBound,
        canonical_width: usize,
        width: usize,
        signed: bool,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        match bound {
            ForBound::Const(v) => {
                let reg = ir_builder.alloc_bit(width, signed);
                ir_builder.emit(SIRInstruction::Imm(
                    reg,
                    crate::ir::SIRValue::new(*v as u64),
                ));
                Ok(reg)
            }
            ForBound::Expression(expr) => {
                self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
                let reg = self.stack.pop_back().unwrap();
                let source_signed = expr.comptime().r#type.signed;
                let extend_signed = source_signed && signed;
                let canonical =
                    self.cast_reg_width_ext(ir_builder, reg, canonical_width, extend_signed);
                let canonical = match ir_builder.register(&canonical) {
                    RegisterType::Bit {
                        width: reg_width,
                        signed: reg_signed,
                    } if *reg_width == canonical_width && *reg_signed == signed => canonical,
                    _ => {
                        let bit_reg = ir_builder.alloc_bit(canonical_width, signed);
                        ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, canonical));
                        bit_reg
                    }
                };
                let widened = self.cast_reg_width_ext(ir_builder, canonical, width, signed);
                match ir_builder.register(&widened) {
                    RegisterType::Bit {
                        width: reg_width,
                        signed: reg_signed,
                    } if *reg_width == width && *reg_signed == signed => Ok(widened),
                    _ => {
                        let bit_reg = ir_builder.alloc_bit(width, signed);
                        ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, widened));
                        Ok(bit_reg)
                    }
                }
            }
        }
    }

    fn parse_for_statement<A>(
        &mut self,
        stmt: &ForStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let Some(base_loop_width) = stmt.var_type.total_width() else {
            return Err(ParserError::unsupported(
                65,
                LoweringPhase::FfLowering,
                "for loop variable width",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        };
        let loop_signed = stmt.var_type.signed;

        let (start_bound, end_bound, inclusive, step, reverse, stepped_op, start_const, end_const) =
            match &stmt.range {
                ForRange::Forward {
                    start,
                    end,
                    inclusive,
                    step,
                } => (
                    start,
                    end,
                    *inclusive,
                    *step,
                    false,
                    None,
                    Self::bound_const_value(start),
                    Self::bound_const_value(end),
                ),
                ForRange::Reverse {
                    start,
                    end,
                    inclusive,
                    step,
                } => (
                    start,
                    end,
                    *inclusive,
                    *step,
                    true,
                    None,
                    Self::bound_const_value(start),
                    Self::bound_const_value(end),
                ),
                ForRange::Stepped {
                    start,
                    end,
                    inclusive,
                    step,
                    op,
                } => (
                    start,
                    end,
                    *inclusive,
                    *step,
                    false,
                    Some(*op),
                    Self::bound_const_value(start),
                    Self::bound_const_value(end),
                ),
            };

        let const_empty = Self::const_range_is_empty(reverse, start_const, end_const, inclusive);
        let const_singleton =
            Self::const_range_is_singleton(reverse, start_const, end_const, inclusive);
        if start_const.is_some()
            && end_const.is_some()
            && Self::step_can_stall(reverse, stepped_op, step, start_const)
            && !const_empty
            && !const_singleton
        {
            return Err(ParserError::unsupported(
                65,
                LoweringPhase::FfLowering,
                "non-progressing for loop in always_ff",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        }

        let loop_width = base_loop_width.max(1);
        let start_bound_width =
            Self::loop_bound_width(start_bound, loop_signed).unwrap_or(loop_width);
        let end_bound_width =
            Self::loop_bound_width(end_bound, loop_signed).unwrap_or(loop_width);
        let start_status = Self::loop_bound_status(start_bound, loop_width, loop_signed);
        let end_status = Self::loop_bound_status(end_bound, loop_width, loop_signed);
        let uses_exclusive_end_sentinel = !inclusive;
        // Veryl now models loop variables as i32. Reject statically invalid
        // bounds, but keep supporting the exclusive upper sentinel used for
        // full-range iteration such as `0..256` on an 8-bit loop variable.
        if matches!(
            start_status,
            Some(LoopBoundStatus::OutOfRange | LoopBoundStatus::ExclusiveUpperSentinel)
        ) || matches!(end_status, Some(LoopBoundStatus::OutOfRange))
            || (inclusive && end_status == Some(LoopBoundStatus::ExclusiveUpperSentinel))
        {
            return Err(ParserError::illegal_context(
                "for loop bound exceeding i32 loop variable",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        }
        let counter_width = loop_width.max(1);
        let bound_width = counter_width.max(start_bound_width).max(end_bound_width);
        let widen_inclusive = inclusive && !loop_signed;
        let compare_width = if widen_inclusive {
            bound_width.saturating_add(1)
        } else if uses_exclusive_end_sentinel {
            bound_width.max(counter_width.saturating_add(1))
        } else {
            bound_width
        };
        let math_width = if reverse {
            Self::step_math_width(compare_width, Some(Op::Add), step)
        } else {
            Self::step_math_width(compare_width, stepped_op, step)
        };

        let start_reg = self.parse_for_bound(
            start_bound,
            compare_width,
            compare_width,
            loop_signed,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let end_reg = self.parse_for_bound(
            end_bound,
            compare_width,
            compare_width,
            loop_signed,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;

        let one_reg = ir_builder.alloc_bit(compare_width, loop_signed);
        ir_builder.emit(SIRInstruction::Imm(one_reg, crate::ir::SIRValue::new(1u64)));
        let end_limit = if widen_inclusive {
            let reg = ir_builder.alloc_bit(compare_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(
                reg,
                end_reg,
                crate::ir::BinaryOp::Add,
                one_reg,
            ));
            reg
        } else {
            end_reg
        };

        let init_reg = if reverse { end_reg } else { start_reg };

        let header_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let fitcheck_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let body_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let precheck_bb = ir_builder.new_block();
        let header_bb = ir_builder.new_block_with(vec![header_counter]);
        let fitcheck_bb = ir_builder.new_block_with(vec![fitcheck_counter]);
        let body_bb = ir_builder.new_block_with(vec![body_counter]);
        let range_error_bb = ir_builder.new_block();
        let stall_bb = ir_builder.new_block();
        let exit_bb = ir_builder.new_block();
        if !reverse && compare_width != loop_width {
            ir_builder.seal_block(SIRTerminator::Jump(precheck_bb, vec![]));
        } else {
            ir_builder.seal_block(SIRTerminator::Jump(header_bb, vec![init_reg]));
        }

        let pre_loop_defined = self.defined_ranges.clone();
        let pre_loop_dynamic = self.dynamic_defined_vars.clone();

        if !reverse && compare_width != loop_width {
            ir_builder.switch_to_block(precheck_bb);
            let precheck_end_visible =
                self.cast_reg_width_ext(ir_builder, end_reg, loop_width, loop_signed);
            let precheck_end_roundtrip =
                self.cast_reg_width_ext(ir_builder, precheck_end_visible, compare_width, loop_signed);
            let end_fits_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                end_fits_reg,
                end_reg,
                crate::ir::BinaryOp::Eq,
                precheck_end_roundtrip,
            ));
            let end_allowed_reg = if !inclusive {
                let sentinel_reg = ir_builder.alloc_bit(compare_width, loop_signed);
                let sentinel_value = if loop_signed {
                    1u64 << (loop_width - 1)
                } else {
                    1u64 << loop_width
                };
                ir_builder.emit(SIRInstruction::Imm(
                    sentinel_reg,
                    crate::ir::SIRValue::new(sentinel_value),
                ));
                let end_is_sentinel_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    end_is_sentinel_reg,
                    end_reg,
                    crate::ir::BinaryOp::Eq,
                    sentinel_reg,
                ));
                let allowed_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    allowed_reg,
                    end_fits_reg,
                    crate::ir::BinaryOp::LogicOr,
                    end_is_sentinel_reg,
                ));
                allowed_reg
            } else {
                end_fits_reg
            };
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: end_allowed_reg,
                true_block: (header_bb, vec![init_reg]),
                false_block: (range_error_bb, vec![]),
            });
        }

        ir_builder.switch_to_block(header_bb);
        if reverse {
            if step == 0 {
                let cmp_op = if loop_signed {
                    if inclusive {
                        crate::ir::BinaryOp::GeS
                    } else {
                        crate::ir::BinaryOp::GtS
                    }
                } else if inclusive {
                    crate::ir::BinaryOp::GeU
                } else {
                    crate::ir::BinaryOp::GtU
                };
                let in_range = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    in_range,
                    header_counter,
                    cmp_op,
                    start_reg,
                ));
                let singleton = if inclusive {
                    let eq = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(
                        eq,
                        header_counter,
                        crate::ir::BinaryOp::Eq,
                        start_reg,
                    ));
                    Some(eq)
                } else {
                    None
                };
                let singleton_bb = ir_builder.new_block();
                let true_loop_bb = ir_builder.new_block();
                let in_range_bb = ir_builder.new_block();
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: in_range,
                    true_block: (in_range_bb, vec![]),
                    false_block: (exit_bb, vec![]),
                });
                ir_builder.switch_to_block(in_range_bb);
                if let Some(singleton) = singleton {
                    ir_builder.seal_block(SIRTerminator::Branch {
                        cond: singleton,
                        true_block: (singleton_bb, vec![header_counter]),
                        false_block: (true_loop_bb, vec![]),
                    });
                } else {
                    ir_builder.seal_block(SIRTerminator::Jump(true_loop_bb, vec![]));
                }
                ir_builder.switch_to_block(true_loop_bb);
                ir_builder.seal_block(SIRTerminator::Error(1));
                ir_builder.switch_to_block(singleton_bb);
                ir_builder.seal_block(SIRTerminator::Jump(fitcheck_bb, vec![header_counter]));
            } else {
                let loop_math =
                    self.cast_reg_width_ext(ir_builder, header_counter, math_width, loop_signed);
                let start_math =
                    self.cast_reg_width_ext(ir_builder, start_reg, math_width, loop_signed);
                let step_reg = ir_builder.alloc_bit(math_width, loop_signed);
                ir_builder.emit(SIRInstruction::Imm(
                    step_reg,
                    crate::ir::SIRValue::new(step as u64),
                ));
                let threshold_reg = ir_builder.alloc_bit(math_width, loop_signed);
                ir_builder.emit(SIRInstruction::Binary(
                    threshold_reg,
                    start_math,
                    crate::ir::BinaryOp::Add,
                    step_reg,
                ));
                let cond_lhs = if inclusive { start_math } else { threshold_reg };
                let cond_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    cond_reg,
                    loop_math,
                    if loop_signed {
                        crate::ir::BinaryOp::GeS
                    } else {
                        crate::ir::BinaryOp::GeU
                    },
                    cond_lhs,
                ));
                let body_counter_reg = if inclusive {
                    header_counter
                } else {
                    let body_math = ir_builder.alloc_bit(math_width, loop_signed);
                    ir_builder.emit(SIRInstruction::Binary(
                        body_math,
                        loop_math,
                        crate::ir::BinaryOp::Sub,
                        step_reg,
                    ));
                    self.cast_reg_width_ext(ir_builder, body_math, compare_width, loop_signed)
                };
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: cond_reg,
                    true_block: (fitcheck_bb, vec![body_counter_reg]),
                    false_block: (exit_bb, vec![]),
                });
            }
        } else {
            let cond_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                cond_reg,
                header_counter,
                if loop_signed {
                    if inclusive {
                        crate::ir::BinaryOp::LeS
                    } else {
                        crate::ir::BinaryOp::LtS
                    }
                } else {
                    crate::ir::BinaryOp::LtU
                },
                end_limit,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (fitcheck_bb, vec![header_counter]),
                false_block: (exit_bb, vec![]),
            });
        }

        ir_builder.switch_to_block(fitcheck_bb);
        let fitcheck_visible_reg =
            self.cast_reg_width_ext(ir_builder, fitcheck_counter, loop_width, loop_signed);
        // Publish the loop variable before entering the body block so the
        // body itself stays a single widened block for native codegen.
        ir_builder.emit(SIRInstruction::Store(
            convert(stmt.var_id, domain.region()),
            crate::ir::SIROffset::Static(0),
            loop_width,
            fitcheck_visible_reg,
            Vec::new(),
        ));
        if compare_width != loop_width {
            let visible_roundtrip =
                self.cast_reg_width_ext(ir_builder, fitcheck_visible_reg, compare_width, loop_signed);
            let fits_loop_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                fits_loop_reg,
                fitcheck_counter,
                crate::ir::BinaryOp::Eq,
                visible_roundtrip,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: fits_loop_reg,
                true_block: (body_bb, vec![fitcheck_counter]),
                false_block: (range_error_bb, vec![]),
            });
        } else {
            ir_builder.seal_block(SIRTerminator::Jump(body_bb, vec![fitcheck_counter]));
        }
        ir_builder.switch_to_block(range_error_bb);
        ir_builder.seal_block(SIRTerminator::Error(1));
        ir_builder.switch_to_block(body_bb);
        self.local_working_vars.insert(stmt.var_id);

        let mut local_defined = self.defined_ranges.clone();
        local_defined.insert(stmt.var_id, (0..loop_width).collect());
        let prev_defined = std::mem::replace(&mut self.defined_ranges, local_defined);
        let mut local_dynamic = self.dynamic_defined_vars.clone();
        local_dynamic.insert(stmt.var_id);
        let prev_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, local_dynamic);

        self.loop_exit_blocks.push(exit_bb);
        let body_flow =
            self.parse_statement_list(&stmt.body, targets, domain, convert, sources, ir_builder)?;
        self.loop_exit_blocks.pop();

        self.defined_ranges = prev_defined;
        self.dynamic_defined_vars = prev_dynamic;

        if matches!(body_flow, ControlFlow::Break) {
            self.local_working_vars.remove(&stmt.var_id);
            ir_builder.switch_to_block(exit_bb);
            self.defined_ranges = pre_loop_defined;
            self.dynamic_defined_vars = pre_loop_dynamic;
            return Ok(());
        }

        if !reverse {
            if inclusive {
                let terminal_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    terminal_reg,
                    body_counter,
                    crate::ir::BinaryOp::Eq,
                    end_reg,
                ));
                let advance_bb = ir_builder.new_block();
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: terminal_reg,
                    true_block: (exit_bb, vec![]),
                    false_block: (advance_bb, vec![]),
                });
                ir_builder.switch_to_block(advance_bb);
            }

            let current_math =
                self.cast_reg_width_ext(ir_builder, body_counter, math_width, loop_signed);
            let step_reg = ir_builder.alloc_bit(math_width, loop_signed);
            ir_builder.emit(SIRInstruction::Imm(
                step_reg,
                crate::ir::SIRValue::new(step as u64),
            ));
            let next_reg = ir_builder.alloc_bit(math_width, loop_signed);
            let op = match stepped_op {
                Some(Op::Mul) => crate::ir::BinaryOp::Mul,
                Some(Op::LogicShiftL | Op::ArithShiftL) => crate::ir::BinaryOp::Shl,
                Some(Op::Add) | None => crate::ir::BinaryOp::Add,
                Some(other) => {
                    self.local_working_vars.remove(&stmt.var_id);
                    return Err(ParserError::unsupported(
                        65,
                        LoweringPhase::FfLowering,
                        "for loop step operator in always_ff",
                        format!("{other:?}"),
                        Some(&stmt.token),
                    ));
                }
            };
            ir_builder.emit(SIRInstruction::Binary(next_reg, current_math, op, step_reg));
            let progress_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                progress_reg,
                next_reg,
                crate::ir::BinaryOp::Ne,
                current_math,
            ));
            let continue_bb = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: progress_reg,
                true_block: (continue_bb, vec![]),
                false_block: (stall_bb, vec![]),
            });
            ir_builder.switch_to_block(continue_bb);
            let increasing_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                increasing_reg,
                next_reg,
                if loop_signed {
                    crate::ir::BinaryOp::GtS
                } else {
                    crate::ir::BinaryOp::GtU
                },
                current_math,
            ));
            let end_reg = self.cast_reg_width_ext(ir_builder, end_limit, math_width, loop_signed);
            let in_range_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                in_range_reg,
                next_reg,
                if loop_signed {
                    if inclusive {
                        crate::ir::BinaryOp::LeS
                    } else {
                        crate::ir::BinaryOp::LtS
                    }
                } else {
                    crate::ir::BinaryOp::LtU
                },
                end_reg,
            ));
            let can_continue_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                can_continue_reg,
                increasing_reg,
                crate::ir::BinaryOp::LogicAnd,
                in_range_reg,
            ));
            let next_counter =
                self.cast_reg_width_ext(ir_builder, next_reg, compare_width, loop_signed);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: can_continue_reg,
                true_block: (header_bb, vec![next_counter]),
                false_block: (exit_bb, vec![]),
            });
            ir_builder.switch_to_block(stall_bb);
            ir_builder.seal_block(SIRTerminator::Error(1));
        } else {
            let current_math =
                self.cast_reg_width_ext(ir_builder, body_counter, math_width, loop_signed);
            if step == 0 {
                ir_builder.seal_block(SIRTerminator::Jump(exit_bb, vec![]));
                self.local_working_vars.remove(&stmt.var_id);
                ir_builder.switch_to_block(exit_bb);
                self.defined_ranges = pre_loop_defined;
                self.dynamic_defined_vars = pre_loop_dynamic;
                return Ok(());
            }
            let start_math =
                self.cast_reg_width_ext(ir_builder, start_reg, math_width, loop_signed);
            let step_reg = ir_builder.alloc_bit(math_width, loop_signed);
            ir_builder.emit(SIRInstruction::Imm(
                step_reg,
                crate::ir::SIRValue::new(step as u64),
            ));
            let threshold_reg = ir_builder.alloc_bit(math_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(
                threshold_reg,
                start_math,
                crate::ir::BinaryOp::Add,
                step_reg,
            ));
            let can_continue = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                can_continue,
                current_math,
                if loop_signed {
                    crate::ir::BinaryOp::GeS
                } else {
                    crate::ir::BinaryOp::GeU
                },
                threshold_reg,
            ));
            let next_reg = ir_builder.alloc_bit(math_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(
                next_reg,
                current_math,
                crate::ir::BinaryOp::Sub,
                step_reg,
            ));
            let next_counter =
                self.cast_reg_width_ext(ir_builder, next_reg, compare_width, loop_signed);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: can_continue,
                true_block: (
                    header_bb,
                    vec![if inclusive {
                        next_counter
                    } else {
                        body_counter
                    }],
                ),
                false_block: (exit_bb, vec![]),
            });
        }

        self.local_working_vars.remove(&stmt.var_id);
        ir_builder.switch_to_block(exit_bb);
        self.defined_ranges = pre_loop_defined;
        self.dynamic_defined_vars = pre_loop_dynamic;
        Ok(())
    }

    fn parse_if_reset_statement<A>(
        &mut self,
        stmt: &IfResetStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        let true_side: Vec<&Statement> = stmt.true_side.iter().collect();
        let false_side: Vec<&Statement> = stmt.false_side.iter().collect();
        self.parse_if_reset_internal(
            &true_side,
            &false_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )
    }

    fn parse_if_reset_internal<A>(
        &mut self,
        true_side: &[&Statement],
        false_side: &[&Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        // 1. Load reset signal (used as condition expression)
        let (reset_id, reset_index, reset_select, is_low) = {
            let reset = self
                .reset
                .as_ref()
                .expect("if_reset used without reset signal in FfDeclaration");
            let var = &self.module.variables[&reset.id];
            let is_low = match var.r#type.kind {
                TypeKind::ResetAsyncLow | TypeKind::ResetSyncLow => true,
                TypeKind::Reset => matches!(
                    self.config.reset_type,
                    veryl_metadata::ResetType::AsyncLow | veryl_metadata::ResetType::SyncLow
                ),
                _ => false,
            };
            (reset.id, reset.index.clone(), reset.select.clone(), is_low)
        };

        self.op_load(
            reset_id,
            &reset_index,
            &reset_select,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let mut cond_reg = self.stack.pop_back().unwrap();

        // 1.1 Handle reset polarity (Invert if Low-Active)
        if is_low {
            let inverted_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Unary(
                inverted_reg,
                UnaryOp::LogicNot,
                cond_reg,
            ));
            cond_reg = inverted_reg;
        }

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // --- Create snapshot ---
        let pre_if_defined = self.defined_ranges.clone();
        let pre_if_dynamic = self.dynamic_defined_vars.clone();

        // 2. Terminate current block with Branch
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // 3. Then Path (Reset active)
        ir_builder.switch_to_block(then_bb);
        let then_flow =
            self.parse_statement_refs(true_side, targets, domain, convert, sources, ir_builder)?;
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic);
        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 4. Else Path (Normal operation)
        ir_builder.switch_to_block(else_bb);
        let else_flow =
            self.parse_statement_refs(false_side, targets, domain, convert, sources, ir_builder)?;
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 5. Merge logic (Intersection of defined states of both paths)
        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
                self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined_ranges = then_defined;
                self.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined_ranges = else_defined;
                self.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

    pub fn detect_trigger_set(&self, decl: &FfDeclaration) -> TriggerSet<VarId> {
        let mut trigger_set = TriggerSet {
            clock: decl.clock.id,
            resets: Vec::new(),
        };

        if let Some(reset) = &decl.reset {
            let var = &self.module.variables[&reset.id];
            let is_async = match var.r#type.kind {
                TypeKind::ResetAsyncHigh | TypeKind::ResetAsyncLow => true,
                TypeKind::Reset => matches!(
                    self.config.reset_type,
                    veryl_metadata::ResetType::AsyncHigh | veryl_metadata::ResetType::AsyncLow
                ),
                _ => false,
            };
            if is_async {
                trigger_set.resets.push(reset.id);
            }
        }
        trigger_set
    }

    pub fn parse_ff_group(
        &mut self,
        decls: &[&FfDeclaration],
        ir_builder: &mut SIRBuilder<crate::ir::RegionedVarAddr>,
    ) -> Result<(), ParserError> {
        if decls.is_empty() {
            return Ok(());
        }

        self.defined_ranges.clear();
        self.dynamic_defined_vars.clear();
        self.reset = decls[0].reset.clone();

        let mut targets = Vec::new();
        let mut sources = Vec::new();

        let mut all_true_sides = Vec::new();
        let mut all_false_sides = Vec::new();
        let mut other_statements = Vec::new();

        for decl in decls {
            for stmt in &decl.statements {
                if let Statement::IfReset(if_reset) = stmt {
                    all_true_sides.extend(if_reset.true_side.iter().collect::<Vec<_>>());
                    all_false_sides.extend(if_reset.false_side.iter().collect::<Vec<_>>());
                } else {
                    other_statements.push(stmt);
                }
            }
        }

        for stmt in other_statements {
            self.parse_statement(
                stmt,
                &mut targets,
                &Domain::Ff,
                &|x, region| crate::ir::RegionedVarAddr { var_id: x, region },
                &mut sources,
                ir_builder,
            )?;
        }

        if !all_true_sides.is_empty() || !all_false_sides.is_empty() {
            self.parse_if_reset_internal(
                &all_true_sides,
                &all_false_sides,
                &mut targets,
                &Domain::Ff,
                &|x, region| crate::ir::RegionedVarAddr { var_id: x, region },
                &mut sources,
                ir_builder,
            )?;
        }

        Ok(())
    }

    /// Returns the set of variables written by this FF group (deduplicated).
    /// Used by the caller to generate Commit instructions.
    pub fn collect_written_vars(decls: &[&FfDeclaration]) -> impl Iterator<Item = VarId> {
        let mut seen = crate::HashSet::default();
        decls
            .iter()
            .flat_map(|d| d.statements.iter())
            .flat_map(Self::collect_assigned_var_ids)
            .filter(move |id| seen.insert(*id))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn collect_expr_output_var_ids(expr: &Expression) -> Vec<VarId> {
        match expr {
            Expression::Term(factor) => {
                if let Factor::FunctionCall(call) = factor.as_ref() {
                    call.outputs
                        .values()
                        .flat_map(|dsts| dsts.iter().map(|d| d.id))
                        .collect()
                } else {
                    vec![]
                }
            }
            Expression::Binary(lhs, _, rhs, _) => {
                let mut v = Self::collect_expr_output_var_ids(lhs);
                v.extend(Self::collect_expr_output_var_ids(rhs));
                v
            }
            Expression::Unary(_, inner, _) => Self::collect_expr_output_var_ids(inner),
            Expression::Ternary(cond, then_e, else_e, _) => {
                let mut v = Self::collect_expr_output_var_ids(cond);
                v.extend(Self::collect_expr_output_var_ids(then_e));
                v.extend(Self::collect_expr_output_var_ids(else_e));
                v
            }
            _ => vec![],
        }
    }

    fn collect_assigned_var_ids(stmt: &Statement) -> Vec<VarId> {
        match stmt {
            Statement::Assign(a) => {
                let mut ids: Vec<VarId> = a.dst.iter().map(|d| d.id).collect();
                // Also collect output args of any FunctionCall embedded in the RHS expression
                ids.extend(Self::collect_expr_output_var_ids(&a.expr));
                ids
            }
            Statement::If(s) => s
                .true_side
                .iter()
                .chain(s.false_side.iter())
                .flat_map(Self::collect_assigned_var_ids)
                .collect(),
            Statement::IfReset(s) => s
                .true_side
                .iter()
                .chain(s.false_side.iter())
                .flat_map(Self::collect_assigned_var_ids)
                .collect(),
            Statement::For(s) => s
                .body
                .iter()
                .flat_map(Self::collect_assigned_var_ids)
                .collect(),
            Statement::FunctionCall(call) => call
                .outputs
                .values()
                .flat_map(|dsts| dsts.iter().map(|d| d.id))
                .collect(),
            _ => vec![],
        }
    }

    fn bound_const_value(bound: &ForBound) -> Option<usize> {
        match bound {
            ForBound::Const(v) => Some(*v),
            ForBound::Expression(expr) => eval_constexpr(expr)?.to_usize(),
        }
    }

    fn step_can_stall(
        reverse: bool,
        stepped_op: Option<Op>,
        step: usize,
        start_const: Option<usize>,
    ) -> bool {
        if reverse {
            return step == 0;
        }
        match stepped_op {
            Some(Op::Mul) => step == 0 || step == 1 || start_const == Some(0),
            Some(Op::LogicShiftL | Op::ArithShiftL) => step == 0 || start_const == Some(0),
            Some(Op::Add) | None => step == 0,
            Some(_) => false,
        }
    }

    fn const_range_is_empty(
        reverse: bool,
        start_const: Option<usize>,
        end_const: Option<usize>,
        inclusive: bool,
    ) -> bool {
        let (Some(start), Some(end)) = (start_const, end_const) else {
            return false;
        };
        if reverse {
            if inclusive { end < start } else { end <= start }
        } else if inclusive {
            start > end
        } else {
            start >= end
        }
    }

    fn const_range_is_singleton(
        _reverse: bool,
        start_const: Option<usize>,
        end_const: Option<usize>,
        inclusive: bool,
    ) -> bool {
        let (Some(start), Some(end)) = (start_const, end_const) else {
            return false;
        };
        if !inclusive {
            return false;
        }
        start == end
    }
}
