use num_bigint::{BigInt, BigUint, Sign};
use num_traits::ToPrimitive as _;

use crate::{ExprBytecode, ExprOpcode as TbOpcode, TestbenchOperator as Op};

// ── Bytecode VM ────────────────────────────────────────────────────────

/// A compiled expression: flat bytecode evaluated on a stack VM.
#[derive(Debug)]
pub struct CompiledExpr {
    bytecode: ExprBytecode,
}

/// Stack value: either a native u64 or a heap-allocated BigUint.
#[derive(Clone, Debug)]
pub enum TestbenchValue {
    U64(u64),
    Wide(BigUint),
}

impl TestbenchValue {
    #[inline]
    pub fn to_u64(&self) -> u64 {
        match self {
            TestbenchValue::U64(v) => *v,
            TestbenchValue::Wide(v) => {
                let digits = v.to_u64_digits();
                digits.first().copied().unwrap_or(0)
            }
        }
    }

    #[inline]
    pub fn is_zero(&self) -> bool {
        match self {
            TestbenchValue::U64(v) => *v == 0,
            TestbenchValue::Wide(v) => *v == BigUint::ZERO,
        }
    }

    #[inline]
    pub fn to_biguint(&self) -> BigUint {
        match self {
            TestbenchValue::U64(v) => BigUint::from(*v),
            TestbenchValue::Wide(v) => v.clone(),
        }
    }
}

impl CompiledExpr {
    pub fn new(bytecode: ExprBytecode) -> Self {
        Self { bytecode }
    }

    /// Evaluate against raw simulator memory, returning the result as u64.
    /// For wide results, returns the low 64 bits.
    pub fn eval_u64(&self, memory: *mut u8) -> u64 {
        self.eval(memory).to_u64()
    }

    /// Evaluate and return the full `TestbenchValue` (preserves wide results).
    pub fn eval_value(&self, memory: *mut u8) -> TestbenchValue {
        self.eval(memory)
    }

    pub fn eval_bool(&self, memory: *mut u8) -> bool {
        !self.eval(memory).is_zero()
    }

    /// Core evaluation loop.  Uses `TestbenchValue` to handle both u64 and wide
    /// signals on a single stack.  The common case (all ≤64-bit operands)
    /// stays in the `TestbenchValue::U64` variant and never allocates.
    fn eval(&self, memory: *mut u8) -> TestbenchValue {
        let mut stack: Vec<TestbenchValue> = Vec::with_capacity(16);
        let mut pc: usize = 0;
        let ops = self.bytecode.ops();

        while pc < ops.len() {
            self.exec_at(ops, &mut pc, &mut stack, memory);
        }
        stack.pop().unwrap_or_else(|| {
            debug_assert!(false, "testbench bytecode: stack empty after evaluation");
            TestbenchValue::U64(0)
        })
    }

