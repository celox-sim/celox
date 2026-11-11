//! Operators: unary, binary, logical, power and the ternary.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn op_binary<A>(
        &mut self,
        op: &Op,
        width: usize,
        left_source_signed: bool,
        right_source_signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let right = self.stack.pop_back().expect("invalid ir");
        let left = self.stack.pop_back().expect("invalid ir");

        // Decompose BitXnor/BitNand/BitNor into existing operations
        match op {
            Op::BitXnor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(tmp, left, BinaryOp::Xor, right));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::BitNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitNand => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(tmp, left, BinaryOp::And, right));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::BitNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitNor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(tmp, left, BinaryOp::Or, right));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::BitNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            _ => {}
        }

        let dest_reg = ir_builder.alloc_logic(width);
        let op = resolve_binary_op(*op, left_source_signed, right_source_signed);
        ir_builder.emit(SIRInstruction::Binary(dest_reg, left, op, right));
        self.stack.push_back(dest_reg);
    }

    fn parse_logic_op<A>(
        &mut self,
        is_and: bool,
        left: &Expression,
        right: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        self.parse_expression_in_context(
            left, targets, domain, convert, sources, ir_builder, None,
        )?;
        let lhs = self.stack.pop_back().unwrap();
        let array_view_candidates = self.array_view_merge_candidates([right]);
        let array_view_params =
            self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
        let pre_rhs_array_views = self.frames.function_array_view_stack.clone();
        let pre_rhs_state = (expression_has_side_effect(right)
            || self.expression_has_runtime_effect(right)
            || !array_view_candidates.is_empty())
        .then(|| {
            (
                self.defined.defined_ranges.clone(),
                self.defined.dynamic_defined_vars.clone(),
            )
        });

        // Only a definite dominant value may short-circuit.  Logical-not
        // produces a one-bit 4-state truth value; ToTwoState maps its X result
        // to zero so an indeterminate LHS continues into the full operation.
        let not_lhs = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Unary(not_lhs, UnaryOp::LogicNot, lhs));
        let shortcut_truth = if is_and {
            not_lhs
        } else {
            let truth = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Unary(truth, UnaryOp::LogicNot, not_lhs));
            truth
        };
        let shortcut = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            shortcut,
            UnaryOp::ToTwoState,
            shortcut_truth,
        ));

        let rhs_block = ir_builder.new_block();
        let result_param = ir_builder.alloc_logic(1);
        let mut merge_params = vec![result_param];
        for params in &array_view_params {
            merge_params.push(params.initialized);
            merge_params.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                merge_params.push(cached.initialized);
                merge_params.extend(cached.elements.iter().copied());
            }
        }
        let merge_block = ir_builder.new_block_with(merge_params);
        let shortcut_value = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(
            shortcut_value,
            SIRValue::new(if is_and { 0u8 } else { 1u8 }),
        ));
        let mut shortcut_args = vec![shortcut_value];
        shortcut_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: shortcut,
            true_block: (merge_block, shortcut_args),
            false_block: (rhs_block, vec![]),
        });

        ir_builder.switch_to_block(rhs_block);
        self.parse_expression_in_context(
            right, targets, domain, convert, sources, ir_builder, None,
        )?;
        let rhs = self.stack.pop_back().unwrap();
        let rhs_state = pre_rhs_state.as_ref().map(|(pre_defined, pre_dynamic)| {
            (
                std::mem::replace(&mut self.defined.defined_ranges, pre_defined.clone()),
                std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_dynamic.clone()),
            )
        });
        let evaluated = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Binary(
            evaluated,
            lhs,
            if is_and {
                BinaryOp::LogicAnd
            } else {
                BinaryOp::LogicOr
            },
            rhs,
        ));
        let rhs_array_views = self.frames.function_array_view_stack.clone();
        let mut rhs_args = vec![evaluated];
        rhs_args.extend(self.array_view_state_args(&array_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Jump(merge_block, rhs_args));

        ir_builder.switch_to_block(merge_block);
        self.frames.function_array_view_stack = pre_rhs_array_views.clone();
        self.install_merged_array_views(
            &array_view_params,
            &[&pre_rhs_array_views, &rhs_array_views],
        );
        if let (Some((pre_defined, pre_dynamic)), Some((rhs_defined, rhs_dynamic))) =
            (pre_rhs_state, rhs_state)
        {
            self.defined.defined_ranges = self.intersect_defined_states(pre_defined, rhs_defined);
            self.defined.dynamic_defined_vars =
                self.intersect_dynamic_vars(pre_dynamic, rhs_dynamic);
        }
        self.stack.push_back(result_param);
        Ok(())
    }

    pub(super) fn op_unary<A>(&mut self, op: &Op, width: usize, ir_builder: &mut SIRBuilder<A>) {
        let expr = self.stack.pop_back().expect("invalid ir");

        // Decompose Reduction Nand/Nor/Xnor into existing reduction + Not
        match op {
            Op::BitNand => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(tmp, UnaryOp::And, expr));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::LogicNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitNor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(tmp, UnaryOp::Or, expr));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::LogicNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            Op::BitXnor => {
                let tmp = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(tmp, UnaryOp::Xor, expr));
                let dest = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::LogicNot, tmp));
                self.stack.push_back(dest);
                return;
            }
            _ => {}
        }

        let dest_reg = ir_builder.alloc_logic(width);
        let op = match op {
            Op::Pow => unreachable!("Pow is binary and must not be lowered by op_unary"),
            Op::Div => unreachable!("Div is binary and must not be lowered by op_unary"),
            Op::Rem => unreachable!("Rem is binary and must not be lowered by op_unary"),
            Op::Mul => unreachable!("Mul is binary and must not be lowered by op_unary"),
            Op::Add => UnaryOp::Ident,
            Op::Sub => UnaryOp::Minus,
            Op::ArithShiftL => {
                unreachable!("ArithShiftL is binary and must not be lowered by op_unary")
            }
            Op::ArithShiftR => {
                unreachable!("ArithShiftR is binary and must not be lowered by op_unary")
            }
            Op::LogicShiftL => {
                unreachable!("LogicShiftL is binary and must not be lowered by op_unary")
            }
            Op::LogicShiftR => {
                unreachable!("LogicShiftR is binary and must not be lowered by op_unary")
            }
            Op::LessEq => unreachable!("LessEq is binary and must not be lowered by op_unary"),
            Op::GreaterEq => {
                unreachable!("GreaterEq is binary and must not be lowered by op_unary")
            }
            Op::Less => unreachable!("Less is binary and must not be lowered by op_unary"),
            Op::Greater => unreachable!("Greater is binary and must not be lowered by op_unary"),
            Op::Eq => unreachable!("Eq is binary and must not be lowered by op_unary"),
            Op::EqWildcard => {
                unreachable!("EqWildcard is binary and must not be lowered by op_unary")
            }
            Op::Ne => unreachable!("Ne is binary and must not be lowered by op_unary"),
            Op::NeWildcard => {
                unreachable!("NeWildcard is binary and must not be lowered by op_unary")
            }
            Op::LogicAnd => {
                unreachable!("LogicAnd is binary and must not be lowered by op_unary")
            }
            Op::LogicOr => unreachable!("LogicOr is binary and must not be lowered by op_unary"),
            Op::LogicNot => UnaryOp::LogicNot,
            Op::BitAnd => UnaryOp::And,
            Op::BitOr => UnaryOp::Or,
            Op::BitXor => UnaryOp::Xor,
            // BitNand, BitNor, BitXnor are handled above via decomposition
            Op::BitNand | Op::BitNor | Op::BitXnor => unreachable!(),
            Op::BitNot => UnaryOp::BitNot,
            Op::As => unreachable!("As is binary and must not be lowered by op_unary"),
            Op::Ternary => {
                unreachable!("Ternary expression must be lowered by ternary-specific path")
            }
            Op::Concatenation => {
                unreachable!("Concatenation must be lowered by concat-specific path")
            }
            Op::ArrayLiteral => unreachable!("Array literal must not be lowered by op_unary"),
            Op::Condition => unreachable!("Condition node must not be lowered by op_unary"),
            Op::Repeat => unreachable!("Repeat node must be lowered by repeat-specific path"),
        };
        ir_builder.emit(SIRInstruction::Unary(dest_reg, op, expr));
        self.stack.push_back(dest_reg);
    }

    pub(super) fn parse_binary<A>(
        &mut self,
        op: &Op,
        left: &Expression,
        right: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        if matches!(op, Op::LogicAnd | Op::LogicOr) {
            self.parse_logic_op(
                matches!(op, Op::LogicAnd),
                left,
                right,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            )?;
            if let Some(context) = context {
                let result = self.stack.pop_back().unwrap();
                let result =
                    self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
                self.stack.push_back(result);
            }
            return Ok(());
        }

        if matches!(op, Op::As) {
            let Some(cast) = cast_semantics(left, right) else {
                return Err(ParserError::internal(
                    "as cast target",
                    format!("{:?}", right),
                    Some(&right.token_range()),
                ));
            };
            self.parse_expression_in_context(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                Some(ValueContext {
                    width: cast.width,
                    signed: cast.source_signed,
                }),
            )?;
            let src = self
                .stack
                .pop_back()
                .expect("Invalid cast source expression");

            let casted = if cast.result_is_2state {
                ir_builder.alloc_bit(cast.width, cast.result_signed)
            } else {
                ir_builder.alloc_logic(cast.width)
            };
            let cast_op = if cast.result_is_2state && !cast.source_is_2state {
                UnaryOp::ToTwoState
            } else {
                UnaryOp::Ident
            };
            ir_builder.emit(SIRInstruction::Unary(casted, cast_op, src));
            let casted = if let Some(context) = context {
                self.cast_reg_width_ext(ir_builder, casted, context.width, context.signed)
            } else {
                casted
            };
            self.stack.push_back(casted);
            return Ok(());
        }

        let lhs_width = self.get_expression_width(left);
        let rhs_width = self.get_expression_width(right);
        let lhs_signed = expression_signed(left);
        let rhs_signed = expression_signed(right);
        let semantics =
            binary_semantics(*op, lhs_width, rhs_width, lhs_signed, rhs_signed, context);

        if matches!(op, Op::Pow) {
            let width = semantics.result_width;
            self.parse_expression_in_context(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                semantics.lhs_context,
            )?;
            let base = self
                .stack
                .pop_back()
                .expect("Invalid LHS for power operation");
            self.parse_expression_in_context(
                right,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                semantics.rhs_context,
            )?;
            let exponent = self
                .stack
                .pop_back()
                .expect("Invalid RHS for power operation");
            let result = self.lower_runtime_pow(base, exponent, width, rhs_signed, ir_builder);

            let result = if let Some(context) = context {
                self.cast_reg_width_ext(ir_builder, result, context.width, context.signed)
            } else {
                result
            };
            self.stack.push_back(result);
            return Ok(());
        }

        self.parse_expression_in_context(
            left,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            semantics.lhs_context,
        )?;
        self.parse_expression_in_context(
            right,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            semantics.rhs_context,
        )?;
        self.op_binary(
            op,
            semantics.result_width,
            semantics.lhs_signed,
            semantics.rhs_signed,
            ir_builder,
        );
        if let Some(context) = context {
            let result = self.stack.pop_back().unwrap();
            let result = self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
            self.stack.push_back(result);
        }
        Ok(())
    }

    fn lower_runtime_pow<A>(
        &self,
        base: RegisterId,
        exponent: RegisterId,
        width: usize,
        exponent_signed: bool,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        let exponent_width = ir_builder.register(&exponent).width();
        debug_assert!(width > 0);
        debug_assert!(exponent_width > 0);

        let mut constant = |value: BigUint, mask: BigUint| {
            let reg = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Imm(
                reg,
                SIRValue::new_four_state(value, mask),
            ));
            reg
        };
        let zero = constant(BigUint::from(0u8), BigUint::from(0u8));
        let one = constant(BigUint::from(1u8), BigUint::from(0u8));
        let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let minus_one = constant(width_mask.clone(), BigUint::from(0u8));
        let unknown = constant(BigUint::from(0u8), width_mask);

        // Exponentiation by squaring keeps the generated SIR linear in the
        // exponent width instead of in its runtime value.
        let mut result = one;
        let mut factor = base;
        let exponent_lsb = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Slice(exponent_lsb, exponent, 0, 1));
        let magnitude_width = exponent_width - usize::from(exponent_signed);
        for bit_index in 0..magnitude_width {
            let bit = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Slice(bit, exponent, bit_index, 1));

            let product = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Binary(
                product,
                result,
                BinaryOp::Mul,
                factor,
            ));
            let next = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Mux(next, bit, product, result));
            result = next;

            if bit_index + 1 != magnitude_width {
                let squared = ir_builder.alloc_logic(width);
                ir_builder.emit(SIRInstruction::Binary(
                    squared,
                    factor,
                    BinaryOp::Mul,
                    factor,
                ));
                factor = squared;
            }
        }

        if exponent_signed {
            let sign = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Slice(sign, exponent, exponent_width - 1, 1));
            let base_is_zero = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Binary(
                base_is_zero,
                base,
                BinaryOp::Eq,
                zero,
            ));
            let base_is_one = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Binary(base_is_one, base, BinaryOp::Eq, one));
            let base_is_minus_one = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Binary(
                base_is_minus_one,
                base,
                BinaryOp::Eq,
                minus_one,
            ));
            let minus_one_result = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Mux(
                minus_one_result,
                exponent_lsb,
                minus_one,
                one,
            ));
            let nonunit = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Mux(
                nonunit,
                base_is_minus_one,
                minus_one_result,
                zero,
            ));
            let negative_nonzero = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Mux(
                negative_nonzero,
                base_is_one,
                one,
                nonunit,
            ));
            let negative_result = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Mux(
                negative_result,
                base_is_zero,
                unknown,
                negative_nonzero,
            ));
            let selected = ir_builder.alloc_logic(width);
            ir_builder.emit(SIRInstruction::Mux(selected, sign, negative_result, result));
            result = selected;
        }

        // IEEE power semantics produce an entirely unknown result if either
        // operand contains X/Z. Comparing each operand with its two-state
        // image is known one exactly when it has no unknown bits; muxing
        // against all-X expands any unknown predicate to the result width.
        let base_two_state = ir_builder.alloc_bit(width, false);
        ir_builder.emit(SIRInstruction::Unary(
            base_two_state,
            UnaryOp::ToTwoState,
            base,
        ));
        let exponent_two_state = ir_builder.alloc_bit(exponent_width, false);
        ir_builder.emit(SIRInstruction::Unary(
            exponent_two_state,
            UnaryOp::ToTwoState,
            exponent,
        ));
        let base_known = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Binary(
            base_known,
            base,
            BinaryOp::Eq,
            base_two_state,
        ));
        let exponent_known = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Binary(
            exponent_known,
            exponent,
            BinaryOp::Eq,
            exponent_two_state,
        ));
        let operands_known = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Binary(
            operands_known,
            base_known,
            BinaryOp::LogicAnd,
            exponent_known,
        ));
        let final_result = ir_builder.alloc_logic(width);
        ir_builder.emit(SIRInstruction::Mux(
            final_result,
            operands_known,
            result,
            unknown,
        ));
        final_result
    }

    pub(super) fn parse_unary<A>(
        &mut self,
        op: &Op,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let is_reduction = matches!(
            op,
            Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::BitNand
                | Op::BitNor
                | Op::BitXnor
                | Op::LogicNot
        );
        let width = if is_reduction {
            1
        } else {
            self.get_expression_width(expr)
                .max(context.map(|context| context.width).unwrap_or(0))
        };
        // Reduction and logical-not operators reduce a multi-bit operand to 1 bit.
        // The operand must be evaluated at its own natural width, not the (narrower)
        // context width of the result — otherwise the input gets truncated before
        // the reduction is applied.
        let operand_context = if is_reduction { None } else { context };
        self.parse_expression_in_context(
            expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            operand_context,
        )?;
        self.op_unary(op, width, ir_builder);
        if is_reduction && let Some(context) = context {
            let result = self.stack.pop_back().unwrap();
            let result = self.cast_reg_width_ext(ir_builder, result, context.width, context.signed);
            self.stack.push_back(result);
        }
        Ok(())
    }

    pub(super) fn parse_ternary<A>(
        &mut self,
        cond: &Expression,
        then: &Expression,
        els: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context: Option<ValueContext>,
    ) -> Result<(), ParserError> {
        let branch_context = ValueContext {
            width: self
                .get_expression_width(then)
                .max(self.get_expression_width(els))
                .max(context.map(|context| context.width).unwrap_or(0)),
            signed: context
                .map(|context| context.signed)
                .unwrap_or_else(|| expression_signed(then) && expression_signed(els)),
        };
        let result_width = branch_context.width;
        self.parse_expression_in_context(
            cond, targets, domain, convert, sources, ir_builder, None,
        )?;
        let cond_reg = self.stack.pop_back().unwrap();

        let array_view_candidates = self.array_view_merge_candidates([then, els]);
        let branch_materializes_array_view = !array_view_candidates.is_empty();

        if !expression_has_side_effect(then)
            && !expression_has_side_effect(els)
            && !self.expression_has_runtime_effect(then)
            && !self.expression_has_runtime_effect(els)
            && !branch_materializes_array_view
        {
            // Evaluate pure ternary trees with an explicit post-order stack.
            // Large decoder muxes must not add a Rust frame for every arm.
            enum Pending<'a> {
                Eval(&'a Expression, ValueContext),
                Merge(RegisterId, usize),
            }
            let mut pending = vec![
                Pending::Merge(cond_reg, result_width),
                Pending::Eval(els, branch_context),
                Pending::Eval(then, branch_context),
            ];
            while let Some(item) = pending.pop() {
                match item {
                    Pending::Merge(cond, width) => {
                        let els = self.stack.pop_back().unwrap();
                        let then = self.stack.pop_back().unwrap();
                        let result = ir_builder.alloc_logic(width);
                        ir_builder.emit(SIRInstruction::Mux(result, cond, then, els));
                        self.stack.push_back(result);
                    }
                    Pending::Eval(expr, context) => {
                        if let Expression::Ternary(cond, then, els, comptime) = expr
                            && !comptime.is_const
                            && self
                                .get_bound_function_event_arg_state(expr.token_range())
                                .is_none()
                            && self
                                .get_bound_function_expression_value(expr.token_range())
                                .is_none()
                            && crate::float::precision(crate::float::type_of(expr)).is_none()
                            && crate::float::precision(crate::float::type_of(then)).is_none()
                            && crate::float::precision(crate::float::type_of(els)).is_none()
                        {
                            let context = ValueContext {
                                width: self
                                    .get_expression_width(then)
                                    .max(self.get_expression_width(els))
                                    .max(context.width),
                                signed: context.signed,
                            };
                            self.parse_expression_in_context(
                                cond, targets, domain, convert, sources, ir_builder, None,
                            )?;
                            let cond = self.stack.pop_back().unwrap();
                            pending.push(Pending::Merge(cond, context.width));
                            pending.push(Pending::Eval(els, context));
                            pending.push(Pending::Eval(then, context));
                        } else {
                            self.parse_expression_in_context(
                                expr,
                                targets,
                                domain,
                                convert,
                                sources,
                                ir_builder,
                                Some(context),
                            )?;
                        }
                    }
                }
            }
            return Ok(());
        }

        let pre_ternary_defined = self.defined.defined_ranges.clone();
        let pre_ternary_dynamic = self.defined.dynamic_defined_vars.clone();
        let pre_ternary_array_views = self.frames.function_array_view_stack.clone();

        // A known condition evaluates only the selected arm. An X/Z
        // condition evaluates both arms and merges their bits.
        let not_cond = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Unary(not_cond, UnaryOp::LogicNot, cond_reg));
        let known_false = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            known_false,
            UnaryOp::ToTwoState,
            not_cond,
        ));
        let true_truth = ir_builder.alloc_logic(1);
        ir_builder.emit(SIRInstruction::Unary(
            true_truth,
            UnaryOp::LogicNot,
            not_cond,
        ));
        let known_true = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            known_true,
            UnaryOp::ToTwoState,
            true_truth,
        ));

        let dummy_then = ir_builder.alloc_logic(result_width);
        ir_builder.emit(SIRInstruction::Imm(dummy_then, SIRValue::new(0u8)));
        let direct_else = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(direct_else, SIRValue::new(0u8)));
        let merge_else = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(merge_else, SIRValue::new(1u8)));

        let else_view_params =
            self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
        let merge_view_params =
            self.alloc_array_view_merge_params(&array_view_candidates, ir_builder)?;
        let then_block = ir_builder.new_block();
        let carried_then = ir_builder.alloc_logic(result_width);
        let needs_merge = ir_builder.alloc_bit(1, false);
        let mut else_params = vec![carried_then, needs_merge];
        for params in &else_view_params {
            else_params.push(params.initialized);
            else_params.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                else_params.push(cached.initialized);
                else_params.extend(cached.elements.iter().copied());
            }
        }
        let else_block = ir_builder.new_block_with(else_params);
        let result = ir_builder.alloc_logic(result_width);
        let mut merge_params = vec![result];
        for params in &merge_view_params {
            merge_params.push(params.initialized);
            merge_params.extend(params.elements.iter().copied());
            for cached in &params.cached_literal_items {
                merge_params.push(cached.initialized);
                merge_params.extend(cached.elements.iter().copied());
            }
        }
        let merge_block = ir_builder.new_block_with(merge_params);

        let mut initial_else_args = vec![dummy_then, direct_else];
        initial_else_args.extend(self.array_view_state_args(&else_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: known_false,
            true_block: (else_block, initial_else_args),
            false_block: (then_block, vec![]),
        });

        ir_builder.switch_to_block(then_block);
        self.parse_expression_in_context(
            then,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(branch_context),
        )?;
        let then_val = self.stack.pop_back().unwrap();
        let then_defined = std::mem::replace(
            &mut self.defined.defined_ranges,
            pre_ternary_defined.clone(),
        );
        let then_dynamic = std::mem::replace(
            &mut self.defined.dynamic_defined_vars,
            pre_ternary_dynamic.clone(),
        );
        let then_array_views = self.frames.function_array_view_stack.clone();
        let mut then_merge_args = vec![then_val];
        let mut then_else_args = vec![then_val, merge_else];
        then_merge_args.extend(self.array_view_state_args(&merge_view_params, ir_builder)?);
        then_else_args.extend(self.array_view_state_args(&else_view_params, ir_builder)?);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: known_true,
            true_block: (merge_block, then_merge_args),
            false_block: (else_block, then_else_args),
        });

        self.frames.function_array_view_stack = pre_ternary_array_views.clone();
        ir_builder.switch_to_block(else_block);
        self.install_merged_array_views(
            &else_view_params,
            &[&pre_ternary_array_views, &then_array_views],
        );
        self.parse_expression_in_context(
            els,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(branch_context),
        )?;
        let else_val = self.stack.pop_back().unwrap();
        let else_defined = std::mem::take(&mut self.defined.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.defined.dynamic_defined_vars);
        let else_array_views = self.frames.function_array_view_stack.clone();
        let merged = ir_builder.alloc_logic(result_width);
        ir_builder.emit(SIRInstruction::Mux(
            merged,
            cond_reg,
            carried_then,
            else_val,
        ));
        let direct_else_block = ir_builder.new_block();
        let mut else_merge_state = Vec::new();
        else_merge_state.extend(self.array_view_state_args(&merge_view_params, ir_builder)?);
        let mut merged_args = vec![merged];
        merged_args.extend(else_merge_state.iter().copied());
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: needs_merge,
            true_block: (merge_block, merged_args),
            false_block: (direct_else_block, vec![]),
        });

        ir_builder.switch_to_block(direct_else_block);
        let mut direct_else_args = vec![else_val];
        direct_else_args.extend(else_merge_state);
        ir_builder.seal_block(SIRTerminator::Jump(merge_block, direct_else_args));

        self.frames.function_array_view_stack = pre_ternary_array_views;

        ir_builder.switch_to_block(merge_block);
        let pre_views = self.frames.function_array_view_stack.clone();
        self.install_merged_array_views(
            &merge_view_params,
            &[&pre_views, &then_array_views, &else_array_views],
        );
        self.defined.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
        self.defined.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
        self.stack.push_back(result);
        Ok(())
    }
}
