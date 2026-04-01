//! Native testbench execution for Veryl `#[test]` modules.
//!
//! Testbench expressions are compiled to a flat bytecode (`TbOpcode`) and
//! evaluated by a stack-based VM that reads directly from the simulator's
//! memory buffer.  Signals ≤64 bits use native `u64` arithmetic with zero
//! heap allocation; wider signals fall back to `BigUint`.

use crate::backend::traits::SimBackend;
use crate::backend::get_byte_size;
use crate::ir::{AbsoluteAddr, SignalRef};
use crate::simulator::Simulator;
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    Expression, Factor, ForRange, Op, Statement, SystemFunctionKind, TbMethod, TbMethodCall, VarId,
};
use veryl_parser::resource_table::StrId;

// ── Public types ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestResult {
    Pass,
    Fail(String),
}

/// Clock cycle count: either a compile-time constant or a runtime expression.
pub enum ClockCount {
    Static(u64),
    Dynamic(CompiledExpr),
}

pub enum TestbenchStatement<B: SimBackend> {
    ClockNext {
        clock_event: B::Event,
        count: ClockCount,
    },
    ResetAssert {
        reset_signal: SignalRef,
        clock_event: B::Event,
        duration: u64,
        /// Value to drive when reset is asserted (0 for active-low, 1 for active-high).
        assert_value: u8,
        /// Value to drive when reset is deasserted.
        deassert_value: u8,
    },
    Assert {
        expr: CompiledExpr,
        message: Option<String>,
    },
    If {
        expr: CompiledExpr,
        then_block: Vec<TestbenchStatement<B>>,
        else_block: Vec<TestbenchStatement<B>>,
    },
    For {
        loop_var: Option<(SignalRef, usize)>,
        start: usize,
        end: usize,
        step: usize,
        reverse: bool,
        body: Vec<TestbenchStatement<B>>,
    },
    Assign {
        dst: SignalRef,
        expr: CompiledExpr,
    },
    Finish,
}

// ── Bytecode VM ────────────────────────────────────────────────────────

/// A compiled expression: flat bytecode evaluated on a stack VM.
#[derive(Debug)]
pub struct CompiledExpr {
    ops: Vec<TbOpcode>,
}

/// Bytecode instructions for the testbench expression evaluator.
#[derive(Debug)]
enum TbOpcode {
    /// Push a constant u64.
    ConstU64(u64),
    /// Push a wide constant (>64 bits).
    ConstWide(BigUint),
    /// Read ≤8 bytes from memory at `offset`, zero-extend to u64.
    LoadU64 { offset: usize, byte_size: usize, mask: u64 },
    /// Read >8 bytes from memory, push as BigUint.
    LoadWide { offset: usize, byte_size: usize, width: usize },
    /// Binary operation: pop two values, push result.
    BinOp(Op),
    /// Unary operation: pop one value, push result.
    UnaryOp(Op),
    /// Conditional: pop condition; if non-zero execute `then_len` ops,
    /// otherwise skip them and execute `else_len` ops.
    Ternary { then_len: usize, else_len: usize },
    /// Dynamic array element load: pop index (u64), compute
    /// `base_offset + index * stride_bytes`, read `element_width` bits.
    LoadIndexed {
        base_offset: usize,
        stride_bytes: usize,
        element_byte_size: usize,
        element_width: usize,
    },
    /// Dynamic bit select: pop bit-index (u64), read full value from
    /// `base_offset`, then shift right by bit-index and mask to `select_width`.
    LoadBitSelect {
        base_offset: usize,
        base_byte_size: usize,
        select_width: usize,
    },
    /// Pop value from stack and write to memory (for function arg binding).
    StoreU64 {
        offset: usize,
        byte_size: usize,
    },
}

/// Stack value: either a native u64 or a heap-allocated BigUint.
#[derive(Clone, Debug)]
enum TbValue {
    U64(u64),
    Wide(BigUint),
}

impl TbValue {
    #[inline]
    fn to_u64(&self) -> u64 {
        match self {
            TbValue::U64(v) => *v,
            TbValue::Wide(v) => {
                let digits = v.to_u64_digits();
                digits.first().copied().unwrap_or(0)
            }
        }
    }

    #[inline]
    fn is_zero(&self) -> bool {
        match self {
            TbValue::U64(v) => *v == 0,
            TbValue::Wide(v) => *v == BigUint::ZERO,
        }
    }

    #[inline]
    fn to_biguint(&self) -> BigUint {
        match self {
            TbValue::U64(v) => BigUint::from(*v),
            TbValue::Wide(v) => v.clone(),
        }
    }
}

impl CompiledExpr {
    /// Evaluate against raw simulator memory, returning the result as u64.
    /// For wide results, returns the low 64 bits.
    pub fn eval_u64(&self, memory: *mut u8) -> u64 {
        self.eval(memory).to_u64()
    }

    /// Evaluate and return the full `TbValue` (preserves wide results).
    fn eval_value(&self, memory: *mut u8) -> TbValue {
        self.eval(memory)
    }

    pub fn eval_bool(&self, memory: *mut u8) -> bool {
        !self.eval(memory).is_zero()
    }

    /// Core evaluation loop.  Uses `TbValue` to handle both u64 and wide
    /// signals on a single stack.  The common case (all ≤64-bit operands)
    /// stays in the `TbValue::U64` variant and never allocates.
    fn eval(&self, memory: *mut u8) -> TbValue {
        let mut stack: Vec<TbValue> = Vec::with_capacity(16);
        let mut pc: usize = 0;
        let ops = &self.ops;

        while pc < ops.len() {
            self.exec_at(ops, &mut pc, &mut stack, memory);
        }
        stack.pop().unwrap_or_else(|| {
            debug_assert!(false, "testbench bytecode: stack empty after evaluation");
            TbValue::U64(0)
        })
    }

