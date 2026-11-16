//! Statements: lists, dispatch and assignments.

use super::*;

impl<'a> FfParser<'a> {
    pub(super) fn parse_statement_list<A>(
        &mut self,
        stmts: &[Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        for stmt in stmts {
            let flow = self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            if matches!(flow, ControlFlow::Break) {
                return Ok(ControlFlow::Break);
            }
        }
        Ok(ControlFlow::Continue)
    }

    pub(super) fn parse_statement_refs<A>(
        &mut self,
        stmts: &[&Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        for stmt in stmts {
            let flow = self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            if matches!(flow, ControlFlow::Break) {
                return Ok(ControlFlow::Break);
            }
        }
        Ok(ControlFlow::Continue)
    }

    pub(super) fn parse_statement<A>(
        &mut self,
        stmt: &Statement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        match stmt {
            Statement::Assign(assign_statement) => {
                self.parse_assign_statement(
                    assign_statement,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            Statement::If(stmt) => {
                return self
                    .parse_if_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::Case(stmt) => {
                return self
                    .parse_case_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::IfReset(stmt) => {
                return self
                    .parse_if_reset_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::Null => {}
            Statement::SystemFunctionCall(call) => {
                return self.parse_system_task_statement(
                    call, targets, domain, convert, sources, ir_builder,
                );
            }
            Statement::FunctionCall(call) => {
                self.parse_function_call_statement(
                    call, targets, domain, convert, sources, ir_builder,
                )?;
            }
            Statement::For(f) => {
                self.parse_for_statement(f, targets, domain, convert, sources, ir_builder)?;
            }
            Statement::Break => {
                let Some(exit_bb) = self.loop_exit_blocks.last().copied() else {
                    return Err(ParserError::illegal_context(
                        "statement in always_ff",
                        "break outside loop".to_string(),
                        None,
                    ));
                };
                ir_builder.seal_block(SIRTerminator::Jump(exit_bb, vec![]));
                return Ok(ControlFlow::Break);
            }
            Statement::TbMethodCall(_) | Statement::Unsupported(_) => {
                return Err(ParserError::illegal_context(
                    "statement in always_ff",
                    format!("{stmt}"),
                    None,
                ));
            }
        }
        Ok(ControlFlow::Continue)
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
            let part_width = get_access_width(self.module, dst.id, &dst.index, &dst.select)?;

            let final_reg = if current_offset == 0 && part_width == rhs_width {
                rhs_reg
            } else {
                // Splitting an assignment copies bits, including X/Z, rather
                // than applying a logical mask that changes Z into X.
                let final_reg = ir_builder.alloc_logic(part_width);
                ir_builder.emit(SIRInstruction::Slice(
                    final_reg,
                    rhs_reg,
                    current_offset,
                    part_width,
                ));
                final_reg
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
            .map(|dst| get_access_width(self.module, dst.id, &dst.index, &dst.select))
            .sum::<Result<usize, ParserError>>()?;

        let expression = assign_statement
            .dst
            .first()
            .map(|dst| crate::float::converted(&assign_statement.expr, &dst.comptime.r#type))
            .unwrap_or(std::borrow::Cow::Borrowed(&assign_statement.expr));
        self.parse_expression(
            &expression,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
            Some(expected_width),
        )?;
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
}