    /// Execute the opcode at `pc` and advance `pc` past it.
    /// Handles all opcodes including `Ternary` (with recursive sub-block
    /// evaluation), so there is no separate `step()` function.
    fn exec_at(
        &self,
        ops: &[TbOpcode],
        pc: &mut usize,
        stack: &mut Vec<TestbenchValue>,
        memory: *mut u8,
    ) {
        match &ops[*pc] {
            TbOpcode::ConstU64(v) => {
                stack.push(TestbenchValue::U64(*v));
                *pc += 1;
            }
            TbOpcode::ConstWide(v) => {
                stack.push(TestbenchValue::Wide(v.clone()));
                *pc += 1;
            }
            TbOpcode::LoadU64 {
                location,
                byte_size,
                mask,
            } => {
                // SAFETY: caller guarantees `memory` is valid simulator memory
                let val = unsafe { read_le_u64(memory.add(*location), *byte_size) } & mask;
                stack.push(TestbenchValue::U64(val));
                *pc += 1;
            }
            TbOpcode::LoadWide {
                location,
                byte_size,
                width,
            } => {
                let val = unsafe { read_le_wide(memory.add(*location), *byte_size, *width) };
                stack.push(TestbenchValue::Wide(val));
                *pc += 1;
            }
            TbOpcode::BinOp(op) => {
                let r = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: BinOp rhs underflow");
                    TestbenchValue::U64(0)
                });
                let l = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: BinOp lhs underflow");
                    TestbenchValue::U64(0)
                });
                stack.push(eval_binop(l, *op, r));
                *pc += 1;
            }
            TbOpcode::TypedBinOp {
                op,
                lhs_width,
                rhs_width,
                result_width,
                lhs_signed,
                rhs_signed,
            } => {
                let r = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: TypedBinOp rhs underflow");
                    TestbenchValue::U64(0)
                });
                let l = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: TypedBinOp lhs underflow");
                    TestbenchValue::U64(0)
                });
                stack.push(eval_typed_binop(
                    l,
                    *op,
                    r,
                    *lhs_width,
                    *rhs_width,
                    *result_width,
                    *lhs_signed,
                    *rhs_signed,
                ));
                *pc += 1;
            }
            TbOpcode::TypedUnary {
                op,
                operand_width,
                result_width,
            } => {
                if let Some(top) = stack.last_mut() {
                    *top = eval_typed_unop(*op, top, *operand_width, *result_width);
                } else {
                    debug_assert!(false, "testbench bytecode: TypedUnary underflow");
                }
                *pc += 1;
            }
            TbOpcode::Resize {
                source_width,
                target_width,
                signed,
            } => {
                if let Some(top) = stack.last_mut() {
                    *top = resize_tb_value(top, *source_width, *target_width, *signed);
                } else {
                    debug_assert!(false, "testbench bytecode: Resize underflow");
                }
                *pc += 1;
            }
            TbOpcode::ConcatPart {
                part_width,
                result_width,
            } => {
                let part = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: ConcatPart value underflow");
                    TestbenchValue::U64(0)
                });
                let accumulator = stack.pop().unwrap_or_else(|| {
                    debug_assert!(
                        false,
                        "testbench bytecode: ConcatPart accumulator underflow"
                    );
                    TestbenchValue::U64(0)
                });
                if let (TestbenchValue::U64(accumulator), TestbenchValue::U64(part)) =
                    (&accumulator, &part)
                    && *result_width <= 64
                {
                    let shifted = if *part_width >= 64 {
                        0
                    } else {
                        accumulator << part_width
                    };
                    stack.push(TestbenchValue::U64(
                        shifted | (part & width_mask_u64(*part_width)),
                    ));
                } else {
                    let value = (accumulator.to_biguint() << part_width)
                        | normalized_bits(&part, *part_width);
                    stack.push(tb_value_from_bits(value, *result_width));
                }
                *pc += 1;
            }
            TbOpcode::Ternary { then_len, else_len } => {
                let cond = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: Ternary cond underflow");
                    TestbenchValue::U64(0)
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
                location,
                stride_bytes,
                element_byte_size,
                element_width,
            } => {
                let idx = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: LoadIndexed underflow");
                    TestbenchValue::U64(0)
                });
                let i = idx.to_u64() as usize;
                let offset = location + i * stride_bytes;
                if *element_byte_size <= 8 {
                    let mask = if *element_width >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << element_width) - 1
                    };
                    let val = unsafe { read_le_u64(memory.add(offset), *element_byte_size) } & mask;
                    stack.push(TestbenchValue::U64(val));
                } else {
                    let val = unsafe {
                        read_le_wide(memory.add(offset), *element_byte_size, *element_width)
                    };
                    stack.push(TestbenchValue::Wide(val));
                }
                *pc += 1;
            }
            TbOpcode::LoadBitSelect {
                location,
                base_byte_size,
                select_width,
            } => {
                let bit_idx = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: LoadBitSelect underflow");
                    TestbenchValue::U64(0)
                });
                let shift = bit_idx.to_u64() as usize;
                if *base_byte_size <= 8 && *select_width <= 64 {
                    let full_val = unsafe { read_le_u64(memory.add(*location), *base_byte_size) };
                    let mask = if *select_width == 64 {
                        u64::MAX
                    } else {
                        (1u64 << select_width) - 1
                    };
                    stack.push(TestbenchValue::U64((full_val >> shift) & mask));
                } else {
                    let full_width = base_byte_size.saturating_mul(8);
                    let full_val =
                        unsafe { read_le_wide(memory.add(*location), *base_byte_size, full_width) };
                    let val = (full_val >> shift) & width_mask(*select_width);
                    stack.push(tb_value_from_bits(val, *select_width));
                }
                *pc += 1;
            }
            TbOpcode::StoreU64 {
                location,
                byte_size,
            } => {
                let val = stack.pop().unwrap_or_else(|| {
                    debug_assert!(false, "testbench bytecode: StoreU64 underflow");
                    TestbenchValue::U64(0)
                });
                let v = val.to_u64();
                let bytes = v.to_le_bytes();
                let n = (*byte_size).min(8);
                unsafe {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), memory.add(*location), n);
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
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), byte_size.min(8));
    }
    u64::from_le_bytes(buf)
}

