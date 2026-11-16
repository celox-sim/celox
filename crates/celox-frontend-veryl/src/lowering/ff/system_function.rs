//! System function calls in expressions.

use super::*;

impl<'a> FfParser<'a> {
    fn system_function_type_bits_width(ty: &Type) -> Option<usize> {
        ty.total_width()
            .map(|width| width * ty.total_array().unwrap_or(1))
    }

    fn system_function_input_bits_width(
        &self,
        input: &veryl_analyzer::ir::SystemFunctionInput,
    ) -> usize {
        let comptime = input.0.comptime();
        match &comptime.value {
            ValueVariant::Type(ty) => Self::system_function_type_bits_width(ty).unwrap_or(0),
            _ => Self::system_function_type_bits_width(&comptime.r#type)
                .unwrap_or_else(|| self.get_expression_width(&input.0)),
        }
    }

    fn system_function_input_size(
        &self,
        input: &veryl_analyzer::ir::SystemFunctionInput,
        dimension: usize,
    ) -> usize {
        let comptime = input.0.comptime();
        match &comptime.value {
            ValueVariant::Type(ty) => system_function_type_size(ty, dimension).unwrap_or(0),
            _ => system_function_type_size(&comptime.r#type, dimension).unwrap_or_else(|| {
                // Only the leading dimension can fall back to the evaluated
                // expression's own width.
                if dimension == 1 {
                    self.get_expression_width(&input.0)
                } else {
                    0
                }
            }),
        }
    }

    pub(super) fn parse_system_function_call<A>(
        &mut self,
        call: &SystemFunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        match &call.kind {
            SystemFunctionKind::Bits(input) => {
                let width = self.system_function_input_bits_width(input);
                self.op_constant(SIRValue::new(width as u64), 32, ir_builder);
                Ok(())
            }
            SystemFunctionKind::Size(input, dimension) => {
                let dimension = system_function_dimension(dimension.as_ref()).ok_or_else(|| {
                    ParserError::illegal_context(
                        "$size dimension",
                        "the dimension must be a compile-time value",
                        Some(&call.comptime.token),
                    )
                })?;
                let size = self.system_function_input_size(input, dimension);
                self.op_constant(SIRValue::new(size as u64), 32, ir_builder);
                Ok(())
            }
            SystemFunctionKind::Clog2(input) => {
                self.parse_expression(
                    &input.0, targets, domain, convert, sources, ir_builder, None,
                )?;
                let arg = self.stack.pop_back().expect("Invalid $clog2 input");
                let width = ir_builder.register(&arg).width();

                let mut result = ir_builder.alloc_bit(32, false);
                ir_builder.emit(SIRInstruction::Imm(result, SIRValue::new(0u8)));
                for k in 1..=width {
                    let threshold = ir_builder.alloc_bit(width, false);
                    ir_builder.emit(SIRInstruction::Imm(
                        threshold,
                        SIRValue::new(BigUint::from(1u8) << (k - 1)),
                    ));
                    let cond = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(cond, arg, BinaryOp::GtU, threshold));
                    let value = ir_builder.alloc_bit(32, false);
                    ir_builder.emit(SIRInstruction::Imm(value, SIRValue::new(k as u64)));
                    let next = ir_builder.alloc_logic(32);
                    ir_builder.emit(SIRInstruction::Mux(next, cond, value, result));
                    result = next;
                }
                self.stack.push_back(result);
                Ok(())
            }
            SystemFunctionKind::Onehot(input) => {
                self.parse_expression(
                    &input.0, targets, domain, convert, sources, ir_builder, None,
                )?;
                let arg = self.stack.pop_back().expect("Invalid $onehot input");
                let width = ir_builder.register(&arg).width();

                let zero = ir_builder.alloc_bit(width, false);
                ir_builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
                let one = ir_builder.alloc_bit(width, false);
                ir_builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));

                let arg_minus_one = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(
                    arg_minus_one,
                    arg,
                    BinaryOp::Sub,
                    one,
                ));

                let overlap = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(
                    overlap,
                    arg,
                    BinaryOp::And,
                    arg_minus_one,
                ));

                let non_zero = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(non_zero, arg, BinaryOp::Ne, zero));

                let no_overlap = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    no_overlap,
                    overlap,
                    BinaryOp::Eq,
                    zero,
                ));

                let result = ir_builder.alloc_logic(1);
                ir_builder.emit(SIRInstruction::Binary(
                    result,
                    non_zero,
                    BinaryOp::LogicAnd,
                    no_overlap,
                ));
                self.stack.push_back(result);
                Ok(())
            }
            SystemFunctionKind::Signed(input) | SystemFunctionKind::Unsigned(input) => {
                self.parse_expression(
                    &input.0, targets, domain, convert, sources, ir_builder, None,
                )?;
                let src = self
                    .stack
                    .pop_back()
                    .expect("Invalid $signed/$unsigned input");
                let width = ir_builder.register(&src).width();
                let signed = matches!(call.kind, SystemFunctionKind::Signed(_));
                let casted = match ir_builder.register(&src) {
                    RegisterType::Logic { .. } => ir_builder.alloc_logic(width),
                    RegisterType::Bit { .. } => ir_builder.alloc_bit(width, signed),
                };
                ir_builder.emit(SIRInstruction::Unary(casted, UnaryOp::Ident, src));
                self.stack.push_back(casted);
                Ok(())
            }
            SystemFunctionKind::Readmemh(_, _)
            | SystemFunctionKind::Display(_)
            | SystemFunctionKind::Write(_)
            | SystemFunctionKind::Assert { .. }
            | SystemFunctionKind::Finish => Err(ParserError::illegal_context(
                "system task in FF expression",
                format!("{call}"),
                Some(&call.comptime.token),
            )),
        }
    }
}
