//! Lower testbench expressions into the same SIR operations as HDL processes.
//! Widths remain explicit through native instruction selection, including
//! values wider than a host register. Strings use the existing heap ABI.
use crate::{ExprBytecode, ExprOpcode, StateLocation, TestbenchOperator as Op};
use celox_design::{BinaryOp, UnaryOp};
use celox_sir::{
    RegisterId, SIRBuilder, SIRInstruction as Inst, SIROffset, SIRTerminator, SIRValue,
};
use num_bigint::BigUint;

#[derive(Clone, Copy, Debug)]
pub struct Value {
    pub register: RegisterId,
    pub string: bool,
}

pub struct Expressions<'a, A> {
    pub builder: &'a mut SIRBuilder<A>,
}
impl<A: Clone> Expressions<'_, A> {
    fn bits(&mut self, width: usize, value: impl Into<BigUint>) -> Value {
        let register = self.builder.alloc_bit(width.max(1), false);
        self.builder.emit(Inst::Imm(register, SIRValue::new(value)));
        Value {
            register,
            string: false,
        }
    }
    fn unary(&mut self, op: UnaryOp, value: Value, width: usize, signed: bool) -> Value {
        let register = self.builder.alloc_bit(width.max(1), signed);
        if op == UnaryOp::Ident && width < self.builder.register(&value.register).width() {
            self.builder
                .emit(Inst::Slice(register, value.register, 0, width.max(1)));
        } else {
            self.builder.emit(Inst::Unary(register, op, value.register));
        }
        Value {
            register,
            string: false,
        }
    }
    fn binary(&mut self, op: BinaryOp, lhs: Value, rhs: Value, width: usize) -> Value {
        let register = self.builder.alloc_bit(width.max(1), false);
        self.builder
            .emit(Inst::Binary(register, lhs.register, op, rhs.register));
        Value {
            register,
            string: false,
        }
    }
    fn resize(&mut self, value: Value, source: usize, target: usize, signed: bool) -> Value {
        if target == 0 {
            return self.bits(1, 0u8);
        }
        if source == 0 {
            let value = self.unary(UnaryOp::Ident, value, 1, false);
            let value = self.unary(UnaryOp::Ident, value, target, false);
            return self.unary(UnaryOp::Minus, value, target, false);
        }
        let value = self.unary(UnaryOp::Ident, value, source, signed);
        let value = self.unary(UnaryOp::Ident, value, target, false);
        if signed && target > source {
            let shift = self.bits(64, (target - source) as u64);
            let value = self.binary(BinaryOp::Shl, value, shift, target);
            self.binary(BinaryOp::Sar, value, shift, target)
        } else {
            value
        }
    }
    fn heap(&mut self, operation: u64, args: &[Value]) -> Value {
        let op = self.bits(64, operation);
        let zero = self.bits(64, 0u8);
        let mut inputs = vec![op.register];
        inputs.extend(args.iter().map(|arg| arg.register));
        inputs.resize(4, zero.register);
        let register = self.builder.alloc_bit(64, false);
        self.builder.emit(Inst::ExternCall {
            dst: Some(register),
            func: celox_sir::extern_abi::HEAP_HELPER_ID,
            args: inputs,
        });
        Value {
            register,
            string: true,
        }
    }
    fn pop(stack: &mut Vec<Value>) -> Result<Value, String> {
        stack
            .pop()
            .ok_or_else(|| "testbench expression stack underflow".into())
    }
    pub fn compile(
        &mut self,
        expression: &ExprBytecode<StateLocation<A>>,
    ) -> Result<Value, String> {
        let mut stack = Vec::new();
        self.block(expression.ops(), &mut stack)?;
        if stack.len() != 1 {
            return Err("testbench expression must produce one value".into());
        }
        let value = Self::pop(&mut stack)?;
        if value.string {
            let width = self.builder.register(&value.register).width();
            let mut value = self.resize(value, width, celox_design::HANDLE_WIDTH, false);
            value.string = true;
            Ok(value)
        } else {
            Ok(value)
        }
    }
    fn block(
        &mut self,
        code: &[ExprOpcode<StateLocation<A>>],
        stack: &mut Vec<Value>,
    ) -> Result<(), String> {
        let mut cursor = 0;
        while cursor < code.len() {
            let instruction = &code[cursor];
            cursor += 1;
            let value = match instruction {
                ExprOpcode::ConstU64(value) => self.bits(64, *value),
                ExprOpcode::ConstWide(value) => {
                    self.bits((value.bits() as usize).max(65), value.clone())
                }
                ExprOpcode::LoadU64 {
                    location,
                    byte_size,
                    mask,
                } => {
                    let register = self.builder.alloc_bit((byte_size * 8).max(1), false);
                    self.builder.emit(Inst::Load(
                        register,
                        location.address.clone(),
                        SIROffset::Static(location.byte_offset * 8),
                        byte_size * 8,
                    ));
                    let value = Value {
                        register,
                        string: false,
                    };
                    let mask = self.bits(64, *mask);
                    self.binary(BinaryOp::And, value, mask, 64)
                }
                ExprOpcode::LoadWide {
                    location, width, ..
                } => {
                    let register = self.builder.alloc_bit((*width).max(1), false);
                    self.builder.emit(Inst::Load(
                        register,
                        location.address.clone(),
                        SIROffset::Static(location.byte_offset * 8),
                        *width,
                    ));
                    Value {
                        register,
                        string: false,
                    }
                }
                ExprOpcode::BinOp(op) => {
                    let rhs = Self::pop(stack)?;
                    let lhs = Self::pop(stack)?;
                    let width = self
                        .builder
                        .register(&lhs.register)
                        .width()
                        .max(self.builder.register(&rhs.register).width());
                    self.operation(*op, lhs, rhs, width, width, width, false, false)?
                }
                ExprOpcode::TypedBinOp {
                    op,
                    lhs_width,
                    rhs_width,
                    result_width,
                    lhs_signed,
                    rhs_signed,
                } => {
                    let rhs = Self::pop(stack)?;
                    let lhs = Self::pop(stack)?;
                    self.operation(
                        *op,
                        lhs,
                        rhs,
                        *lhs_width,
                        *rhs_width,
                        *result_width,
                        *lhs_signed,
                        *rhs_signed,
                    )?
                }
                ExprOpcode::TypedUnary {
                    op,
                    operand_width,
                    result_width,
                } => {
                    let operand = Self::pop(stack)?;
                    let operand = self.resize(operand, *operand_width, *operand_width, false);
                    let (op, invert) = match op {
                        Op::Add => (UnaryOp::Ident, false),
                        Op::Sub => (UnaryOp::Minus, false),
                        Op::LogicNot => (UnaryOp::LogicNot, false),
                        Op::BitNot => (UnaryOp::BitNot, false),
                        Op::BitAnd => (UnaryOp::And, false),
                        Op::BitNand => (UnaryOp::And, true),
                        Op::BitOr => (UnaryOp::Or, false),
                        Op::BitNor => (UnaryOp::Or, true),
                        Op::BitXor => (UnaryOp::Xor, false),
                        Op::BitXnor => (UnaryOp::Xor, true),
                        _ => return Err("invalid testbench unary operator".into()),
                    };
                    let value = self.unary(op, operand, *result_width, false);
                    if invert {
                        self.unary(UnaryOp::LogicNot, value, 1, false)
                    } else {
                        value
                    }
                }
                ExprOpcode::FloatBinary { op, precision } => {
                    let rhs = Self::pop(stack)?;
                    let lhs = Self::pop(stack)?;
                    let lhs = self.resize(lhs, precision.width(), precision.width(), false);
                    let rhs = self.resize(rhs, precision.width(), precision.width(), false);
                    let op = BinaryOp::Float {
                        op: *op,
                        precision: *precision,
                    };
                    let width = match op {
                        BinaryOp::Float {
                            op:
                                celox_design::FloatBinaryOp::Eq
                                | celox_design::FloatBinaryOp::Ne
                                | celox_design::FloatBinaryOp::Lt
                                | celox_design::FloatBinaryOp::Le
                                | celox_design::FloatBinaryOp::Gt
                                | celox_design::FloatBinaryOp::Ge,
                            ..
                        } => 1,
                        _ => precision.width(),
                    };
                    self.binary(op, lhs, rhs, width)
                }
                ExprOpcode::FloatUnary { op, precision } => {
                    let value = Self::pop(stack)?;
                    let input_width = match op {
                        celox_design::FloatUnaryOp::FromSigned { width }
                        | celox_design::FloatUnaryOp::FromUnsigned { width } => *width,
                        _ => precision.width(),
                    };
                    let value = self.resize(value, input_width, input_width, false);
                    let op = UnaryOp::Float {
                        op: *op,
                        precision: *precision,
                    };
                    self.unary(op, value, op.result_width(precision.width()), false)
                }
                ExprOpcode::Resize {
                    source_width,
                    target_width,
                    signed,
                } => {
                    let value = Self::pop(stack)?;
                    self.resize(value, *source_width, *target_width, *signed)
                }
                ExprOpcode::ConcatPart {
                    part_width,
                    result_width,
                } => {
                    let rhs = Self::pop(stack)?;
                    let lhs = Self::pop(stack)?;
                    if *part_width >= *result_width {
                        self.resize(rhs, *part_width, *result_width, false)
                    } else {
                        let rhs = self.resize(rhs, *part_width, *part_width, false);
                        let lhs = self.resize(
                            lhs,
                            self.builder.register(&lhs.register).width(),
                            result_width - part_width,
                            false,
                        );
                        let register = self.builder.alloc_bit((*result_width).max(1), false);
                        self.builder
                            .emit(Inst::Concat(register, vec![lhs.register, rhs.register]));
                        Value {
                            register,
                            string: false,
                        }
                    }
                }
                ExprOpcode::Ternary { then_len, else_len } => {
                    let condition = Self::pop(stack)?;
                    let end = cursor
                        .checked_add(*then_len)
                        .and_then(|x| x.checked_add(*else_len))
                        .filter(|x| *x <= code.len())
                        .ok_or("invalid testbench conditional lengths")?;
                    let yes = self.builder.new_block();
                    let no = self.builder.new_block();
                    self.builder.seal_block(SIRTerminator::Branch {
                        cond: condition.register,
                        true_block: (yes, vec![]),
                        false_block: (no, vec![]),
                    });
                    self.builder.switch_to_block(yes);
                    let mut left = stack.clone();
                    self.block(&code[cursor..cursor + then_len], &mut left)?;
                    let yes_end = self.builder.current_block();
                    self.builder.seal_block(SIRTerminator::Return);
                    self.builder.switch_to_block(no);
                    let mut right = stack.clone();
                    self.block(&code[cursor + then_len..end], &mut right)?;
                    if left.len() != right.len() || left.len() != stack.len() + 1 {
                        return Err("testbench conditional stack mismatch".into());
                    }
                    let no_end = self.builder.current_block();
                    self.builder.seal_block(SIRTerminator::Return);
                    let mut params = Vec::new();
                    let mut merged = Vec::new();
                    for (l, r) in left.iter().zip(&right) {
                        if l.string != r.string {
                            return Err("testbench conditional value kind mismatch".into());
                        }
                        let width = self
                            .builder
                            .register(&l.register)
                            .width()
                            .max(self.builder.register(&r.register).width());
                        let register = self.builder.alloc_bit(width, false);
                        params.push(register);
                        merged.push(Value {
                            register,
                            string: l.string,
                        });
                    }
                    let join = self.builder.new_block_with(params);
                    for (block, values) in [(yes_end, left), (no_end, right)] {
                        self.builder.switch_to_block(block);
                        let values = values
                            .into_iter()
                            .zip(&merged)
                            .map(|(value, target)| {
                                self.unary(
                                    UnaryOp::Ident,
                                    value,
                                    self.builder.register(&target.register).width(),
                                    false,
                                )
                                .register
                            })
                            .collect();
                        self.builder.seal_block(SIRTerminator::Jump(join, values));
                    }
                    self.builder.switch_to_block(join);
                    *stack = merged;
                    cursor = end;
                    continue;
                }
                ExprOpcode::LoadIndexed {
                    location,
                    stride_bits,
                    base_bit_offset,
                    element_width,
                } => {
                    let index = Self::pop(stack)?;
                    let stride = self.bits(64, *stride_bits as u64);
                    let offset = self.binary(BinaryOp::Mul, index, stride, 64);
                    let base = self.bits(64, (location.byte_offset * 8 + base_bit_offset) as u64);
                    let offset = self.binary(BinaryOp::Add, offset, base, 64);
                    let register = self.builder.alloc_bit((*element_width).max(1), false);
                    self.builder.emit(Inst::Load(
                        register,
                        location.address.clone(),
                        SIROffset::Dynamic(offset.register),
                        *element_width,
                    ));
                    Value {
                        register,
                        string: false,
                    }
                }
                ExprOpcode::LoadBitSelect {
                    location,
                    base_byte_size,
                    select_width,
                } => {
                    // Select from the value, not from an unchecked dynamic
                    // memory address. A selection may extend past the object.
                    let offset = Self::pop(stack)?;
                    let width = base_byte_size.saturating_mul(8);
                    if width == 0 || *select_width == 0 {
                        self.bits((*select_width).max(1), 0u8)
                    } else {
                        let register = self.builder.alloc_bit(width, false);
                        self.builder.emit(Inst::Load(
                            register,
                            location.address.clone(),
                            SIROffset::Static(location.byte_offset * 8),
                            width,
                        ));
                        let value = Value {
                            register,
                            string: false,
                        };
                        let value = self.binary(BinaryOp::Shr, value, offset, width);
                        self.resize(value, width, *select_width, false)
                    }
                }
                ExprOpcode::StoreU64 {
                    location,
                    byte_size,
                } => {
                    let value = Self::pop(stack)?;
                    self.builder.emit(Inst::Store(
                        location.address.clone(),
                        SIROffset::Static(location.byte_offset * 8),
                        byte_size * 8,
                        value.register,
                        vec![],
                        vec![],
                    ));
                    continue;
                }
                ExprOpcode::ConstString(bytes) => {
                    let mut value = self.heap(celox_heap::ops::NEW, &[]);
                    for chunk in bytes.chunks(8) {
                        let bits = chunk
                            .iter()
                            .fold(0u64, |v, byte| (v << 8) | u64::from(*byte));
                        let bits = self.bits(64, bits);
                        let count = self.bits(64, chunk.len() as u64);
                        value = self.heap(celox_heap::ops::APPEND_BYTES, &[value, bits, count]);
                    }
                    self.heap(celox_heap::ops::FREEZE, &[value])
                }
                ExprOpcode::LoadString { location, elements } => {
                    let offset = Self::pop(stack)?;
                    let shift = self.bits(64, celox_design::HANDLE_WIDTH.trailing_zeros() as u64);
                    let index = self.binary(BinaryOp::Shr, offset, shift, 64);
                    let limit = self.bits(64, *elements as u64);
                    let valid = self.binary(BinaryOp::LtU, index, limit, 1);
                    let present = self.builder.new_block();
                    let absent = self.builder.new_block();
                    let result = self.builder.alloc_bit(celox_design::HANDLE_WIDTH, false);
                    let join = self.builder.new_block_with(vec![result]);
                    self.builder.seal_block(SIRTerminator::Branch {
                        cond: valid.register,
                        true_block: (present, vec![]),
                        false_block: (absent, vec![]),
                    });
                    self.builder.switch_to_block(present);
                    let shift = self.bits(64, celox_design::HANDLE_WIDTH.trailing_zeros() as u64);
                    let offset = self.binary(BinaryOp::Shl, index, shift, 64);
                    let base = self.bits(64, (location.byte_offset * 8) as u64);
                    let offset = self.binary(BinaryOp::Add, offset, base, 64);
                    let register = self.builder.alloc_bit(celox_design::HANDLE_WIDTH, false);
                    self.builder.emit(Inst::Load(
                        register,
                        location.address.clone(),
                        SIROffset::Dynamic(offset.register),
                        celox_design::HANDLE_WIDTH,
                    ));
                    self.builder
                        .seal_block(SIRTerminator::Jump(join, vec![register]));
                    self.builder.switch_to_block(absent);
                    let zero = self.bits(celox_design::HANDLE_WIDTH, 0u8);
                    self.builder
                        .seal_block(SIRTerminator::Jump(join, vec![zero.register]));
                    self.builder.switch_to_block(join);
                    Value {
                        register: result,
                        string: true,
                    }
                }
                ExprOpcode::StoreString { location } => {
                    let value = Self::pop(stack)?;
                    let width = self.builder.register(&value.register).width();
                    let value = self.resize(value, width, celox_design::HANDLE_WIDTH, false);
                    self.builder.emit(Inst::Store(
                        location.address.clone(),
                        SIROffset::Static(location.byte_offset * 8),
                        celox_design::HANDLE_WIDTH,
                        value.register,
                        vec![],
                        vec![],
                    ));
                    continue;
                }
            };
            stack.push(value);
        }
        Ok(())
    }
    fn operation(
        &mut self,
        op: Op,
        mut lhs: Value,
        mut rhs: Value,
        lw: usize,
        rw: usize,
        width: usize,
        ls: bool,
        rs: bool,
    ) -> Result<Value, String> {
        if lhs.string && rhs.string {
            lhs = self.heap(celox_heap::ops::COMPARE, &[lhs, rhs]);
            rhs = self.bits(64, 0u8);
            return self.operation(
                op,
                Value {
                    string: false,
                    ..lhs
                },
                rhs,
                64,
                64,
                1,
                true,
                true,
            );
        }
        lhs = self.resize(lhs, lw, lw, ls);
        rhs = self.resize(rhs, rw, rw, rs);
        let signed = ls && rs;
        if op == Op::Pow {
            let mut result = self.bits(width, 1u8);
            let mut factor = self.resize(lhs, lw, width, false);
            for bit in 0..rw {
                let predicate = self.builder.alloc_bit(1, false);
                self.builder
                    .emit(Inst::Slice(predicate, rhs.register, bit, 1));
                let product = self.binary(BinaryOp::Mul, result, factor, width);
                let register = self.builder.alloc_bit(width.max(1), false);
                self.builder.emit(Inst::Mux(
                    register,
                    predicate,
                    product.register,
                    result.register,
                ));
                result = Value {
                    register,
                    string: false,
                };
                if bit + 1 < rw {
                    factor = self.binary(BinaryOp::Mul, factor, factor, width);
                }
            }
            return Ok(result);
        }
        let (binary, boolean, invert) = match op {
            Op::Add => (BinaryOp::Add, false, false),
            Op::Sub => (BinaryOp::Sub, false, false),
            Op::Mul => (BinaryOp::Mul, false, false),
            Op::Div => (
                if signed {
                    BinaryOp::DivS
                } else {
                    BinaryOp::DivU
                },
                false,
                false,
            ),
            Op::Rem => (
                if signed {
                    BinaryOp::RemS
                } else {
                    BinaryOp::RemU
                },
                false,
                false,
            ),
            Op::BitAnd | Op::BitNand => (BinaryOp::And, false, op == Op::BitNand),
            Op::BitOr | Op::BitNor => (BinaryOp::Or, false, op == Op::BitNor),
            Op::BitXor | Op::BitXnor => (BinaryOp::Xor, false, op == Op::BitXnor),
            Op::LogicShiftL | Op::ArithShiftL => (BinaryOp::Shl, false, false),
            Op::LogicShiftR => (BinaryOp::Shr, false, false),
            Op::ArithShiftR => (if ls { BinaryOp::Sar } else { BinaryOp::Shr }, false, false),
            Op::Eq | Op::EqWildcard => (BinaryOp::Eq, true, false),
            Op::Ne | Op::NeWildcard => (BinaryOp::Ne, true, false),
            Op::Less => (
                if signed { BinaryOp::LtS } else { BinaryOp::LtU },
                true,
                false,
            ),
            Op::LessEq => (
                if signed { BinaryOp::LeS } else { BinaryOp::LeU },
                true,
                false,
            ),
            Op::Greater => (
                if signed { BinaryOp::GtS } else { BinaryOp::GtU },
                true,
                false,
            ),
            Op::GreaterEq => (
                if signed { BinaryOp::GeS } else { BinaryOp::GeU },
                true,
                false,
            ),
            Op::LogicAnd => (BinaryOp::LogicAnd, true, false),
            Op::LogicOr => (BinaryOp::LogicOr, true, false),
            _ => return Err("invalid testbench binary operator".into()),
        };
        let result = self.binary(binary, lhs, rhs, if boolean { 1 } else { width });
        Ok(if invert {
            self.unary(UnaryOp::BitNot, result, width, false)
        } else {
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lower(ops: Vec<ExprOpcode<StateLocation<u32>>>) -> celox_sir::ExecutionUnit<u32> {
        let mut builder = SIRBuilder::new();
        let value = Expressions {
            builder: &mut builder,
        }
        .compile(&ExprBytecode::new(ops))
        .unwrap();
        let width = builder.register(&value.register).width();
        builder.emit(Inst::Store(
            0,
            SIROffset::Static(0),
            width,
            value.register,
            vec![],
            vec![],
        ));
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let unit = celox_sir::ExecutionUnit {
            entry_block_id: celox_sir::BlockId(0),
            blocks,
            register_map,
        };
        unit.verify_result().unwrap();
        unit
    }
    #[test]
    fn wide_arithmetic_uses_sir_instead_of_value_callbacks() {
        let unit = lower(vec![
            ExprOpcode::ConstWide(BigUint::from(1u8) << 170),
            ExprOpcode::ConstU64(37),
            ExprOpcode::TypedBinOp {
                op: Op::Mul,
                lhs_width: 192,
                rhs_width: 192,
                result_width: 192,
                lhs_signed: false,
                rhs_signed: false,
            },
        ]);
        assert!(unit.blocks.values().flat_map(|b|&b.instructions).any(|i|matches!(i,Inst::Binary(dst,_,BinaryOp::Mul,_) if unit.register_map[dst].width()==192)));
        assert!(
            !unit
                .blocks
                .values()
                .flat_map(|b| &b.instructions)
                .any(|i| matches!(i, Inst::ExternCall { .. }))
        );
    }
    #[test]
    fn floating_point_uses_the_shared_sir_operator() {
        let unit = lower(vec![
            ExprOpcode::ConstU64(1.5f64.to_bits()),
            ExprOpcode::ConstU64(2.0f64.to_bits()),
            ExprOpcode::FloatBinary {
                op: celox_design::FloatBinaryOp::Mul,
                precision: celox_design::FloatPrecision::F64,
            },
        ]);
        assert!(
            unit.blocks
                .values()
                .flat_map(|b| &b.instructions)
                .any(|i| matches!(i, Inst::Binary(_, _, BinaryOp::Float { .. }, _)))
        );
    }
    #[test]
    fn strings_use_the_existing_hdl_heap_abi() {
        let unit = lower(vec![ExprOpcode::ConstString(b"native testbench".to_vec())]);
        assert!(
            unit.blocks
                .values()
                .flat_map(|b| &b.instructions)
                .filter_map(|i| if let Inst::ExternCall { func, .. } = i {
                    Some(*func)
                } else {
                    None
                })
                .all(|id| id == celox_sir::extern_abi::HEAP_HELPER_ID)
        );
    }
    #[test]
    fn conditional_side_effects_remain_in_separate_blocks() {
        let unit = lower(vec![
            ExprOpcode::ConstU64(1),
            ExprOpcode::Ternary {
                then_len: 1,
                else_len: 1,
            },
            ExprOpcode::ConstWide(BigUint::from(1u8) << 127),
            ExprOpcode::ConstU64(42),
        ]);
        assert_eq!(unit.blocks.len(), 4);
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "lowering_native_tests.rs"]
pub(crate) mod native_tests;