/// # Safety
/// `ptr` must be valid for `byte_size` bytes of read access.
unsafe fn read_le_wide(ptr: *const u8, byte_size: usize, width: usize) -> BigUint {
    let mut buf = vec![0u8; byte_size];
    unsafe {
        std::ptr::copy_nonoverlapping(ptr, buf.as_mut_ptr(), byte_size);
    }
    let mut val = BigUint::from_bytes_le(&buf);
    let extra_bits = byte_size * 8 - width;
    if extra_bits > 0 {
        val &= (BigUint::from(1u32) << width) - BigUint::from(1u32);
    }
    val
}

// ── Typed evaluation ───────────────────────────────────────────────────

/// Binary operation on `TestbenchValue`.  When both operands are `U64` the fast
/// path runs entirely in registers; otherwise we promote to `BigUint`.
#[inline]
fn eval_binop(l: TestbenchValue, op: Op, r: TestbenchValue) -> TestbenchValue {
    match (&l, &r) {
        (TestbenchValue::U64(lv), TestbenchValue::U64(rv)) => {
            TestbenchValue::U64(eval_binop_u64(*lv, op, *rv))
        }
        _ => {
            let lv = l.to_biguint();
            let rv = r.to_biguint();
            // Comparison / logic ops always return u64
            match op {
                Op::Eq
                | Op::Ne
                | Op::Less
                | Op::LessEq
                | Op::Greater
                | Op::GreaterEq
                | Op::LogicAnd
                | Op::LogicOr => TestbenchValue::U64(eval_binop_wide_cmp(&lv, op, &rv)),
                _ => TestbenchValue::Wide(eval_binop_wide(lv, op, rv)),
            }
        }
    }
}

fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::ZERO
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    }
}

#[inline]
fn width_mask_u64(width: usize) -> u64 {
    match width {
        0 => 0,
        1..=63 => (1u64 << width) - 1,
        _ => u64::MAX,
    }
}

#[inline]
fn signed_i128(value: u64, width: usize) -> i128 {
    let value = value & width_mask_u64(width);
    if width == 0 || width >= 64 {
        (value as i64) as i128
    } else if value & (1u64 << (width - 1)) == 0 {
        value as i128
    } else {
        value as i128 - (1i128 << width)
    }
}

fn normalized_bits(value: &TestbenchValue, width: usize) -> BigUint {
    value.to_biguint() & width_mask(width)
}

fn tb_value_from_bits(value: BigUint, width: usize) -> TestbenchValue {
    let value = value & width_mask(width);
    if width <= 64 {
        TestbenchValue::U64(value.to_u64().unwrap_or(0))
    } else {
        TestbenchValue::Wide(value)
    }
}

