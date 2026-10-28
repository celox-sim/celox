//! String operations (IEEE 1800-2023 6.16), lowered to calls of the heap
//! helper.
//!
//! A string value is a 32-bit heap handle. Each operation evaluates its
//! operands, calls the helper with a `celox_heap::ops` opcode, and leaves
//! its result in a temporary that the expression then reads. Operations
//! that build a string fill a heap buffer and intern it at the end.

use super::*;
use celox_heap::ops;
use sv::ir::{Radix, StringOp};

impl Ff<'_, '_> {
    /// The value of string operation `op`, as a temporary.
    pub(super) fn string_call(
        &mut self,
        op: StringOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported(format!("omitted argument of `{op}`")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // A format captures each operand at its own type, in order.
        if let StringOp::Format { ticks_per_unit } = op {
            return self.format_string(ticks_per_unit, &args);
        }
        if args.iter().any(|arg| self.m.calls(arg)) {
            for (index, arg) in args.iter_mut().enumerate() {
                let context = match op {
                    StringOp::FromBits | StringOp::Concat => None,
                    StringOp::Itoa(_) => Some((32, true)),
                    _ if index == 0 || matches!(op, StringOp::Compare | StringOp::ICompare) => {
                        Some((32, false))
                    }
                    StringOp::Putc if index == 2 => Some((64, false)),
                    _ => Some((64, true)),
                };
                *arg = self.heap_operand(arg, context, None)?;
            }
        }
        let result = match op {
            StringOp::Format { .. } => unreachable!("formatted above"),
            StringOp::FromBits => {
                let buffer = self.heap_call(ops::NEW, &[]);
                let buffer = self.append_bits(buffer, &args[0])?;
                self.heap_call(ops::FREEZE, &[buffer])
            }
            StringOp::Concat => {
                let mut buffer = self.heap_call(ops::NEW, &[]);
                for arg in &args {
                    buffer = if self.m.expr_is_string(arg) {
                        let string = self.handle(arg)?;
                        self.heap_call(ops::APPEND, &[buffer, string])
                    } else {
                        self.append_bits(buffer, arg)?
                    };
                }
                self.heap_call(ops::FREEZE, &[buffer])
            }
            StringOp::Replicate => {
                let string = self.handle(&args[0])?;
                let count = self.integer(&args[1], true)?;
                let buffer = self.heap_call(ops::REPLICATE, &[string, count]);
                self.heap_call(ops::FREEZE, &[buffer])
            }
            StringOp::Compare | StringOp::ICompare => {
                let left = self.handle(&args[0])?;
                let right = self.handle(&args[1])?;
                let code = if op == StringOp::Compare {
                    ops::COMPARE
                } else {
                    ops::ICOMPARE
                };
                self.heap_call(code, &[left, right])
            }
            StringOp::Len => {
                let string = self.handle(&args[0])?;
                self.heap_call(ops::LEN, &[string])
            }
            StringOp::Getc => {
                let string = self.handle(&args[0])?;
                let index = self.integer(&args[1], true)?;
                self.heap_call(ops::GETC, &[string, index])
            }
            StringOp::Putc => {
                let string = self.handle(&args[0])?;
                let index = self.integer(&args[1], true)?;
                let character = self.integer(&args[2], false)?;
                let buffer = self.heap_call(ops::COPY, &[string]);
                let buffer = self.heap_call(ops::PUTC, &[buffer, index, character]);
                self.heap_call(ops::FREEZE, &[buffer])
            }
            StringOp::Substr => {
                let string = self.handle(&args[0])?;
                let first = self.integer(&args[1], true)?;
                let last = self.integer(&args[2], true)?;
                let buffer = self.heap_call(ops::SUBSTR, &[string, first, last]);
                self.heap_call(ops::FREEZE, &[buffer])
            }
            StringOp::ToUpper | StringOp::ToLower => {
                let string = self.handle(&args[0])?;
                let code = if op == StringOp::ToUpper {
                    ops::TO_UPPER
                } else {
                    ops::TO_LOWER
                };
                let buffer = self.heap_call(code, &[string]);
                self.heap_call(ops::FREEZE, &[buffer])
            }
            StringOp::Atoi(radix) => {
                let string = self.handle(&args[0])?;
                let parameter = ops::radix_parameter(radix_value(radix), 0, false);
                self.heap_call(ops::encode(ops::ATOI, parameter), &[string])
            }
            StringOp::Itoa(radix) => {
                // The argument is an `integer` (6.16.11).
                let value = self.integer_of_width(&args[0], 32, true)?;
                let parameter = ops::radix_parameter(radix_value(radix), 32, true);
                let buffer = self.heap_call(ops::encode(ops::ITOA, parameter), &[value]);
                self.heap_call(ops::FREEZE, &[buffer])
            }
        };
        let (width, signed) = op.result_type();
        let value = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Slice(value, result, 0, width));
        let (temp, name) = self.m.temp("string", width, signed, false);
        if op.returns_string() {
            self.m.variables.get_mut(&temp).unwrap().value_kind = celox_design::ValueKind::StrRef;
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// `value` as the value of string variable `target`: an integral value
    /// such as a string literal becomes its bytes (IEEE 1800-2023 6.16).
    pub(super) fn string_conversion(
        &self,
        target: SourceVarId,
        value: sv::ir::Expr,
    ) -> sv::ir::Expr {
        if self.m.var(target).value_kind == celox_design::ValueKind::StrRef
            && !self.m.expr_is_string(&value)
        {
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::String(StringOp::FromBits),
                args: vec![Some(value)],
                arg_order: Vec::new(),
            }
        } else {
            value
        }
    }

    /// Call the heap helper with `code` and up to three operands; the result
    /// is a 64-bit register.
    pub(super) fn heap_call(&mut self, code: u64, operands: &[RegisterId]) -> RegisterId {
        let mut args = vec![self.constant64(code)];
        args.extend_from_slice(operands);
        while args.len() < celox_sir::extern_abi::HEAP_HELPER_ARGUMENTS {
            args.push(self.constant64(0));
        }
        let dst = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(dst),
            func: celox_sir::extern_abi::HEAP_HELPER_ID,
            args,
        });
        dst
    }

    pub(super) fn constant64(&mut self, value: u64) -> RegisterId {
        let register = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Imm(
            register,
            SIRValue::new(BigUint::from(value)),
        ));
        register
    }

    /// The handle of string `expr`, as a 64-bit operand.
    pub(super) fn handle(&mut self, expr: &sv::ir::Expr) -> Result<RegisterId, sv::AnalyzerError> {
        self.integer_of_width(expr, celox_design::HANDLE_WIDTH, false)
    }

    /// Integral `expr` as a 64-bit operand, sign-extended if `signed`.
    fn integer(
        &mut self,
        expr: &sv::ir::Expr,
        signed: bool,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        self.integer_of_width(expr, 64, signed)
    }

    /// `expr` converted to `width` bits, then extended to a 64-bit operand.
    /// Unknown bits read as zero.
    pub(super) fn integer_of_width(
        &mut self,
        expr: &sv::ir::Expr,
        width: usize,
        signed: bool,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let expr = self.hoist(expr)?;
        let value = self.eval(&expr, Some((width, signed)))?;
        let known = self.b.alloc_bit(width, signed);
        self.b
            .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
        // An identity cast widens with the operand's signedness.
        let operand = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Unary(operand, UnaryOp::Ident, known));
        Ok(operand)
    }

    /// `buffer` extended by the bytes of integral `expr`, most significant
    /// first.
    fn append_bits(
        &mut self,
        mut buffer: RegisterId,
        expr: &sv::ir::Expr,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let expr = self.hoist(expr)?;
        let natural = self.eval(&expr, None)?;
        let width = self.b.register(&natural).width();
        let bytes = width.div_ceil(8);
        let value = self.eval(&expr, Some((bytes * 8, false)))?;
        let known = self.b.alloc_bit(bytes * 8, false);
        self.b
            .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
        let mut first = 0;
        while first < bytes {
            let count = (bytes - first).min(8);
            let lsb = (bytes - first - count) * 8;
            let chunk = self.b.alloc_bit(count * 8, false);
            self.b
                .emit(SIRInstruction::Slice(chunk, known, lsb, count * 8));
            let operand = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(operand, UnaryOp::Ident, chunk));
            let count_register = self.constant64(count as u64);
            buffer = self.heap_call(ops::APPEND_BYTES, &[buffer, operand, count_register]);
            first += count;
        }
        Ok(buffer)
    }
}

fn radix_value(radix: Radix) -> u32 {
    match radix {
        Radix::Binary => 2,
        Radix::Octal => 8,
        Radix::Decimal => 10,
        Radix::Hexadecimal => 16,
    }
}