    /// Execute the opcode at `pc` and advance `pc` past it.
    /// Handles all opcodes including `Ternary` (with recursive sub-block
    /// evaluation), so there is no separate `step()` function.
    fn exec_at(
        &self,
        ops: &[TbOpcode],
        pc: &mut usize,
        stack: &mut Vec<TbValue>,
        memory: *mut u8,
    ) {
        match &ops[*pc] {
            TbOpcode::ConstU64(v) => {
                stack.push(TbValue::U64(*v));
                *pc += 1;
            }
            TbOpcode::ConstWide(v) => {
                stack.push(TbValue::Wide(v.clone()));
                *pc += 1;
            }
            TbOpcode::LoadU64 { offset, byte_size, mask } => {
                // SAFETY: caller guarantees `memory` is valid simulator memory
                let val = unsafe { read_le_u64(memory.add(*offset), *byte_size) } & mask;
                stack.push(TbValue::U64(val));
                *pc += 1;
            }
            TbOpcode::LoadWide { offset, byte_size, width } => {
                let val = unsafe { read_le_wide(memory.add(*offset), *byte_size, *width) };
                stack.push(TbValue::Wide(val));
                *pc += 1;
            }
            TbOpcode::BinOp(op) => {
                let r = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: BinOp rhs underflow");
                    TbValue::U64(0)
                });
                let l = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: BinOp lhs underflow");
                    TbValue::U64(0)
                });
                stack.push(eval_binop(l, *op, r));
                *pc += 1;
            }
            TbOpcode::UnaryOp(op) => {
                if let Some(top) = stack.last_mut() {
                    *top = eval_unop(*op, top);
                } else {
                    debug_assert!(false, "testbench bytecode: UnaryOp underflow");
                }
                *pc += 1;
            }
            TbOpcode::Ternary { then_len, else_len } => {
                let cond = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: Ternary cond underflow");
                    TbValue::U64(0)
                });
                *pc += 1; // skip past Ternary opcode
                if !cond.is_zero() {
                    let then_end = *pc + then_len;
                    while *pc < then_end {
                        self.exec_at(ops, pc, stack, memory);
                    }
                    *pc += else_len; // skip else block
                } else {
                    *pc += then_len; // skip then block
                    let else_end = *pc + else_len;
                    while *pc < else_end {
                        self.exec_at(ops, pc, stack, memory);
                    }
                }
            }
            TbOpcode::LoadIndexed {
                base_offset,
                stride_bytes,
                element_byte_size,
                element_width,
            } => {
                let idx = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: LoadIndexed underflow");
                    TbValue::U64(0)
                });
                let i = idx.to_u64() as usize;
                let offset = base_offset + i * stride_bytes;
                let mask = if *element_width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << element_width) - 1
                };
                let val = unsafe { read_le_u64(memory.add(offset), *element_byte_size) } & mask;
                stack.push(TbValue::U64(val));
                *pc += 1;
            }
            TbOpcode::LoadBitSelect {
                base_offset,
                base_byte_size,
                select_width,
            } => {
                let bit_idx = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: LoadBitSelect underflow");
                    TbValue::U64(0)
                });
                let shift = bit_idx.to_u64() as usize;
                let full_val = unsafe { read_le_u64(memory.add(*base_offset), *base_byte_size) };
                let mask = if *select_width >= 64 {
                    u64::MAX
                } else {
                    (1u64 << select_width) - 1
                };
                let val = (full_val >> shift) & mask;
                stack.push(TbValue::U64(val));
                *pc += 1;
            }
            TbOpcode::StoreU64 { offset, byte_size } => {
                let val = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: StoreU64 underflow");
                    TbValue::U64(0)
                });
                let v = val.to_u64();
                let bytes = v.to_le_bytes();
                let n = (*byte_size).min(8);
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        bytes.as_ptr(),
                        memory.add(*offset),
                        n,
                    );
                }
                *pc += 1;
            }
        }
    }
}

/// # Safety
/// `ptr` must be valid for `byte_size` bytes of read access.
#[inline(always)]
unsafe fn read_le_u64(ptr: *const u8, byte_size: usize) -> u64 {
    let mut buf = [0u8; 8];
    unsafe { std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), byte_size.min(8)); }
    u64::from_le_bytes(buf)
}

/// # Safety
/// `ptr` must be valid for `byte_size` bytes of read access.
unsafe fn read_le_wide(ptr: *const u8, byte_size: usize, width: usize) -> BigUint {
    let mut buf = vec![0u8; byte_size];
    unsafe { std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), byte_size); }
    let mut val = BigUint::from_bytes_le(&buf);
    let extra_bits = byte_size * 8 - width;
    if extra_bits > 0 {
        val &= (BigUint::from(1u32) << width) - BigUint::from(1u32);
    }
    val
}

// ── Typed evaluation ───────────────────────────────────────────────────