fn signed_bigint(value: &TestbenchValue, width: usize) -> BigInt {
    let raw = normalized_bits(value, width);
    if width == 0 || !raw.bit((width - 1) as u64) {
        BigInt::from(raw)
    } else {
        BigInt::from(raw) - (BigInt::from(1u8) << width)
    }
}

fn signed_bits(value: BigInt, width: usize) -> BigUint {
    if width == 0 {
        return BigUint::ZERO;
    }
    let modulus = BigUint::from(1u8) << width;
    match value.sign() {
        Sign::Minus => {
            let magnitude = (-value).to_biguint().unwrap_or_default() % &modulus;
            if magnitude == BigUint::ZERO {
                BigUint::ZERO
            } else {
                modulus - magnitude
            }
        }
        _ => value.to_biguint().unwrap_or_default() % modulus,
    }
}

fn resize_tb_value(
    value: &TestbenchValue,
    source_width: usize,
    target_width: usize,
    signed: bool,
) -> TestbenchValue {
    if target_width == 0 {
        return TestbenchValue::U64(0);
    }
    if source_width == 0 {
        let fill = if value.to_u64() & 1 == 0 {
            BigUint::ZERO
        } else {
            width_mask(target_width)
        };
        return tb_value_from_bits(fill, target_width);
    }

    if let TestbenchValue::U64(value) = value
        && source_width <= 64
        && target_width <= 64
    {
        let mut value = value & width_mask_u64(source_width);
        if target_width > source_width && signed && value & (1u64 << (source_width - 1)) != 0 {
            value |= width_mask_u64(target_width) ^ width_mask_u64(source_width);
        }
        return TestbenchValue::U64(value & width_mask_u64(target_width));
    }

    let mut value = normalized_bits(value, source_width);
    if target_width > source_width && signed && value.bit((source_width - 1) as u64) {
        value |= width_mask(target_width) ^ width_mask(source_width);
    }
    tb_value_from_bits(value, target_width)
}

