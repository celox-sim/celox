//! `if` and `case` statements: procedural conditions and the merge of definedness at joins.

use super::*;

/// Which bits the statements lowered so far have assigned on every path that reaches the
/// current block. Saved before a branch and merged or restored at its join.
pub(super) struct Definedness {
    pub(super) defined_ranges: HashMap<VarId, BitSet>,
    pub(super) dynamic_defined_vars: HashSet<VarId>,
}

impl<'a> FfParser<'a> {
    pub(super) fn get_constant_procedural_truth(expr: &Expression) -> Option<bool> {
        let comptime = expr.comptime();
        let is_value = matches!(expr, Expression::Term(factor) if matches!(factor.as_ref(), veryl_analyzer::ir::Factor::Value(_)));
        if !(comptime.is_const || is_value && comptime.evaluated) {
            return None;
        }

        if let Some((value, mask, width, _)) = celox_value_from_comptime_in_context(comptime, None)
        {
            let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let known = &width_mask ^ (&mask & &width_mask);
            return Some(!(value & known).is_zero());
        }

        comptime
            .r#type
            .is_2state()
            .then(|| eval_constexpr(expr).map(|value| !value.is_zero()))
            .flatten()
    }

    pub(super) fn lower_procedural_condition<A>(
        &self,
        condition: RegisterId,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        if matches!(
            ir_builder.register(&condition),
            RegisterType::Bit {
                width: 1,
                signed: false
            }
        ) {
            return condition;
        }

        if matches!(
            ir_builder.register(&condition),
            RegisterType::Logic { width: 1 }
        ) {
            let known_truth = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Unary(
                known_truth,
                UnaryOp::ToTwoState,
                condition,
            ));
            return known_truth;
        }

        let source_is_two_state =
            matches!(ir_builder.register(&condition), RegisterType::Bit { .. });
        let truth = if source_is_two_state {
            ir_builder.alloc_bit(1, false)
        } else {
            ir_builder.alloc_logic(1)
        };
        ir_builder.emit(SIRInstruction::Unary(truth, UnaryOp::Or, condition));
        if source_is_two_state {
            truth
        } else {
            let known_truth = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Unary(
                known_truth,
                UnaryOp::ToTwoState,
                truth,
            ));
            known_truth
        }
    }

    pub(super) fn parse_if_statement<A>(
        &mut self,
        stmt: &IfStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        // Constant folding: if condition is compile-time constant, inline the appropriate side
        if let Some(cond_is_true) = Self::get_constant_procedural_truth(&stmt.cond) {
            let side = if cond_is_true {
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
        let cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // --- Create snapshot ---
        // Save both static (BitSet) and dynamic (HashSet) states
        let pre_if_defined = self.defined.defined_ranges.clone();
        let pre_if_dynamic = self.defined.dynamic_defined_vars.clone(); // 【追加】

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
        let then_defined =
            std::mem::replace(&mut self.defined.defined_ranges, pre_if_defined.clone());
        let then_dynamic =
            std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_if_dynamic); // 【追加】

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
        let else_defined = std::mem::take(&mut self.defined.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.defined.dynamic_defined_vars); // 【追加】

        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 5. Merge logic
        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined.defined_ranges =
                    self.intersect_defined_states(then_defined, else_defined);
                self.defined.dynamic_defined_vars =
                    self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined.defined_ranges = then_defined;
                self.defined.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined.defined_ranges = else_defined;
                self.defined.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

    pub(super) fn parse_case_statement<A>(
        &mut self,
        stmt: &CaseStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        self.parse_case_arm(stmt, 0, targets, domain, convert, sources, ir_builder)
    }

    pub(super) fn parse_case_arm<A>(
        &mut self,
        stmt: &CaseStatement,
        arm_index: usize,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        let Some(arm) = stmt.arms.get(arm_index) else {
            return self.parse_statement_list(
                &stmt.default,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            );
        };

        let cond = case_arm_condition_expr(&stmt.case_target, &arm.patterns);
        if let Some(cond_is_true) = Self::get_constant_procedural_truth(&cond) {
            return if cond_is_true {
                self.parse_statement_list(&arm.body, targets, domain, convert, sources, ir_builder)
            } else {
                self.parse_case_arm(
                    stmt,
                    arm_index + 1,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )
            };
        }

        self.parse_expression(&cond, targets, domain, convert, sources, ir_builder, None)?;
        let cond_reg = self.stack.pop_back().unwrap();
        let cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        let pre_case_defined = self.defined.defined_ranges.clone();
        let pre_case_dynamic = self.defined.dynamic_defined_vars.clone();

        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        ir_builder.switch_to_block(then_bb);
        let then_flow =
            self.parse_statement_list(&arm.body, targets, domain, convert, sources, ir_builder)?;
        let then_defined =
            std::mem::replace(&mut self.defined.defined_ranges, pre_case_defined.clone());
        let then_dynamic =
            std::mem::replace(&mut self.defined.dynamic_defined_vars, pre_case_dynamic);
        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        ir_builder.switch_to_block(else_bb);
        let else_flow = self.parse_case_arm(
            stmt,
            arm_index + 1,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let else_defined = std::mem::take(&mut self.defined.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.defined.dynamic_defined_vars);
        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined.defined_ranges =
                    self.intersect_defined_states(then_defined, else_defined);
                self.defined.dynamic_defined_vars =
                    self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined.defined_ranges = then_defined;
                self.defined.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined.defined_ranges = else_defined;
                self.defined.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

    /// Helper to take intersection of dynamic defined variables
    pub(super) fn intersect_dynamic_vars(
        &self,
        mut left: HashSet<VarId>,
        right: HashSet<VarId>,
    ) -> HashSet<VarId> {
        left.retain(|var_id| right.contains(var_id));
        left
    }

    /// Helper to take intersection of defined states of two paths
    pub(super) fn intersect_defined_states(
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
}