/// Binary operation on `TbValue`.  When both operands are `U64` the fast
/// path runs entirely in registers; otherwise we promote to `BigUint`.
#[inline]
fn eval_binop(l: TbValue, op: Op, r: TbValue) -> TbValue {
    match (&l, &r) {
        (TbValue::U64(lv), TbValue::U64(rv)) => TbValue::U64(eval_binop_u64(*lv, op, *rv)),
        _ => {
            let lv = l.to_biguint();
            let rv = r.to_biguint();
            // Comparison / logic ops always return u64
            match op {
                Op::Eq | Op::Ne | Op::Less | Op::LessEq | Op::Greater | Op::GreaterEq
                | Op::LogicAnd | Op::LogicOr => TbValue::U64(eval_binop_wide_cmp(&lv, op, &rv)),
                _ => TbValue::Wide(eval_binop_wide(lv, op, rv)),
            }
        }
    }
}

#[inline]
fn eval_unop(op: Op, val: &TbValue) -> TbValue {
    match val {
        TbValue::U64(v) => TbValue::U64(eval_unop_u64(op, *v)),
        TbValue::Wide(v) => match op {
            Op::LogicNot => TbValue::U64((*v == BigUint::ZERO) as u64),
            Op::BitNot => {
                // For wide values, bitwise NOT without width info is ill-defined.
                // Return logical NOT as a safe default.
                TbValue::U64((*v == BigUint::ZERO) as u64)
            }
            _ => TbValue::Wide(v.clone()),
        },
    }
}

#[inline]
fn eval_binop_u64(l: u64, op: Op, r: u64) -> u64 {
    match op {
        Op::Add => l.wrapping_add(r),
        Op::Sub => l.wrapping_sub(r),
        Op::Mul => l.wrapping_mul(r),
        Op::Div => if r == 0 { 0 } else { l / r },
        Op::Rem => if r == 0 { 0 } else { l % r },
        Op::BitAnd => l & r,
        Op::BitOr => l | r,
        Op::BitXor => l ^ r,
        Op::LogicShiftL => if r >= 64 { 0 } else { l << r },
        Op::LogicShiftR => if r >= 64 { 0 } else { l >> r },
        Op::ArithShiftL => if r >= 64 { 0 } else { l << r },
        Op::ArithShiftR => if r >= 64 { ((l as i64) >> 63) as u64 } else { ((l as i64) >> r) as u64 },
        Op::Eq => (l == r) as u64,
        Op::Ne => (l != r) as u64,
        Op::Less => (l < r) as u64,
        Op::LessEq => (l <= r) as u64,
        Op::Greater => (l > r) as u64,
        Op::GreaterEq => (l >= r) as u64,
        Op::LogicAnd => ((l != 0) && (r != 0)) as u64,
        Op::LogicOr => ((l != 0) || (r != 0)) as u64,
        _ => 0,
    }
}

#[inline]
fn eval_unop_u64(op: Op, val: u64) -> u64 {
    match op {
        Op::LogicNot => (val == 0) as u64,
        Op::BitNot => !val,
        _ => val,
    }
}

fn eval_binop_wide(l: BigUint, op: Op, r: BigUint) -> BigUint {
    match op {
        Op::Add => l + r,
        Op::Sub => if l >= r { l - r } else { BigUint::ZERO },
        Op::Mul => l * r,
        Op::Div => if r == BigUint::ZERO { BigUint::ZERO } else { l / r },
        Op::Rem => if r == BigUint::ZERO { BigUint::ZERO } else { l % r },
        Op::BitAnd => l & r,
        Op::BitOr => l | r,
        Op::BitXor => l ^ r,
        Op::LogicShiftL => {
            let s: u64 = (&r).try_into().unwrap_or(256);
            l << s
        }
        Op::LogicShiftR => {
            let s: u64 = (&r).try_into().unwrap_or(256);
            l >> s
        }
        _ => BigUint::ZERO,
    }
}

fn eval_binop_wide_cmp(l: &BigUint, op: Op, r: &BigUint) -> u64 {
    match op {
        Op::Eq => (l == r) as u64,
        Op::Ne => (l != r) as u64,
        Op::Less => (l < r) as u64,
        Op::LessEq => (l <= r) as u64,
        Op::Greater => (l > r) as u64,
        Op::GreaterEq => (l >= r) as u64,
        Op::LogicAnd => ((*l != BigUint::ZERO) && (*r != BigUint::ZERO)) as u64,
        Op::LogicOr => ((*l != BigUint::ZERO) || (*r != BigUint::ZERO)) as u64,
        _ => 0,
    }
}

// ── Expression compiler ────────────────────────────────────────────────

struct ExprCompiler<'a, B: SimBackend> {
    sim: &'a Simulator<B>,
    /// Root module instance ID, cached for repeated lookups.
    root_instance_id: crate::ir::InstanceId,
    root_module_id: crate::ir::ModuleId,
}

impl<'a, B: SimBackend> ExprCompiler<'a, B> {
    fn compile(&self, expr: &Expression) -> CompiledExpr {
        let mut ops = Vec::new();
        self.emit(expr, &mut ops);
        CompiledExpr { ops }
    }