fn eval_typed_binop_u64(
    l: u64,
    op: Op,
    r: u64,
    lhs_width: usize,
    rhs_width: usize,
    result_width: usize,
    lhs_signed: bool,
    rhs_signed: bool,
) -> u64 {
    let l = l & width_mask_u64(lhs_width);
    let r = r & width_mask_u64(rhs_width);
    let result_mask = width_mask_u64(result_width);
    let signed = lhs_signed && rhs_signed;
    let bool_value = |value: bool| u64::from(value);

    match op {
        Op::Eq | Op::EqWildcard => bool_value(l == r),
        Op::Ne | Op::NeWildcard => bool_value(l != r),
        Op::Less if signed => bool_value(signed_i128(l, lhs_width) < signed_i128(r, rhs_width)),
        Op::Less => bool_value(l < r),
        Op::LessEq if signed => bool_value(signed_i128(l, lhs_width) <= signed_i128(r, rhs_width)),
        Op::LessEq => bool_value(l <= r),
        Op::Greater if signed => bool_value(signed_i128(l, lhs_width) > signed_i128(r, rhs_width)),
        Op::Greater => bool_value(l > r),
        Op::GreaterEq if signed => {
            bool_value(signed_i128(l, lhs_width) >= signed_i128(r, rhs_width))
        }
        Op::GreaterEq => bool_value(l >= r),
        Op::LogicAnd => bool_value(l != 0 && r != 0),
        Op::LogicOr => bool_value(l != 0 || r != 0),
        Op::Add => l.wrapping_add(r) & result_mask,
        Op::Sub => l.wrapping_sub(r) & result_mask,
        Op::Mul => l.wrapping_mul(r) & result_mask,
        Op::Div if signed => {
            let divisor = signed_i128(r, rhs_width);
            if divisor == 0 {
                0
            } else {
                (signed_i128(l, lhs_width) / divisor) as u64 & result_mask
            }
        }
        Op::Div => l.checked_div(r).unwrap_or(0) & result_mask,
        Op::Rem if signed => {
            let divisor = signed_i128(r, rhs_width);
            if divisor == 0 {
                0
            } else {
                (signed_i128(l, lhs_width) % divisor) as u64 & result_mask
            }
        }
        Op::Rem => l.checked_rem(r).unwrap_or(0) & result_mask,
        Op::Pow => {
            let mut exponent = r;
            let mut base = l & result_mask;
            let mut value = 1u64 & result_mask;
            while exponent != 0 {
                if exponent & 1 != 0 {
                    value = ((value as u128 * base as u128) as u64) & result_mask;
                }
                exponent >>= 1;
                if exponent != 0 {
                    base = ((base as u128 * base as u128) as u64) & result_mask;
                }
            }
            value
        }
        Op::BitAnd => (l & r) & result_mask,
        Op::BitOr => (l | r) & result_mask,
        Op::BitXor => (l ^ r) & result_mask,
        Op::BitXnor => (!(l ^ r)) & result_mask,
        Op::BitNand => (!(l & r)) & result_mask,
        Op::BitNor => (!(l | r)) & result_mask,
        Op::LogicShiftL | Op::ArithShiftL => {
            if r >= result_width as u64 {
                0
            } else {
                l.wrapping_shl(r as u32) & result_mask
            }
        }
        Op::LogicShiftR => {
            if r >= result_width as u64 {
                0
            } else {
                (l >> r) & result_mask
            }
        }
        Op::ArithShiftR if lhs_signed => {
            let value = signed_i128(l, lhs_width);
            if r >= result_width as u64 {
                if value < 0 { result_mask } else { 0 }
            } else {
                ((value >> r) as u64) & result_mask
            }
        }
        Op::ArithShiftR => {
            if r >= result_width as u64 {
                0
            } else {
                (l >> r) & result_mask
            }
        }
        _ => unreachable!("operator is not a source-language binary op: {op:?}"),
    }
}

