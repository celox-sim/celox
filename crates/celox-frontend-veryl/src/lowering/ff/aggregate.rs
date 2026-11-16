//! Concatenations, struct constructors and array literals.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn parse_concatenation<A>(
        &mut self,
        exprs: &[(Expression, Option<Expression>)],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut evaluated = Vec::with_capacity(exprs.len());
        for (expr, replication) in exprs {
            self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
            let part_reg = self
                .stack
                .pop_back()
                .expect("Concatenation part evaluation failed");
            let part_width = ir_builder.register(&part_reg).width();
            let rep_count = if let Some(rep_expr) = replication {
                use crate::bitaccess::eval_constexpr;
                let v = eval_constexpr(rep_expr);
                v.unwrap().iter_u64_digits().next().unwrap()
            } else {
                1
            };
            for _ in 0..rep_count {
                evaluated.push((part_reg, part_width));
            }
        }

        // Concat transports the value and mask planes without normalizing Z.
        let result = self.emit_concat_registers(&evaluated, ir_builder);
        self.stack.push_back(result);
        Ok(())
    }

    pub(super) fn emit_concat_registers<A>(
        &mut self,
        parts: &[(RegisterId, usize)],
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        if parts.is_empty() {
            let reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Imm(reg, SIRValue::new(0u32)));
            return reg;
        }
        let registers = parts
            .iter()
            .map(|(reg, width)| self.cast_reg_width_ext(ir_builder, *reg, *width, false))
            .collect::<Vec<_>>();
        if registers.len() == 1 {
            return registers[0];
        }
        let total_width = parts.iter().map(|(_, width)| width).sum();
        let result = ir_builder.alloc_logic(total_width);
        ir_builder.emit(SIRInstruction::Concat(result, registers));
        result
    }

    pub(super) fn parse_struct_constructor<A>(
        &mut self,
        ty: &Type,
        fields: &Vec<(veryl_parser::resource_table::StrId, Expression)>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        _context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        let mut parts: Vec<(RegisterId, usize)> = Vec::new();

        for (name, expr) in fields {
            let Some(member_type) = ty.get_member_type(*name) else {
                return Err(ParserError::internal(
                    "struct constructor member",
                    format!("unknown member: {:?} in {:?}", name, ty),
                    Some(&expr.token_range()),
                ));
            };
            let Some(member_width) = member_type.total_width() else {
                return Err(ParserError::internal(
                    "struct constructor member width",
                    format!("member: {:?}, type: {:?}", name, member_type),
                    Some(&expr.token_range()),
                ));
            };
            self.parse_expression(
                expr,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(member_width),
            )?;
            let mut reg = self
                .stack
                .pop_back()
                .expect("Struct constructor part evaluation failed");
            reg = self.coerce_register_to_formal(
                ir_builder,
                reg,
                member_width,
                expression_signed(expr),
                member_type.signed,
                member_type.is_2state(),
            );

            parts.push((reg, member_width));
        }

        let reg = self.emit_concat_registers(&parts, ir_builder);
        self.stack.push_back(reg);
        Ok(())
    }

    pub(super) fn parse_array_literal<A>(
        &mut self,
        items: &Vec<ArrayLiteralItem>,
        expected_width: Option<usize>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut parts: Vec<(RegisterId, usize)> = Vec::new();
        let mut explicit_width = 0usize;
        let mut default_part: Option<(RegisterId, usize)> = None;

        for item in items {
            match item {
                ArrayLiteralItem::Value(expr, repeat) => {
                    self.parse_expression(
                        expr, targets, domain, convert, sources, ir_builder, None,
                    )?;
                    let part_reg = self
                        .stack
                        .pop_back()
                        .expect("Array literal part evaluation failed");
                    let part_width = ir_builder.register(&part_reg).width();

                    let rep_count = if let Some(rep_expr) = repeat {
                        self.get_constant_value(rep_expr).ok_or_else(|| {
                            ParserError::illegal_context(
                                "array literal non-constant repeat",
                                format!("{:?}", rep_expr),
                                Some(&rep_expr.token_range()),
                            )
                        })?
                    } else {
                        1
                    };

                    for _ in 0..rep_count {
                        parts.push((part_reg, part_width));
                    }
                    explicit_width += part_width * rep_count as usize;
                }
                ArrayLiteralItem::Defaul(expr) => {
                    if default_part.is_some() {
                        return Err(ParserError::illegal_context(
                            "array literal multiple default",
                            format!("{:?}", items),
                            Some(&expr.token_range()),
                        ));
                    }

                    self.parse_expression(
                        expr, targets, domain, convert, sources, ir_builder, None,
                    )?;
                    let part_reg = self
                        .stack
                        .pop_back()
                        .expect("Array literal default evaluation failed");
                    let part_width = ir_builder.register(&part_reg).width();
                    default_part = Some((part_reg, part_width));
                }
            }
        }

        if let Some((default_reg, default_width)) = default_part {
            let Some(target_width) = expected_width else {
                return Err(ParserError::illegal_context(
                    "array literal default without context width",
                    format!("{:?}", items),
                    items.first().map(|it| it.token_range()).as_ref(),
                ));
            };

            if explicit_width > target_width {
                return Err(ParserError::illegal_context(
                    "array literal width overflow",
                    format!("explicit_width={explicit_width}, target_width={target_width}"),
                    items.first().map(|it| it.token_range()).as_ref(),
                ));
            }

            let remaining = target_width - explicit_width;
            if default_width == 0 || !remaining.is_multiple_of(default_width) {
                return Err(ParserError::illegal_context(
                    "array literal default width mismatch",
                    format!(
                        "remaining={remaining}, default_width={default_width}, target_width={target_width}"
                    ),
                    items.first().map(|it| it.token_range()).as_ref(),
                ));
            }

            for _ in 0..(remaining / default_width) {
                parts.push((default_reg, default_width));
            }
        }

        let reg = self.emit_concat_registers(&parts, ir_builder);
        self.stack.push_back(reg);
        Ok(())
    }
}