    fn emit(&self, expr: &Expression, ops: &mut Vec<TbOpcode>) {
        match expr {
            Expression::Term(f) => self.emit_factor(f, ops),
            Expression::Unary(op, inner, _) => {
                self.emit(inner, ops);
                ops.push(TbOpcode::UnaryOp(*op));
            }
            Expression::Binary(lhs, op, rhs, _) => {
                self.emit(lhs, ops);
                self.emit(rhs, ops);
                ops.push(TbOpcode::BinOp(*op));
            }
            Expression::Ternary(cond, then_expr, else_expr, _) => {
                self.emit(cond, ops);
                let mut then_ops = Vec::new();
                self.emit(then_expr, &mut then_ops);
                let mut else_ops = Vec::new();
                self.emit(else_expr, &mut else_ops);
                ops.push(TbOpcode::Ternary {
                    then_len: then_ops.len(),
                    else_len: else_ops.len(),
                });
                ops.extend(then_ops);
                ops.extend(else_ops);
            }
            Expression::Concatenation(parts, _) => {
                // Build from MSB (first) to LSB (last):
                //   acc = 0; for part: acc = (acc << width) | part
                ops.push(TbOpcode::ConstU64(0));
                for (val_expr, repeat_expr) in parts {
                    let part_width = self.infer_expr_width(val_expr);
                    let repeat = repeat_expr
                        .as_ref()
                        .and_then(|e| Self::try_const_usize(e))
                        .unwrap_or(1);
                    for _ in 0..repeat {
                        if part_width > 0 {
                            ops.push(TbOpcode::ConstU64(part_width as u64));
                            ops.push(TbOpcode::BinOp(Op::LogicShiftL));
                        }
                        self.emit(val_expr, ops);
                        if part_width > 0 && part_width < 64 {
                            ops.push(TbOpcode::ConstU64((1u64 << part_width) - 1));
                            ops.push(TbOpcode::BinOp(Op::BitAnd));
                        }
                        ops.push(TbOpcode::BinOp(Op::BitOr));
                    }
                }
            }
            _ => ops.push(TbOpcode::ConstU64(0)),
        }
    }

    fn emit_factor(&self, factor: &Factor, ops: &mut Vec<TbOpcode>) {
        match factor {
            Factor::Variable(var_id, index, select, _) => {
                if let Some(sig) = self.resolve_var(var_id) {
                    self.emit_var_access(var_id, sig, index, select, ops);
                } else {
                    ops.push(TbOpcode::ConstU64(0));
                }
            }
            Factor::Value(comptime) => {
                if let Ok(val) = comptime.get_value() {
                    let width = comptime.expr_context.width;
                    if width <= 64 {
                        ops.push(TbOpcode::ConstU64(val.payload_u64()));
                    } else {
                        ops.push(TbOpcode::ConstWide(val.payload().into_owned()));
                    }
                } else {
                    ops.push(TbOpcode::ConstU64(0));
                }
            }
            Factor::FunctionCall(fc) => {
                self.emit_function_call(fc, ops);
            }
            _ => ops.push(TbOpcode::ConstU64(0)),
        }
    }

    /// Emit bytecode for a function call used as an expression value.
    /// Inline-expands: store args → emit body assigns → load return value.
    fn emit_function_call(
        &self,
        fc: &veryl_analyzer::ir::FunctionCall,
        ops: &mut Vec<TbOpcode>,
    ) {
        let p = self.sim.program();
        let func = match p.tb_functions.get(&fc.id) {
            Some(f) => f,
            None => {
                ops.push(TbOpcode::ConstU64(0));
                return;
            }
        };
        let func_body = match if let Some(idx) = &fc.index {
            func.get_function(idx)
        } else {
            func.get_function(&[])
        } {
            Some(fb) => fb,
            None => {
                ops.push(TbOpcode::ConstU64(0));
                return;
            }
        };

        // 1. Store input arguments into memory
        for (arg_path, arg_expr) in &fc.inputs {
            if let Some(&arg_var_id) = func_body.arg_map.get(arg_path) {
                if let Some(sig) = self.resolve_var(&arg_var_id) {
                    self.emit(arg_expr, ops);
                    ops.push(TbOpcode::StoreU64 {
                        offset: sig.offset,
                        byte_size: get_byte_size(sig.width),
                    });
                }
            }
        }

        // 2. Emit body statements as bytecode (only Assign is supported)
        for stmt in &func_body.statements {
            if let veryl_analyzer::ir::Statement::Assign(a) = stmt {
                if let Some(first_dst) = a.dst.first() {
                    if let Some(dst_sig) = self.resolve_var(&first_dst.id) {
                        self.emit(&a.expr, ops);
                        ops.push(TbOpcode::StoreU64 {
                            offset: dst_sig.offset,
                            byte_size: get_byte_size(dst_sig.width),
                        });
                    }
                }
            }
            // Non-assign statements (if/for in function body) are skipped.
            // This covers the common case of pure computation functions.
        }

        // 3. Load return value
        if let Some(ret_var_id) = &func_body.ret {
            if let Some(sig) = self.resolve_var(ret_var_id) {
                self.emit_load(sig.offset, sig.width, ops);
            } else {
                ops.push(TbOpcode::ConstU64(0));
            }
        } else {
            ops.push(TbOpcode::ConstU64(0));
        }
    }