fn eval_typed_binop(
    l: TestbenchValue,
    op: Op,
    r: TestbenchValue,
    lhs_width: usize,
    rhs_width: usize,
    result_width: usize,
    lhs_signed: bool,
    rhs_signed: bool,
) -> TestbenchValue {
    if let (TestbenchValue::U64(l), TestbenchValue::U64(r)) = (&l, &r)
        && lhs_width <= 64
        && rhs_width <= 64
        && result_width <= 64
    {
        return TestbenchValue::U64(eval_typed_binop_u64(
            *l,
            op,
            *r,
            lhs_width,
            rhs_width,
            result_width,
            lhs_signed,
            rhs_signed,
        ));
    }
    let lb = normalized_bits(&l, lhs_width);
    let rb = normalized_bits(&r, rhs_width);
    let signed = lhs_signed && rhs_signed;

    let comparison = |value: bool| TestbenchValue::U64(u64::from(value));
    match op {
        Op::Eq | Op::EqWildcard => comparison(lb == rb),
        Op::Ne | Op::NeWildcard => comparison(lb != rb),
        Op::Less if signed => {
            comparison(signed_bigint(&l, lhs_width) < signed_bigint(&r, rhs_width))
        }
        Op::Less => comparison(lb < rb),
        Op::LessEq if signed => {
            comparison(signed_bigint(&l, lhs_width) <= signed_bigint(&r, rhs_width))
        }
        Op::LessEq => comparison(lb <= rb),
        Op::Greater if signed => {
            comparison(signed_bigint(&l, lhs_width) > signed_bigint(&r, rhs_width))
        }
        Op::Greater => comparison(lb > rb),
        Op::GreaterEq if signed => {
            comparison(signed_bigint(&l, lhs_width) >= signed_bigint(&r, rhs_width))
        }
        Op::GreaterEq => comparison(lb >= rb),
        Op::LogicAnd => comparison(lb != BigUint::ZERO && rb != BigUint::ZERO),
        Op::LogicOr => comparison(lb != BigUint::ZERO || rb != BigUint::ZERO),
        Op::Add => tb_value_from_bits(lb + rb, result_width),
        Op::Sub => tb_value_from_bits(
            signed_bits(BigInt::from(lb) - BigInt::from(rb), result_width),
            result_width,
        ),
        Op::Mul => tb_value_from_bits(lb * rb, result_width),
        Op::Div if signed => {
            let divisor = signed_bigint(&r, rhs_width);
            if divisor == BigInt::from(0u8) {
                TestbenchValue::U64(0)
            } else {
                let quotient = signed_bigint(&l, lhs_width) / divisor;
                tb_value_from_bits(signed_bits(quotient, result_width), result_width)
            }
        }
        Op::Div => {
            if rb == BigUint::ZERO {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb / rb, result_width)
            }
        }
        Op::Rem if signed => {
            let divisor = signed_bigint(&r, rhs_width);
            if divisor == BigInt::from(0u8) {
                TestbenchValue::U64(0)
            } else {
                let remainder = signed_bigint(&l, lhs_width) % divisor;
                tb_value_from_bits(signed_bits(remainder, result_width), result_width)
            }
        }
        Op::Rem => {
            if rb == BigUint::ZERO {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb % rb, result_width)
            }
        }
        Op::Pow => {
            if result_width == 0 {
                TestbenchValue::U64(0)
            } else {
                let modulus = BigUint::from(1u8) << result_width;
                tb_value_from_bits(lb.modpow(&rb, &modulus), result_width)
            }
        }
        Op::BitAnd => tb_value_from_bits(lb & rb, result_width),
        Op::BitOr => tb_value_from_bits(lb | rb, result_width),
        Op::BitXor => tb_value_from_bits(lb ^ rb, result_width),
        Op::BitXnor => tb_value_from_bits((lb ^ rb) ^ width_mask(result_width), result_width),
        Op::LogicShiftL | Op::ArithShiftL => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            if shift >= result_width {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb << shift, result_width)
            }
        }
        Op::LogicShiftR => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            if shift >= result_width {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb >> shift, result_width)
            }
        }
        Op::ArithShiftR if lhs_signed => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            let value = signed_bigint(&l, lhs_width);
            let shifted = if shift >= result_width {
                if value.sign() == Sign::Minus {
                    BigInt::from(-1)
                } else {
                    BigInt::from(0)
                }
            } else {
                value >> shift
            };
            tb_value_from_bits(signed_bits(shifted, result_width), result_width)
        }
        Op::ArithShiftR => {
            let shift = rb.to_usize().unwrap_or(usize::MAX);
            if shift >= result_width {
                TestbenchValue::U64(0)
            } else {
                tb_value_from_bits(lb >> shift, result_width)
            }
        }
        Op::BitNand => tb_value_from_bits((lb & rb) ^ width_mask(result_width), result_width),
        Op::BitNor => tb_value_from_bits((lb | rb) ^ width_mask(result_width), result_width),
        _ => unreachable!("operator is not a source-language binary op: {op:?}"),
    }
}

