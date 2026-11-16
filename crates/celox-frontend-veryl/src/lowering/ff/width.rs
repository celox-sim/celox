//! Widths and constants of expressions, and width and type coercion of registers.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn get_constant_value(&self, expr: &Expression) -> Option<u64> {
        eval_constexpr(expr)?.to_u64()
    }

    pub(super) fn cast_reg_width_ext<A>(
        &self,
        ir_builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        target_width: usize,
        signed: bool,
    ) -> RegisterId {
        let src_type = ir_builder.register(&reg).clone();
        let src_width = src_type.width();
        let alloc_like_source = |builder: &mut SIRBuilder<A>, width, signed| match &src_type {
            RegisterType::Logic { .. } => builder.alloc_logic(width),
            RegisterType::Bit { .. } => builder.alloc_bit(width, signed),
        };
        if src_width == target_width {
            reg
        } else if src_width < target_width {
            let dest = alloc_like_source(ir_builder, target_width, signed);
            if signed {
                let sign = alloc_like_source(ir_builder, 1, false);
                ir_builder.emit(SIRInstruction::Slice(sign, reg, src_width - 1, 1));
                let pad_width = target_width - src_width;
                let pad = if pad_width == 1 {
                    sign
                } else {
                    let ext = alloc_like_source(ir_builder, pad_width, true);
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
            let dest = alloc_like_source(ir_builder, target_width, signed);
            // Resizing transports X/Z bits; a bitwise mask would turn Z into X.
            ir_builder.emit(SIRInstruction::Slice(dest, reg, 0, target_width));
            dest
        }
    }

    pub(super) fn get_expression_width(&self, expr: &Expression) -> usize {
        crate::context_width::get_expr_width(expr)
            .or_else(|| expr.comptime().r#type.total_width())
            .unwrap_or(64)
    }

    pub(super) fn coerce_register_to_formal<A>(
        &self,
        ir_builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        target_width: usize,
        extend_signed: bool,
        result_signed: bool,
        target_is_2state: bool,
    ) -> RegisterId {
        let widened = self.cast_reg_width_ext(ir_builder, reg, target_width, extend_signed);
        if target_is_2state {
            match ir_builder.register(&widened) {
                RegisterType::Bit { width, signed }
                    if *width == target_width && *signed == result_signed =>
                {
                    widened
                }
                RegisterType::Bit { .. } => {
                    let bit_reg = ir_builder.alloc_bit(target_width, result_signed);
                    ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, widened));
                    bit_reg
                }
                RegisterType::Logic { .. } => {
                    let bit_reg = ir_builder.alloc_bit(target_width, result_signed);
                    ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::ToTwoState, widened));
                    bit_reg
                }
            }
        } else if matches!(ir_builder.register(&widened), RegisterType::Logic { .. }) {
            widened
        } else {
            // Four-state signedness is carried by the expression context. A
            // Bit register here would incorrectly discard the formal's X/Z
            // state merely to encode its signed flag.
            let logic_reg = ir_builder.alloc_logic(target_width);
            ir_builder.emit(SIRInstruction::Unary(logic_reg, UnaryOp::Ident, widened));
            logic_reg
        }
    }

    pub(super) fn coerce_register_to_type<A>(
        &self,
        reg: RegisterId,
        ty: &RegisterType,
        signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        match ty {
            RegisterType::Bit {
                width,
                signed: result_signed,
            } => self.coerce_register_to_formal(
                ir_builder,
                reg,
                *width,
                signed,
                *result_signed,
                true,
            ),
            RegisterType::Logic { width } => {
                let widened = self.cast_reg_width_ext(ir_builder, reg, *width, signed);
                if matches!(ir_builder.register(&widened), RegisterType::Logic { .. }) {
                    widened
                } else {
                    let logic = ir_builder.alloc_logic(*width);
                    ir_builder.emit(SIRInstruction::Unary(logic, UnaryOp::Ident, widened));
                    logic
                }
            }
        }
    }

    pub(super) fn coerce_register_to_variable_type<A>(
        &self,
        reg: RegisterId,
        var_id: VarId,
        extend_signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        let variable = &self.module.variables[&var_id];
        let width = resolve_total_width(self.module, variable)?;
        Ok(self.coerce_register_to_formal(
            ir_builder,
            reg,
            width,
            extend_signed,
            variable.r#type.signed,
            variable.r#type.is_2state(),
        ))
    }
}