    /// Emit bytecode for a variable access, handling static and dynamic
    /// array indices and bit selects.
    fn emit_var_access(
        &self,
        var_id: &VarId,
        sig: SignalRef,
        index: &veryl_analyzer::ir::VarIndex,
        select: &veryl_analyzer::ir::VarSelect,
        ops: &mut Vec<TbOpcode>,
    ) {
        let p = self.sim.program();
        let info = match p.module_variables.get(&self.root_module_id).and_then(|v| v.get(var_id)) {
            Some(i) => i,
            None => { self.emit_load(sig.offset, sig.width, ops); return; }
        };

        // No index or select → whole variable
        if index.0.is_empty() && select.0.is_empty() && select.1.is_none() {
            self.emit_load(sig.offset, sig.width, ops);
            return;
        }

        let array_total: usize = info.array_dims.iter().product::<usize>().max(1);
        let element_width = info.width / array_total;

        // Compute array strides
        let mut strides_bits = vec![element_width; info.array_dims.len()];
        if !info.array_dims.is_empty() {
            let mut stride = element_width;
            for i in (0..info.array_dims.len()).rev() {
                strides_bits[i] = stride;
                stride *= info.array_dims[i];
            }
        }

        // Process unpacked array indices
        let mut static_bit_offset: usize = 0;
        let mut dynamic_emitted = false;

        for (i, idx_expr) in index.0.iter().enumerate() {
            if i >= info.array_dims.len() { break; }
            let stride = strides_bits[i];

            if let Some(idx_val) = Self::try_const_usize(idx_expr) {
                // Static index: accumulate into offset
                static_bit_offset += idx_val * stride;
            } else {
                // Dynamic index: emit the index expression, then LoadIndexed
                let base_byte_offset = sig.offset + static_bit_offset / 8;
                let stride_bytes = get_byte_size(stride);
                let elem_byte_size = get_byte_size(element_width);
                self.emit(idx_expr, ops);
                ops.push(TbOpcode::LoadIndexed {
                    base_offset: base_byte_offset,
                    stride_bytes,
                    element_byte_size: elem_byte_size,
                    element_width,
                });
                dynamic_emitted = true;
                // After a dynamic index, remaining indices would need chaining.
                // For now, only single dynamic index is supported.
                break;
            }
        }

        if dynamic_emitted {
            // Apply bit select on top of dynamic result if present
            if select.1.is_some() || !select.0.is_empty() {
                self.emit_post_select(select, element_width, ops);
            }
            return;
        }

        // All indices were static — apply bit select
        let accessed_width = if index.0.len() >= info.array_dims.len() {
            element_width
        } else if index.0.is_empty() {
            info.width
        } else {
            strides_bits[index.0.len() - 1]
        };

        if select.0.is_empty() && select.1.is_none() {
            // No bit select, just load the element
            let byte_offset = sig.offset + static_bit_offset / 8;
            let sub = static_bit_offset % 8;
            if sub == 0 {
                self.emit_load(byte_offset, accessed_width, ops);
            } else {
                let load_width = accessed_width + sub;
                self.emit_load(byte_offset, load_width, ops);
                ops.push(TbOpcode::ConstU64(sub as u64));
                ops.push(TbOpcode::BinOp(Op::LogicShiftR));
                if accessed_width < 64 {
                    ops.push(TbOpcode::ConstU64((1u64 << accessed_width) - 1));
                    ops.push(TbOpcode::BinOp(Op::BitAnd));
                }
            }
            return;
        }

        // Static bit select
        let (sel_lsb, sel_width, is_dynamic_select) =
            self.resolve_select(select, ops);

        if is_dynamic_select {
            // Dynamic bit select: load full value, shift by dynamic amount, mask
            let byte_offset = sig.offset + static_bit_offset / 8;
            let total_byte_size = get_byte_size(accessed_width);
            ops.push(TbOpcode::LoadBitSelect {
                base_offset: byte_offset,
                base_byte_size: total_byte_size,
                select_width: sel_width,
            });
            return;
        }

        let bit_offset = static_bit_offset + sel_lsb;
        let byte_offset = sig.offset + bit_offset / 8;
        let sub = bit_offset % 8;
        if sub == 0 {
            self.emit_load(byte_offset, sel_width, ops);
        } else {
            let load_width = sel_width + sub;
            self.emit_load(byte_offset, load_width, ops);
            ops.push(TbOpcode::ConstU64(sub as u64));
            ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            if sel_width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << sel_width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        }
    }

    /// Resolve a VarSelect to `(lsb, width, is_dynamic)`.
    /// If any index is dynamic, emits the dynamic index expression to `ops`
    /// and returns `is_dynamic = true`.
    fn resolve_select(
        &self,
        select: &veryl_analyzer::ir::VarSelect,
        ops: &mut Vec<TbOpcode>,
    ) -> (usize, usize, bool) {
        if let Some((op, range_expr)) = &select.1 {
            let anchor_expr = select.0.last();
            let anchor = anchor_expr.and_then(|e| Self::try_const_usize(e));
            let range_val = Self::try_const_usize(range_expr);

            if let (Some(a), Some(v)) = (anchor, range_val) {
                let (lsb, msb) = match op {
                    veryl_analyzer::ir::VarSelectOp::Colon => (v, a),
                    veryl_analyzer::ir::VarSelectOp::PlusColon => (a, a + v - 1),
                    veryl_analyzer::ir::VarSelectOp::MinusColon => (a.saturating_sub(v) + 1, a),
                    veryl_analyzer::ir::VarSelectOp::Step => (a * v, (a + 1) * v - 1),
                };
                return (lsb, msb - lsb + 1, false);
            }

            // Dynamic select: emit the anchor expression
            if let Some(anchor_expr) = anchor_expr {
                self.emit(anchor_expr, ops);
            } else {
                ops.push(TbOpcode::ConstU64(0));
            }
            let width = range_val.unwrap_or(1);
            return (0, width, true);
        }

        // Simple bit index (no range)
        if let Some(first) = select.0.first() {
            if let Some(idx) = Self::try_const_usize(first) {
                return (idx, 1, false);
            }
            // Dynamic single bit select
            self.emit(first, ops);
            return (0, 1, true);
        }

        (0, 0, false)
    }

