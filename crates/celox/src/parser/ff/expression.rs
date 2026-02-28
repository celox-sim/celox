use super::{Domain, FfParser};
use crate::ir::{
    BinaryOp, BitAccess, RegisterId, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue, STABLE_REGION, UnaryOp, VarAtomBase, WORKING_REGION,
};
use crate::parser::{
    ParserError,
    bitaccess::{eval_var_select, is_static_access},
};
use malachite_bigint::BigUint;

use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, AssignStatement, Expression, Factor, Op, Type,
    ValueVariant, VarId, VarIndex, VarSelect,
};

impl<'a> FfParser<'a> {
    pub(super) fn emit_offset_calc<A>(
        &mut self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<SIROffset, ParserError> {
        let variable = &self.module.variables[&var_id];
        let var_type = &variable.r#type;

        // 1. Calculate Stride for array dimensions
        let array_dims: Vec<usize> = var_type
            .array
            .as_slice()
            .iter()
            .map(|d| d.expect("Array dimension must be known"))
            .collect();

        // Scalar bit width (N in logic<N>)
        let scalar_bits = var_type.width.total().unwrap_or(1);

        // Strides: How many bits each array index moves
        let mut strides = vec![0; array_dims.len()];
        let mut current_stride = scalar_bits;
        for i in (0..array_dims.len()).rev() {
            strides[i] = current_stride;
            current_stride *= array_dims[i];
        }

        // 2. Offset calculation (Static + Dynamic)
        let mut static_offset: u64 = 0;
        let mut dynamic_offset_reg: Option<RegisterId> = None;

        let mut add_dynamic_term = |term_reg: RegisterId, builder: &mut SIRBuilder<A>| {
            if let Some(curr) = dynamic_offset_reg {
                let next = builder.alloc_bit(64, false);
                builder.emit(SIRInstruction::Binary(next, curr, BinaryOp::Add, term_reg));
                dynamic_offset_reg = Some(next);
            } else {
                dynamic_offset_reg = Some(term_reg);
            }
        };

        // 3. Array index part (VarIndex)
        let mut dummy_targets: Vec<VarAtomBase<A>> = Vec::new();

        for (i, expr) in index.0.iter().enumerate() {
            let stride = strides[i];
            if let Some(c) = self.get_constant_value(expr) {
                static_offset += c * (stride as u64);
            } else {
                // Dynamic term
                let term_reg = self.emit_arith_term(
                    expr,
                    &mut dummy_targets,
                    stride,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                add_dynamic_term(term_reg, ir_builder);
            }
        }

        // 4. Bit select / Final dimension array part (VarSelect)
        let select_len = select.0.len();
        for (i, expr) in select.0.iter().enumerate() {
            // Case: final element and slice (Option exists)
            if i == select_len - 1
                && let Some((op, end_expr)) = &select.1
            {
                // Get LSB expression using VarSelectOp::eval_expr
                let (_, lsb_expr) = op.eval_expr(expr, end_expr);

                // Stride is 1 since it's the start position of a bit slice
                if let Some(c) = self.get_constant_value(&lsb_expr) {
                    static_offset += c;
                } else {
                    let term_reg = self.emit_arith_term(
                        &lsb_expr,
                        &mut dummy_targets,
                        1,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    add_dynamic_term(term_reg, ir_builder);
                }
            } else {
                // Normal index (array dimension or single bit select)
                // Stride is strides[j] if still in array dimension, 1 if inside scalar
                let stride = if i < strides.len() { strides[i] } else { 1 };

                if let Some(c) = self.get_constant_value(expr) {
                    static_offset += c * (stride as u64);
                } else {
                    let term_reg = self.emit_arith_term(
                        expr,
                        &mut dummy_targets,
                        stride,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                    add_dynamic_term(term_reg, ir_builder);
                }
            }
        }

        if let Some(dyn_reg) = dynamic_offset_reg {
            if static_offset == 0 {
                Ok(SIROffset::Dynamic(dyn_reg))
            } else {
                // Combine dynamic + static
                let s_reg = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Imm(s_reg, SIRValue::new(static_offset)));
                let total_reg = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Binary(
                    total_reg,
                    dyn_reg,
                    BinaryOp::Add,
                    s_reg,
                ));
                Ok(SIROffset::Dynamic(total_reg))
            }
        } else {
            Ok(SIROffset::Static(static_offset as usize))
        }
    }

    /// Helper: returns (expr * stride)
    pub(super) fn emit_arith_term<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        stride: usize,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
        let idx_reg = self.stack.pop_back().unwrap();

        // Optimization possible by skipping multiplication if stride == 1
        if stride == 1 {
            Ok(idx_reg)
        } else {
            let s_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(s_reg, SIRValue::new(stride as u64)));

            let m_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Binary(m_reg, idx_reg, BinaryOp::Mul, s_reg));
            Ok(m_reg)
        }
    }

    pub(super) fn is_range_fully_defined(&self, var_id: VarId, access: BitAccess) -> bool {
        if let Some(bits) = self.defined_ranges.get(&var_id) {
            // Whether all bits in the specified range [lsb, msb] are set in BitSet
            (access.lsb..=access.msb).all(|i| bits.contains(i))
        } else {
            false
        }
    }

    pub(super) fn op_load<A>(
        &mut self,
        var_id: VarId,
        index: &VarIndex,
        select: &VarSelect,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let access = eval_var_select(self.module, var_id, index, select);
        let width = access.msb - access.lsb + 1; // Selected bit width
        let dest_reg = if self.module.variables[&var_id].r#type.signed {
            ir_builder.alloc_bit(width, true)
        } else {
            ir_builder.alloc_logic(width)
        };

        let offset =
            self.emit_offset_calc(var_id, index, select, domain, convert, sources, ir_builder)?;

        let access = eval_var_select(self.module, var_id, index, select);
        let width = access.msb - access.lsb + 1;

        ir_builder.emit(SIRInstruction::Load(
            dest_reg,
            convert(var_id, STABLE_REGION),
            offset,
            width,
        ));

        self.stack.push_back(dest_reg);

        let is_internal = self.is_range_fully_defined(var_id, access)
            || self.dynamic_defined_vars.contains(&var_id);
        if !is_internal {
            sources.push(VarAtomBase::new(
                convert(var_id, STABLE_REGION),
                access.lsb,
                access.msb,
            ));
        }
        Ok(())
    }

    pub(super) fn op_store<A>(
        &mut self,
        dst: &AssignDestination,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let src_reg = self.stack.pop_back().expect("invalid ir");

        let offset = self.emit_offset_calc(
            dst.id,
            &dst.index,
            &dst.select,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select);

        let target_width = access.msb - access.lsb + 1;
        ir_builder.emit(SIRInstruction::Store(
            convert(dst.id, domain.region()),
            offset,
            target_width,
            src_reg,
            Vec::new(),
        ));

        if is_static_access(&dst.index, &dst.select) {
            let bits = self.defined_ranges.entry(dst.id).or_default();
            for i in access.lsb..=access.msb {
                bits.insert(i);
            }
        }
        self.dynamic_defined_vars.insert(dst.id);

        if matches!(domain, Domain::Ff) {
            // This is a temporary hack since we don't know the clock yet.
            // We will move targets into clock-specific buckets in parse_ff_declaration.
            targets.push(VarAtomBase::new(
                convert(dst.id, WORKING_REGION),
                access.lsb,
                access.msb,
            ));
        }
        Ok(())
    }

    pub(super) fn op_binary<A>(&mut self, op: &Op, width: usize, ir_builder: &mut SIRBuilder<A>) {
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
        let left_is_signed = ir_builder.register(&left).is_signed();
        let right_is_signed = ir_builder.register(&right).is_signed();
        let use_signed = left_is_signed && right_is_signed;
        let op = match op {
            Op::Pow => unreachable!("Pow must be lowered by parse_binary before op_binary"),
            Op::Div => BinaryOp::Div,
            Op::Rem => BinaryOp::Rem,
            Op::Mul => BinaryOp::Mul,
            Op::Add => BinaryOp::Add,
            Op::Sub => BinaryOp::Sub,
            // Logic Shift Left and Arithmetic Shift Left are identical (fill with 0)
            Op::ArithShiftL => BinaryOp::Shl,
            Op::LogicShiftL => BinaryOp::Shl,
            // Right shifts differ in how they fill the MSB
            Op::ArithShiftR => {
                if left_is_signed {
                    BinaryOp::Sar
                } else {
                    BinaryOp::Shr
                }
            }
            Op::LogicShiftR => BinaryOp::Shr,
            Op::Less => {
                if use_signed {
                    BinaryOp::LtS
                } else {
                    BinaryOp::LtU
                }
            }
            Op::LessEq => {
                if use_signed {
                    BinaryOp::LeS
                } else {
                    BinaryOp::LeU
                }
            }
            Op::Greater => {
                if use_signed {
                    BinaryOp::GtS
                } else {
                    BinaryOp::GtU
                }
            }
            Op::GreaterEq => {
                if use_signed {
                    BinaryOp::GeS
                } else {
                    BinaryOp::GeU
                }
            }
            Op::Eq => BinaryOp::Eq,
            Op::EqWildcard => BinaryOp::EqWildcard,
            Op::Ne => BinaryOp::Ne,
            Op::NeWildcard => BinaryOp::NeWildcard,
            Op::LogicAnd => BinaryOp::LogicAnd,
            Op::LogicOr => BinaryOp::LogicOr,
            Op::LogicNot => {
                unreachable!("LogicNot is unary and must not be lowered by op_binary")
            }
            Op::BitAnd => BinaryOp::And,
            Op::BitOr => BinaryOp::Or,
            Op::BitXor => BinaryOp::Xor,
            // BitXnor, BitNand, BitNor are handled above via decomposition
            Op::BitXnor | Op::BitNand | Op::BitNor => unreachable!(),
            Op::BitNot => unreachable!("BitNot is unary and must not be lowered by op_binary"),
            Op::As => unreachable!("As must be lowered by parse_binary before op_binary"),
            Op::Ternary => {
                unreachable!("Ternary expression must be lowered by ternary-specific path")
            }
            Op::Concatenation => {
                unreachable!("Concatenation must be lowered by concat-specific path")
            }
            Op::ArrayLiteral => unreachable!("Array literal must not be lowered by op_binary"),
            Op::Condition => unreachable!("Condition node must not be lowered by op_binary"),
            Op::Repeat => unreachable!("Repeat node must be lowered by repeat-specific path"),
        };
        ir_builder.emit(SIRInstruction::Binary(dest_reg, left, op, right));
        self.stack.push_back(dest_reg);
    }

    pub(super) fn parse_logic_op<A>(
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
        // 1. Evaluate LHS
        self.parse_expression(left, targets, domain, convert, sources, ir_builder, None)?;
        let lhs_reg = self.stack.pop_back().unwrap();

        let rhs_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // SSA register (1-bit) to receive result after merging
        let res_reg = ir_builder.alloc_bit(1, false);
        ir_builder.set_block_params(merge_bb, vec![res_reg]);

        // 2. Prepare value for shortcutting
        // Shortcut with 0 for AND, 1 for OR
        let shortcut_val = if is_and { 0u32 } else { 1u32 };
        let shortcut_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(
            shortcut_reg,
            SIRValue::new(shortcut_val),
        ));

        // 3. Conditional branch
        // AND: If LHS is True, evaluate RHS (rhs_bb). If False, go to Merge with 0.
        // OR : If LHS is False, evaluate RHS (rhs_bb). If True, go to Merge with 1.
        if is_and {
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: lhs_reg,
                true_block: (rhs_bb, vec![]),
                false_block: (merge_bb, vec![shortcut_reg]), // Merge with 0
            });
        } else {
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: lhs_reg,
                true_block: (merge_bb, vec![shortcut_reg]), // Merge with 1
                false_block: (rhs_bb, vec![]),
            });
        }

        // --- RHS Block (RHS evaluation path) ---
        ir_builder.switch_to_block(rhs_bb);
        self.parse_expression(right, targets, domain, convert, sources, ir_builder, None)?;
        let rhs_val = self.stack.pop_back().unwrap();

        // Normalize RHS value to 0/1 (!!rhs_val)
        let tmp_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(tmp_reg, UnaryOp::LogicNot, rhs_val));

        let normalized_rhs_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Unary(
            normalized_rhs_reg,
            UnaryOp::LogicNot,
            tmp_reg,
        ));

        // Merge with evaluation result of RHS
        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![normalized_rhs_reg]));

        // --- Merge Block ---
        ir_builder.switch_to_block(merge_bb);
        self.stack.push_back(res_reg);
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

    pub(super) fn emit_multi_dst_assign<A>(
        &mut self,
        rhs_reg: RegisterId,
        dsts: &[AssignDestination],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut current_offset = 0;
        let rhs_width = ir_builder.register(&rhs_reg).width();

        for dst in dsts.iter().rev() {
            let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select);
            let part_width = access.msb - access.lsb + 1;

            let final_reg = if current_offset == 0 && part_width == rhs_width {
                rhs_reg
            } else {
                let shifted_reg = if current_offset == 0 {
                    rhs_reg
                } else {
                    let shifted_reg = ir_builder.alloc_logic(rhs_width);

                    let shift_amt_reg = ir_builder.alloc_bit(64, false);
                    ir_builder.emit(SIRInstruction::Imm(
                        shift_amt_reg,
                        SIRValue::new(current_offset),
                    ));

                    ir_builder.emit(SIRInstruction::Binary(
                        shifted_reg,
                        rhs_reg,
                        BinaryOp::Shr,
                        shift_amt_reg,
                    ));
                    shifted_reg
                };

                if part_width == rhs_width && current_offset == 0 {
                    shifted_reg
                } else {
                    let mask_val = (BigUint::from(1u64) << part_width) - BigUint::from(1u64);
                    let mask_reg = ir_builder.alloc_bit(part_width, false);
                    ir_builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));

                    let final_reg = ir_builder.alloc_logic(part_width);
                    ir_builder.emit(SIRInstruction::Binary(
                        final_reg,
                        shifted_reg,
                        BinaryOp::And,
                        mask_reg,
                    ));
                    final_reg
                }
            };

            self.stack.push_back(final_reg);
            self.op_store(dst, targets, domain, convert, sources, ir_builder)?;

            current_offset += part_width;
        }
        Ok(())
    }

    pub(super) fn parse_assign_statement<A>(
        &mut self,
        assign_statement: &AssignStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let expected_width: usize = assign_statement
            .dst
            .iter()
            .map(|dst| {
                let access = eval_var_select(self.module, dst.id, &dst.index, &dst.select);
                access.msb - access.lsb + 1
            })
            .sum();

        match &assign_statement.expr {
            Expression::ArrayLiteral(items) => {
                self.parse_array_literal(
                    items,
                    Some(expected_width),
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            Expression::StructConstructor(ty, fields) => {
                self.parse_struct_constructor(
                    ty,
                    fields,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    Some(expected_width),
                )?;
            }
            _ => {
                self.parse_expression(
                    &assign_statement.expr,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    Some(expected_width),
                )?;
            }
        }
        let rhs_reg = self.stack.pop_back().expect("Invalid RHS");
        self.emit_multi_dst_assign(
            rhs_reg,
            &assign_statement.dst,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )
    }

    pub(super) fn op_constant<A>(
        &mut self,
        v: SIRValue,
        width: usize,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let reg = ir_builder.alloc_bit(width, false);

        ir_builder.emit(SIRInstruction::Imm(reg, v));
        self.stack.push_back(reg);
    }

    pub(super) fn parse_factor<A>(
        &mut self,
        factor: &Factor,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        match factor {
            Factor::Variable(var_id, var_index, var_select, _comptime, _token_range) => {
                if let Some(bound_expr) = self.get_bound_function_arg_expr(*var_id) {
                    let bound_expr = bound_expr.clone();
                    if var_index.0.is_empty() && var_select.0.is_empty() && var_select.1.is_none() {
                        self.parse_expression(
                            &bound_expr,
                            targets,
                            domain,
                            convert,
                            sources,
                            ir_builder,
                            context_width,
                        )?;
                        return Ok(());
                    }

                    let Expression::Term(bound_factor) = &bound_expr else {
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function argument indexed access",
                            detail: format!(
                                "non-variable argument expression with indexed access: var_id={:?}",
                                var_id
                            ),
                        });
                    };

                    let Factor::Variable(bound_var_id, bound_var_index, bound_var_select, _, _) =
                        bound_factor.as_ref()
                    else {
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function argument indexed access",
                            detail: format!(
                                "non-variable argument expression with indexed access: var_id={:?}",
                                var_id
                            ),
                        });
                    };

                    if bound_var_select.1.is_some() {
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "function argument indexed access",
                            detail: format!(
                                "chained range access is unsupported: var_id={:?}",
                                var_id
                            ),
                        });
                    }

                    let mut merged_index = bound_var_index.clone();
                    merged_index.append(var_index);

                    let mut merged_select = bound_var_select.clone();
                    merged_select.0.extend(var_select.0.iter().cloned());
                    merged_select.1 = var_select.1.clone();

                    self.op_load(
                        *bound_var_id,
                        &merged_index,
                        &merged_select,
                        domain,
                        convert,
                        sources,
                        ir_builder,
                    )?;
                } else {
                    self.op_load(
                        *var_id, var_index, var_select, domain, convert, sources, ir_builder,
                    )?;
                }
            }
            Factor::Value(comptime, _token_range) => {
                let v = comptime.get_value().unwrap();
                self.op_constant(
                    SIRValue::new(v.payload().into_owned()),
                    v.width(),
                    ir_builder,
                );
            }
            Factor::SystemFunctionCall(call, _) => {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "system function call",
                    detail: format!("{call}"),
                });
            }
            Factor::FunctionCall(call, _) => {
                self.parse_function_call_expr(call, targets, domain, convert, sources, ir_builder)?;
            }
            Factor::Anonymous(_) | Factor::Unresolved(_, _) | Factor::Unknown(_) => {
                unreachable!("Expression factors must be resolved before FF lowering")
            }
        }

        // Apply context_width adjustment
        if let Some(target_width) = context_width {
            let src_reg = self.stack.pop_back().unwrap();
            let src_width = ir_builder.register(&src_reg).width();

            if src_width < target_width {
                // Extension
                let dest_reg = ir_builder.alloc_logic(target_width);
                ir_builder.emit(SIRInstruction::Unary(dest_reg, UnaryOp::Ident, src_reg));
                self.stack.push_back(dest_reg);
            } else if src_width > target_width {
                // Truncation
                let mask_val = (BigUint::from(1u64) << target_width) - BigUint::from(1u64);
                let mask_reg = ir_builder.alloc_bit(target_width, false);
                ir_builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));

                let trunc_reg = ir_builder.alloc_logic(target_width);
                ir_builder.emit(SIRInstruction::Binary(
                    trunc_reg,
                    src_reg,
                    BinaryOp::And,
                    mask_reg,
                ));
                self.stack.push_back(trunc_reg);
            } else {
                self.stack.push_back(src_reg);
            }
        }

        Ok(())
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
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        let parent_expr = Expression::Binary(Box::new(left.clone()), *op, Box::new(right.clone()));
        if matches!(op, Op::LogicAnd) {
            self.parse_logic_op(
                true, left, right, targets, domain, convert, sources, ir_builder,
            )?;
            return Ok(());
        }
        if matches!(op, Op::LogicOr) {
            self.parse_logic_op(
                false, left, right, targets, domain, convert, sources, ir_builder,
            )?;
            return Ok(());
        }

        let (lhs_context_width, rhs_context_width) = if matches!(op, Op::As) {
            // `as` cast: LHS inherits target width from RHS type, RHS is type metadata
            let target_width = if let Expression::Term(f) = right {
                if let Factor::Value(v, _) = f.as_ref() {
                    if let ValueVariant::Type(ty) = &v.value {
                        ty.total_width()
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            (target_width, None)
        } else if matches!(op, Op::LogicAnd | Op::LogicOr | Op::LogicNot) {
            (None, None)
        } else if matches!(
            op,
            Op::Less
                | Op::LessEq
                | Op::Greater
                | Op::GreaterEq
                | Op::Eq
                | Op::Ne
                | Op::EqWildcard
                | Op::NeWildcard
        ) {
            let cw = self
                .get_expression_width(left)
                .max(self.get_expression_width(right));
            (Some(cw), Some(cw))
        } else {
            (
                crate::context_width::get_context_width(&parent_expr, context_width),
                crate::context_width::get_context_width(&parent_expr, context_width),
            )
        };

        if matches!(op, Op::As) {
            self.parse_expression(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                lhs_context_width,
            )?;
            let src = self
                .stack
                .pop_back()
                .expect("Invalid cast source expression");

            let Some((target_width, target_signed, target_is_2state)) =
                self.get_cast_target_info(right)
            else {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "as cast target",
                    detail: format!("{:?}", right),
                });
            };

            let casted = if target_is_2state {
                ir_builder.alloc_bit(target_width, target_signed)
            } else {
                ir_builder.alloc_logic(target_width)
            };
            ir_builder.emit(SIRInstruction::Unary(casted, UnaryOp::Ident, src));
            self.stack.push_back(casted);
            return Ok(());
        }

        if matches!(op, Op::Pow) {
            let Some(exp) = self.get_constant_value(right) else {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "pow non-constant exponent",
                    detail: format!("{:?}", right),
                });
            };

            let width = self.get_expression_width(left);
            self.parse_expression(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                lhs_context_width,
            )?;
            let base = self
                .stack
                .pop_back()
                .expect("Invalid LHS for power operation");

            let result = if exp == 0 {
                let one = ir_builder.alloc_bit(width, false);
                ir_builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u32)));
                one
            } else {
                let mut acc = base;
                for _ in 1..exp {
                    let next = ir_builder.alloc_logic(width);
                    ir_builder.emit(SIRInstruction::Binary(next, acc, BinaryOp::Mul, base));
                    acc = next;
                }
                acc
            };

            self.stack.push_back(result);
            return Ok(());
        }

        let width = self
            .get_expression_width(left)
            .max(self.get_expression_width(right));
        if let Some(w) = context_width {
            let width = width.max(w);
            self.parse_expression(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                lhs_context_width,
            )?;
            self.parse_expression(
                right,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                rhs_context_width,
            )?;
            self.op_binary(op, width, ir_builder);
        } else {
            self.parse_expression(
                left,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                lhs_context_width,
            )?;
            self.parse_expression(
                right,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                rhs_context_width,
            )?;
            self.op_binary(op, width, ir_builder);
        }
        Ok(())
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
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        let width = self.get_expression_width(expr);
        // Reduction and logical-not operators reduce a multi-bit operand to 1 bit.
        // The operand must be evaluated at its own natural width, not the (narrower)
        // context width of the result — otherwise the input gets truncated before
        // the reduction is applied.
        let operand_context = match op {
            Op::BitAnd
            | Op::BitOr
            | Op::BitXor
            | Op::BitNand
            | Op::BitNor
            | Op::BitXnor
            | Op::LogicNot => None,
            _ => context_width,
        };
        self.parse_expression(
            expr,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            operand_context,
        )?;
        self.op_unary(op, width, ir_builder);
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
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        // 1. Parse condition expression
        self.parse_expression(cond, targets, domain, convert, sources, ir_builder, None)?;
        let cond_reg = self.stack.pop_back().unwrap();

        // 2. Reserve blocks
        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block(); // Don't add params yet

        // Terminate current BB with branch
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // --- Then Block ---
        ir_builder.switch_to_block(then_bb);
        self.parse_expression(
            then,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            context_width,
        )?;
        let then_val = self.stack.pop_back().unwrap();
        let then_width = ir_builder.register(&then_val).width();
        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![then_val]));

        // --- Else Block ---
        ir_builder.switch_to_block(else_bb);
        self.parse_expression(
            els,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            context_width,
        )?;
        let else_val = self.stack.pop_back().unwrap();
        let else_width = ir_builder.register(&else_val).width();
        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![else_val]));

        // 3. Determine width and allocate res_reg
        // Use the larger width of Then and Else
        let res_width = then_width.max(else_width);
        let res_reg = ir_builder.alloc_logic(res_width);

        // 4. Set parameters to merge_bb (inject params later)
        // * Assuming SIRBuilder has a method for this
        ir_builder.set_block_params(merge_bb, vec![res_reg]);

        // 5. Merge
        self.stack.push_back(res_reg);
        ir_builder.switch_to_block(merge_bb);
        Ok(())
    }

    pub(super) fn parse_concatenation<A>(
        &mut self,
        exprs: &Vec<(Expression, Option<Expression>)>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,

        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let mut total_width = 0;

        // Create accumulator with initial value 0
        let mut acc_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(acc_reg, SIRValue::new(0u32)));

        // Parse sequentially from right (LSB)
        for (expr, replication) in exprs.iter().rev() {
            // 1. Evaluate expression to be repeated
            self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
            let part_reg = self
                .stack
                .pop_back()
                .expect("Concatenation part evaluation failed");
            let part_width = ir_builder.register(&part_reg).width();

            // 2. Get replication count (1 if not specified)
            let rep_count = if let Some(rep_expr) = replication {
                use crate::parser::bitaccess::eval_constexpr;
                let v = eval_constexpr(rep_expr);
                v.unwrap().iter_u64_digits().next().unwrap()
            } else {
                1
            };

            // 3. Repeat packing for the specified number of times
            for _ in 0..rep_count {
                let next_total_width = total_width + part_width;

                // Generate left shift amount

                let shift_amt_reg = ir_builder.alloc_bit(64, false);
                ir_builder.emit(SIRInstruction::Imm(
                    shift_amt_reg,
                    SIRValue::new(total_width),
                ));

                // Shift target to current position
                let shifted_part_reg = ir_builder.alloc_logic(next_total_width);
                ir_builder.emit(SIRInstruction::Binary(
                    shifted_part_reg,
                    part_reg,
                    BinaryOp::Shl,
                    shift_amt_reg,
                ));

                // Integrate into accumulator
                let next_acc_reg = ir_builder.alloc_logic(next_total_width);
                ir_builder.emit(SIRInstruction::Binary(
                    next_acc_reg,
                    acc_reg,
                    BinaryOp::Or,
                    shifted_part_reg,
                ));

                // Update state
                acc_reg = next_acc_reg;
                total_width = next_total_width;
            }
        }

        // Push final result to stack
        self.stack.push_back(acc_reg);
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
        if parts.len() == 1 {
            return parts[0].0;
        }

        let mut total_width = 0usize;
        let mut acc_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Imm(acc_reg, SIRValue::new(0u32)));

        for (part_reg, part_width) in parts.iter().rev() {
            let next_total_width = total_width + *part_width;

            let shift_amt_reg = ir_builder.alloc_bit(64, false);
            ir_builder.emit(SIRInstruction::Imm(
                shift_amt_reg,
                SIRValue::new(total_width),
            ));

            let shifted_part_reg = ir_builder.alloc_logic(next_total_width);
            ir_builder.emit(SIRInstruction::Binary(
                shifted_part_reg,
                *part_reg,
                BinaryOp::Shl,
                shift_amt_reg,
            ));

            let next_acc_reg = ir_builder.alloc_logic(next_total_width);
            ir_builder.emit(SIRInstruction::Binary(
                next_acc_reg,
                acc_reg,
                BinaryOp::Or,
                shifted_part_reg,
            ));

            acc_reg = next_acc_reg;
            total_width = next_total_width;
        }

        acc_reg
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
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        let mut parts: Vec<(RegisterId, usize)> = Vec::new();

        for (name, expr) in fields {
            self.parse_expression(
                expr,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
                context_width,
            )?;
            let mut reg = self
                .stack
                .pop_back()
                .expect("Struct constructor part evaluation failed");
            let src_width = ir_builder.register(&reg).width();

            let Some(member_type) = ty.get_member_type(*name) else {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "struct constructor member",
                    detail: format!("unknown member: {:?} in {:?}", name, ty),
                });
            };
            let Some(member_width) = member_type.total_width() else {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "struct constructor member width",
                    detail: format!("member: {:?}, type: {:?}", name, member_type),
                });
            };

            if src_width > member_width {
                let mask_val = (BigUint::from(1u64) << member_width) - BigUint::from(1u64);
                let mask_reg = ir_builder.alloc_bit(member_width, false);
                ir_builder.emit(SIRInstruction::Imm(mask_reg, SIRValue::new(mask_val)));

                let trunc_reg = ir_builder.alloc_logic(member_width);
                ir_builder.emit(SIRInstruction::Binary(
                    trunc_reg,
                    reg,
                    BinaryOp::And,
                    mask_reg,
                ));
                reg = trunc_reg;
            } else if src_width < member_width {
                let pad_width = member_width - src_width;
                let zero_reg = ir_builder.alloc_bit(pad_width, false);
                ir_builder.emit(SIRInstruction::Imm(zero_reg, SIRValue::new(0u32)));
                reg = self
                    .emit_concat_registers(&[(zero_reg, pad_width), (reg, src_width)], ir_builder);
            }

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
                            ParserError::UnsupportedFFLowering {
                                feature: "array literal non-constant repeat",
                                detail: format!("{:?}", rep_expr),
                            }
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
                        return Err(ParserError::UnsupportedFFLowering {
                            feature: "array literal multiple default",
                            detail: format!("{:?}", items),
                        });
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
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "array literal default without context width",
                    detail: format!("{:?}", items),
                });
            };

            if explicit_width > target_width {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "array literal width overflow",
                    detail: format!("explicit_width={explicit_width}, target_width={target_width}"),
                });
            }

            let remaining = target_width - explicit_width;
            if default_width == 0 || !remaining.is_multiple_of(default_width) {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "array literal default width mismatch",
                    detail: format!(
                        "remaining={remaining}, default_width={default_width}, target_width={target_width}"
                    ),
                });
            }

            for _ in 0..(remaining / default_width) {
                parts.push((default_reg, default_width));
            }
        }

        let reg = self.emit_concat_registers(&parts, ir_builder);
        self.stack.push_back(reg);
        Ok(())
    }

    pub(super) fn parse_expression<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
        context_width: Option<usize>,
    ) -> Result<(), ParserError> {
        match expr {
            Expression::Term(factor) => {
                self.parse_factor(
                    factor,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    context_width,
                )?;
            }
            Expression::Binary(left, op, right) => {
                self.parse_binary(
                    op,
                    left,
                    right,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    context_width,
                )?;
            }
            Expression::Unary(op, expr) => {
                self.parse_unary(
                    op,
                    expr,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    context_width,
                )?;
            }
            Expression::Ternary(cond, then, els) => {
                self.parse_ternary(
                    cond,
                    then,
                    els,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    context_width,
                )?;
            }
            Expression::Concatenation(exprs) => {
                self.parse_concatenation(exprs, targets, domain, convert, sources, ir_builder)?;
            }
            Expression::ArrayLiteral(items) => {
                self.parse_array_literal(
                    items,
                    context_width,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            Expression::StructConstructor(ty, fields) => {
                self.parse_struct_constructor(
                    ty,
                    fields,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                    context_width,
                )?;
            }
        }
        Ok(())
    }
}