fn eval_typed_unop(
    op: Op,
    value: &TestbenchValue,
    operand_width: usize,
    result_width: usize,
) -> TestbenchValue {
    if let TestbenchValue::U64(value) = value
        && operand_width <= 64
        && result_width <= 64
    {
        let bits = value & width_mask_u64(operand_width);
        let value = match op {
            Op::LogicNot => u64::from(bits == 0),
            Op::BitAnd => u64::from(bits == width_mask_u64(operand_width)),
            Op::BitNand => u64::from(bits != width_mask_u64(operand_width)),
            Op::BitOr => u64::from(bits != 0),
            Op::BitNor => u64::from(bits == 0),
            Op::BitXor => u64::from(!bits.count_ones().is_multiple_of(2)),
            Op::BitXnor => u64::from(bits.count_ones().is_multiple_of(2)),
            Op::Add => bits & width_mask_u64(result_width),
            Op::Sub => bits.wrapping_neg() & width_mask_u64(result_width),
            Op::BitNot => !bits & width_mask_u64(result_width),
            _ => unreachable!("operator is not a source-language unary op: {op:?}"),
        };
        return TestbenchValue::U64(value);
    }

    let bits = normalized_bits(value, operand_width);
    let reduced = match op {
        Op::LogicNot => Some(bits == BigUint::ZERO),
        Op::BitAnd => Some(bits == width_mask(operand_width)),
        Op::BitNand => Some(bits != width_mask(operand_width)),
        Op::BitOr => Some(bits != BigUint::ZERO),
        Op::BitNor => Some(bits == BigUint::ZERO),
        Op::BitXor | Op::BitXnor => {
            let odd = bits.iter_u64_digits().map(u64::count_ones).sum::<u32>() % 2 != 0;
            Some(if matches!(op, Op::BitXor) { odd } else { !odd })
        }
        _ => None,
    };
    if let Some(value) = reduced {
        return TestbenchValue::U64(u64::from(value));
    }

    match op {
        Op::Add => tb_value_from_bits(bits, result_width),
        Op::Sub => tb_value_from_bits(signed_bits(-BigInt::from(bits), result_width), result_width),
        Op::BitNot => tb_value_from_bits(bits ^ width_mask(operand_width), result_width),
        _ => unreachable!("operator is not a source-language unary op: {op:?}"),
    }
}

#[inline]
fn eval_binop_u64(l: u64, op: Op, r: u64) -> u64 {
    match op {
        Op::Add => l.wrapping_add(r),
        Op::Sub => l.wrapping_sub(r),
        Op::Mul => l.wrapping_mul(r),
        Op::Div => l.checked_div(r).unwrap_or(0),
        Op::Rem => l.checked_rem(r).unwrap_or(0),
        Op::BitAnd => l & r,
        Op::BitOr => l | r,
        Op::BitXor => l ^ r,
        Op::LogicShiftL => {
            if r >= 64 {
                0
            } else {
                l << r
            }
        }
        Op::LogicShiftR => {
            if r >= 64 {
                0
            } else {
                l >> r
            }
        }
        Op::ArithShiftL => {
            if r >= 64 {
                0
            } else {
                l << r
            }
        }
        Op::ArithShiftR => {
            if r >= 64 {
                ((l as i64) >> 63) as u64
            } else {
                ((l as i64) >> r) as u64
            }
        }
        Op::Eq => (l == r) as u64,
        Op::Ne => (l != r) as u64,
        Op::Less => (l < r) as u64,
        Op::LessEq => (l <= r) as u64,
        Op::Greater => (l > r) as u64,
        Op::GreaterEq => (l >= r) as u64,
        Op::LogicAnd => ((l != 0) && (r != 0)) as u64,
        Op::LogicOr => ((l != 0) || (r != 0)) as u64,
        _ => unreachable!("operator is not testbench bytecode plumbing: {op:?}"),
    }
}

fn eval_binop_wide(l: BigUint, op: Op, r: BigUint) -> BigUint {
    match op {
        Op::Add => l + r,
        Op::Sub => {
            if l >= r {
                l - r
            } else {
                BigUint::ZERO
            }
        }
        Op::Mul => l * r,
        Op::Div => {
            if r == BigUint::ZERO {
                BigUint::ZERO
            } else {
                l / r
            }
        }
        Op::Rem => {
            if r == BigUint::ZERO {
                BigUint::ZERO
            } else {
                l % r
            }
        }
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
        _ => unreachable!("operator is not wide testbench bytecode plumbing: {op:?}"),
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
        _ => unreachable!("operator is not testbench comparison plumbing: {op:?}"),
    }
}