    /// Emit post-load bit select operations on a value already on the stack
    /// (for dynamic array element access followed by bit select).
    fn emit_post_select(
        &self,
        select: &veryl_analyzer::ir::VarSelect,
        _base_width: usize,
        ops: &mut Vec<TbOpcode>,
    ) {
        let (lsb, width, is_dynamic) = self.resolve_select(select, ops);
        if is_dynamic {
            // Stack: [value, bit_index]
            ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            if width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        } else if lsb > 0 || width > 0 {
            if lsb > 0 {
                ops.push(TbOpcode::ConstU64(lsb as u64));
                ops.push(TbOpcode::BinOp(Op::LogicShiftR));
            }
            if width > 0 && width < 64 {
                ops.push(TbOpcode::ConstU64((1u64 << width) - 1));
                ops.push(TbOpcode::BinOp(Op::BitAnd));
            }
        }
    }

    /// Emit a LoadU64 or LoadWide opcode for the given byte offset and bit width.
    fn emit_load(&self, offset: usize, width: usize, ops: &mut Vec<TbOpcode>) {
        let byte_size = get_byte_size(width);
        if byte_size <= 8 {
            let mask = if width >= 64 {
                u64::MAX
            } else {
                (1u64 << width) - 1
            };
            ops.push(TbOpcode::LoadU64 {
                offset,
                byte_size,
                mask,
            });
        } else {
            ops.push(TbOpcode::LoadWide {
                offset,
                byte_size,
                width,
            });
        }
    }

