//! `if_reset` statements.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn parse_if_reset_statement<A>(
        &mut self,
        stmt: &IfResetStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        let true_side: Vec<&Statement> = stmt.true_side.iter().collect();
        let false_side: Vec<&Statement> = stmt.false_side.iter().collect();
        self.parse_if_reset_internal(
            &true_side,
            &false_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )
    }

    pub(super) fn parse_if_reset_internal<A>(
        &mut self,
        true_side: &[&Statement],
        false_side: &[&Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        // 1. Load reset signal (used as condition expression)
        let (reset_id, reset_index, reset_select, is_low) = {
            let reset = self
                .reset
                .as_ref()
                .expect("if_reset used without reset signal in FfDeclaration");
            let var = &self.module.variables[&reset.id];
            let is_low = match var.r#type.kind {
                TypeKind::ResetAsyncLow | TypeKind::ResetSyncLow => true,
                TypeKind::Reset => matches!(
                    self.config.reset_type,
                    veryl_metadata::ResetType::AsyncLow | veryl_metadata::ResetType::SyncLow
                ),
                _ => false,
            };
            (reset.id, reset.index.clone(), reset.select.clone(), is_low)
        };

        self.op_load(
            reset_id,
            &reset_index,
            &reset_select,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let mut cond_reg = self.stack.pop_back().unwrap();

        // 1.1 Handle reset polarity (Invert if Low-Active)
        if is_low {
            let inverted_reg = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Unary(
                inverted_reg,
                UnaryOp::LogicNot,
                cond_reg,
            ));
            cond_reg = inverted_reg;
        }
        cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // --- Create snapshot ---
        let pre_if_defined = self.defined_ranges.clone();
        let pre_if_dynamic = self.dynamic_defined_vars.clone();

        // 2. Terminate current block with Branch
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // 3. Then Path (Reset active)
        ir_builder.switch_to_block(then_bb);
        let then_flow =
            self.parse_statement_refs(true_side, targets, domain, convert, sources, ir_builder)?;
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic);
        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 4. Else Path (Normal operation)
        ir_builder.switch_to_block(else_bb);
        let else_flow =
            self.parse_statement_refs(false_side, targets, domain, convert, sources, ir_builder)?;
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 5. Merge logic (Intersection of defined states of both paths)
        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
                self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined_ranges = then_defined;
                self.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined_ranges = else_defined;
                self.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }
}