    /// Resolve VarIndex (unpacked array) and VarSelect (bit select) to
    /// a concrete (byte_offset, bit_width) pair.
    ///
    /// For static indices, adjusts the offset and narrows the width.
    /// Dynamic indices are not supported and fall back to the full variable.
    /// Infer the bit width of an expression. Falls back to comptime if available,
    /// otherwise resolves from VariableInfo for variables.
    fn infer_expr_width(&self, expr: &Expression) -> usize {
        let ctx_width = expr.comptime().expr_context.width;
        if ctx_width > 0 {
            return ctx_width;
        }
        // Try type-level width
        if let Some(w) = expr.comptime().r#type.total_width() {
            if w > 0 {
                return w;
            }
        }
        // For terms, look up variable info
        if let Expression::Term(f) = expr {
            match f.as_ref() {
                Factor::Variable(var_id, _, _, _) => {
                    let p = self.sim.program();
                    if let Some(vars) = p.module_variables.get(&self.root_module_id) {
                        if let Some(info) = vars.get(var_id) {
                            return info.width;
                        }
                    }
                }
                Factor::Value(c) => {
                    if let Ok(v) = c.get_value() {
                        return v.width();
                    }
                }
                _ => {}
            }
        }
        0
    }

    fn try_const_usize(expr: &Expression) -> Option<usize> {
        match expr {
            Expression::Term(f) => match f.as_ref() {
                Factor::Value(c) => c.get_value().ok().map(|v| v.payload_u64() as usize),
                Factor::Variable(_, _, _, c) => {
                    c.get_value().ok().map(|v| v.payload_u64() as usize)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn resolve_var(&self, var_id: &VarId) -> Option<SignalRef> {
        let p = self.sim.program();
        let vars = p.module_variables.get(&self.root_module_id)?;
        let _ = vars.get(var_id)?;
        Some(self.sim.backend_ref().resolve_signal(&AbsoluteAddr {
            instance_id: self.root_instance_id,
            var_id: *var_id,
        }))
    }
}

// ── Builder ────────────────────────────────────────────────────────────

pub struct TestbenchBuilder<'a, B: SimBackend> {
    sim: &'a Simulator<B>,
    event_map: std::collections::HashMap<StrId, B::Event>,
    signal_map: std::collections::HashMap<StrId, SignalRef>,
    default_reset_duration: u64,
}

impl<'a, B: SimBackend> TestbenchBuilder<'a, B> {
    pub fn new(sim: &'a Simulator<B>) -> Self {
        Self {
            sim,
            event_map: Default::default(),
            signal_map: Default::default(),
            default_reset_duration: 3,
        }
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
                self.signal_map
                    .insert(*inst, self.sim.backend_ref().resolve_signal(&addr));
            }
        }
    }

    fn scan_tb_methods(stmts: &[Statement], clks: &mut Vec<StrId>, rsts: &mut Vec<StrId>) {
        for stmt in stmts {
            match stmt {
                Statement::TbMethodCall(tb) => match &tb.method {
                    TbMethod::ClockNext { .. } => {
                        if !clks.contains(&tb.inst) {
                            clks.push(tb.inst);
                        }
                    }
                    TbMethod::ResetAssert { clock, .. } => {
                        if !rsts.contains(&tb.inst) {
                            rsts.push(tb.inst);
                        }
                        if !clks.contains(clock) {
                            clks.push(*clock);
                        }
                    }
                },
                Statement::If(s) => {
                    Self::scan_tb_methods(&s.true_side, clks, rsts);
                    Self::scan_tb_methods(&s.false_side, clks, rsts);
                }
                Statement::For(s) => Self::scan_tb_methods(&s.body, clks, rsts),
                _ => {}
            }
        }
    }

    pub fn convert(&self, stmts: &[Statement]) -> Vec<TestbenchStatement<B>> {
        let p = self.sim.program();
        let root_instance_id = *p
            .instance_ids
            .get(&crate::ir::InstancePath(Vec::new()))
            .expect("root instance not found");
        let root_module_id = p.instance_module[&root_instance_id];
        let ec = ExprCompiler {
            sim: self.sim,
            root_instance_id,
            root_module_id,
        };
        stmts.iter().filter_map(|s| self.convert_stmt(s, &ec)).collect()
    }

    fn convert_stmt(
        &self,
        stmt: &Statement,
        ec: &ExprCompiler<'_, B>,
    ) -> Option<TestbenchStatement<B>> {
        match stmt {
            Statement::TbMethodCall(tb) => self.convert_tb_method(tb, ec),
            Statement::SystemFunctionCall(sf) => match &sf.kind {
                SystemFunctionKind::Assert(cond, msg) => Some(TestbenchStatement::Assert {
                    expr: ec.compile(&cond.0),
                    message: msg.as_ref().map(|m| format!("{}", m.0)),
                }),
                SystemFunctionKind::Finish => Some(TestbenchStatement::Finish),
                _ => None,
            },
            Statement::If(s) => Some(TestbenchStatement::If {
                expr: ec.compile(&s.cond),
                then_block: s
                    .true_side
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec))
                    .collect(),
                else_block: s
                    .false_side
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec))
                    .collect(),
            }),
            Statement::For(s) => {
                let body: Vec<_> = s
                    .body
                    .iter()
                    .filter_map(|s| self.convert_stmt(s, ec))
                    .collect();
                let lv = self.resolve_loop_var(&s.var_id);
                match &s.range {
                    ForRange::Forward { start, end, step } => Some(TestbenchStatement::For {
                        loop_var: lv,
                        start: *start,
                        end: *end,
                        step: *step,
                        reverse: false,
                        body,
                    }),
                    ForRange::Reverse { start, end, step } => Some(TestbenchStatement::For {
                        loop_var: lv,
                        start: *start,
                        end: *end,
                        step: *step,
                        reverse: true,
                        body,
                    }),
                    ForRange::Stepped {
                        start, end, step, ..
                    } => Some(TestbenchStatement::For {
                        loop_var: lv,
                        start: *start,
                        end: *end,
                        step: *step,
                        reverse: false,
                        body,
                    }),
                }
            }
            Statement::Assign(a) => {
                let compiled = ec.compile(&a.expr);
                a.dst
                    .first()
                    .and_then(|d| ec.resolve_var(&d.id))
                    .map(|dst| TestbenchStatement::Assign {
                        dst,
                        expr: compiled,
                    })
            }
            Statement::FunctionCall(fc) => {
                self.convert_function_call(fc, ec)
            }
            _ => None,
        }
    }

    /// Inline-expand a function call by binding arguments and converting
    /// the function body's statements.
    fn convert_function_call(
        &self,
        fc: &veryl_analyzer::ir::FunctionCall,
        ec: &ExprCompiler<'_, B>,
    ) -> Option<TestbenchStatement<B>> {
        let program = self.sim.program();
        let func = program.tb_functions.get(&fc.id)?;
        let func_body = if let Some(idx) = &fc.index {
            func.get_function(idx)?
        } else {
            func.get_function(&[])?
        };

        // Build a list of statements: argument assignments + body
        let mut stmts: Vec<TestbenchStatement<B>> = Vec::new();

        // Bind input arguments
        for (arg_path, arg_expr) in &fc.inputs {
            if let Some(&arg_var_id) = func_body.arg_map.get(arg_path) {
                let compiled = ec.compile(arg_expr);
                if let Some(sig) = ec.resolve_var(&arg_var_id) {
                    stmts.push(TestbenchStatement::Assign {
                        dst: sig,
                        expr: compiled,
                    });
                }
            }
        }

        // Inline body statements
        for stmt in &func_body.statements {
            if let Some(ts) = self.convert_stmt(stmt, ec) {
                stmts.push(ts);
            }
        }

        if stmts.len() == 1 {
            Some(stmts.into_iter().next().unwrap())
        } else {
            // Wrap multiple statements into an If(true) block as a sequence container
            // (there's no "Block" variant in TestbenchStatement)
            // Actually, we can return None and use a different approach:
            // flatten into the parent's statement list.
            // For now, wrap in an always-true If:
            Some(TestbenchStatement::If {
                expr: CompiledExpr {
                    ops: vec![TbOpcode::ConstU64(1)],
                },
                then_block: stmts,
                else_block: Vec::new(),
            })
        }
    }

    fn convert_tb_method(&self, tb: &TbMethodCall, ec: &ExprCompiler<'_, B>) -> Option<TestbenchStatement<B>> {
        match &tb.method {
            TbMethod::ClockNext { count, .. } => {
                let ev = self.event_map.get(&tb.inst).copied()?;
                let clock_count = match count {
                    Some(expr) => {
                        if let Some(n) = try_eval_const(expr) {
                            ClockCount::Static(n)
                        } else {
                            ClockCount::Dynamic(ec.compile(expr))
                        }
                    }
                    None => ClockCount::Static(1),
                };
                Some(TestbenchStatement::ClockNext {
                    clock_event: ev,
                    count: clock_count,
                })
            }
            TbMethod::ResetAssert { clock, duration } => {
                let reset_signal = self.signal_map.get(&tb.inst).copied()?;
                let clock_event = self.event_map.get(clock).copied()?;
                let dur = duration
                    .as_ref()
                    .and_then(|e| try_eval_const(e))
                    .unwrap_or(self.default_reset_duration);
                // Determine reset polarity from the variable's DomainKind
                let (assert_value, deassert_value) = self.resolve_reset_polarity(&tb.inst);
                Some(TestbenchStatement::ResetAssert {
                    reset_signal,
                    clock_event,
                    duration: dur,
                    assert_value,
                    deassert_value,
                })
            }
        }
    }

    /// Determine reset assert/deassert values from the variable's PortTypeKind.
    /// PortTypeKind covers all four reset types (async/sync × high/low),
    /// unlike DomainKind which maps sync resets to Other.
    fn resolve_reset_polarity(&self, inst: &StrId) -> (u8, u8) {
        let name = veryl_parser::resource_table::get_str_value(*inst).unwrap_or_default();
        let program = self.sim.program();
        if let Ok(addr) = program.get_addr(&[], &[&name]) {
            if let Some(info) = program.get_variable_info(&addr) {
                return match info.type_kind {
                    crate::ir::PortTypeKind::ResetAsyncHigh
                    | crate::ir::PortTypeKind::ResetSyncHigh => (1, 0),
                    crate::ir::PortTypeKind::ResetAsyncLow
                    | crate::ir::PortTypeKind::ResetSyncLow => (0, 1),
                    _ => (0, 1),
                };
            }
        }
        (0, 1)
    }

    fn resolve_loop_var(&self, var_id: &VarId) -> Option<(SignalRef, usize)> {
        let p = self.sim.program();
        let rid = p.instance_ids.get(&crate::ir::InstancePath(Vec::new()))?;
        let mid = p.instance_module.get(rid)?;
        let vars = p.module_variables.get(mid)?;
        let info = vars.get(var_id)?;
        let addr = AbsoluteAddr {
            instance_id: *rid,
            var_id: *var_id,
        };
        Some((self.sim.backend_ref().resolve_signal(&addr), info.width))
    }
}

fn try_eval_const(expr: &Expression) -> Option<u64> {
    match expr {
        Expression::Term(f) => match f.as_ref() {
            Factor::Value(c) => c.get_value().ok().map(|v| v.payload_u64()),
            Factor::Variable(_, _, _, c) => c.get_value().ok().map(|v| v.payload_u64()),
            _ => None,
        },
        _ => None,
    }
}

// ── Executor ───────────────────────────────────────────────────────────

enum ExecResult {
    Continue,
    Finished,
    Fail(String),
}

impl ExecResult {
    fn should_stop(&self) -> bool {
        !matches!(self, ExecResult::Continue)
    }
}

impl From<ExecResult> for TestResult {
    fn from(r: ExecResult) -> Self {
        match r {
            ExecResult::Continue | ExecResult::Finished => TestResult::Pass,
            ExecResult::Fail(m) => TestResult::Fail(m),
        }
    }
}

pub fn run_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmts: &[TestbenchStatement<B>],
) -> TestResult {
    exec(sim, stmts).into()
}

fn exec<B: SimBackend>(sim: &mut Simulator<B>, stmts: &[TestbenchStatement<B>]) -> ExecResult {
    for stmt in stmts {
        let r = exec_one(sim, stmt);
        if r.should_stop() {
            return r;
        }
    }
    ExecResult::Continue
}

fn exec_one<B: SimBackend>(sim: &mut Simulator<B>, stmt: &TestbenchStatement<B>) -> ExecResult {
    match stmt {
        TestbenchStatement::ClockNext { clock_event, count } => {
            let n = match count {
                ClockCount::Static(n) => *n,
                ClockCount::Dynamic(expr) => {
                    if let Err(e) = sim.eval_comb() {
                        return ExecResult::Fail(format!("eval_comb: {e}"));
                    }
                    let (ptr, _) = sim.memory_as_mut_ptr();
                    expr.eval_u64(ptr)
                }
            };
            for _ in 0..n {
                if let Err(e) = sim.tick(*clock_event) {
                    return ExecResult::Fail(format!("tick: {e}"));
                }
            }
            ExecResult::Continue
        }
        TestbenchStatement::ResetAssert {
            reset_signal,
            clock_event,
            duration,
            assert_value,
            deassert_value,
        } => {
            sim.set(*reset_signal, *assert_value);
            for _ in 0..*duration {
                if let Err(e) = sim.tick(*clock_event) {
                    return ExecResult::Fail(format!("reset: {e}"));
                }
            }
            sim.set(*reset_signal, *deassert_value);
            ExecResult::Continue
        }
        TestbenchStatement::Assert { expr, message } => {
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            if !expr.eval_bool(ptr) {
                ExecResult::Fail(message.as_deref().unwrap_or("assertion failed").to_string())
            } else {
                ExecResult::Continue
            }
        }
        TestbenchStatement::If {
            expr,
            then_block,
            else_block,
        } => {
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            if expr.eval_bool(ptr) {
                exec(sim, then_block)
            } else {
                exec(sim, else_block)
            }
        }
        TestbenchStatement::For {
            loop_var,
            start,
            end,
            step,
            reverse,
            body,
        } => {
            if *reverse {
                let mut i = *end;
                while i.checked_sub(*step).is_some_and(|next| next >= *start) {
                    i -= step;
                    if let Some((sig, _)) = loop_var {
                        sim.set(*sig, i as u64);
                    }
                    let r = exec(sim, body);
                    if r.should_stop() {
                        return r;
                    }
                }
            } else {
                let mut i = *start;
                while i < *end {
                    if let Some((sig, _)) = loop_var {
                        sim.set(*sig, i as u64);
                    }
                    let r = exec(sim, body);
                    if r.should_stop() {
                        return r;
                    }
                    i += step;
                }
            }
            ExecResult::Continue
        }
        TestbenchStatement::Assign { dst, expr } => {
            if let Err(e) = sim.eval_comb() {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let (ptr, _) = sim.memory_as_mut_ptr();
            let val = expr.eval_value(ptr);
            match val {
                TbValue::U64(v) => sim.set(*dst, v),
                TbValue::Wide(v) => sim.set_wide(*dst, v),
            }
            ExecResult::Continue
        }
        TestbenchStatement::Finish => ExecResult::Finished,
    }
}
