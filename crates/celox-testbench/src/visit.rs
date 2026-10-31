//! Compiler traversal of expressions in a semantic testbench.
use crate::*;
impl<A> TestbenchProgram<A> {
    pub fn try_visit_expressions<E>(
        &mut self,
        mut visit: impl FnMut(&mut ExprBytecode<StateLocation<A>>) -> Result<(), E>,
    ) -> Result<(), E> {
        fn target<A, E>(
            value: &mut TestbenchTarget<SemanticSignal<A>, ExprBytecode<StateLocation<A>>>,
            visit: &mut impl FnMut(&mut ExprBytecode<StateLocation<A>>) -> Result<(), E>,
        ) -> Result<(), E> {
            if let Some(selection) = &mut value.selection {
                visit(&mut selection.offset)?;
            }
            Ok(())
        }
        fn message<A, E>(
            value: &mut Option<AssertMessage<SemanticArgument<A>>>,
            visit: &mut impl FnMut(&mut ExprBytecode<StateLocation<A>>) -> Result<(), E>,
        ) -> Result<(), E> {
            if let Some(AssertMessage::Formatted { args, .. } | AssertMessage::DynamicArgs(args)) =
                value
            {
                for argument in args {
                    visit(&mut argument.expr)?;
                }
            }
            Ok(())
        }
        fn block<A, E>(
            statements: &mut [SemanticStatement<A>],
            visit: &mut impl FnMut(&mut ExprBytecode<StateLocation<A>>) -> Result<(), E>,
        ) -> Result<(), E> {
            for statement in statements {
                match statement {
                    TestbenchStatement::ClockNext { count, .. }
                    | TestbenchStatement::ResetAssert {
                        duration: count, ..
                    } => {
                        if let ClockCount::Dynamic(expr) = count {
                            visit(expr)?;
                        }
                    }
                    TestbenchStatement::Assert {
                        expr,
                        message: value,
                        ..
                    } => {
                        visit(expr)?;
                        message(value, visit)?;
                    }
                    TestbenchStatement::Display { message: value, .. } => message(value, visit)?,
                    TestbenchStatement::If {
                        expr,
                        then_block,
                        else_block,
                    } => {
                        visit(expr)?;
                        block(then_block, visit)?;
                        block(else_block, visit)?;
                    }
                    TestbenchStatement::For {
                        start, end, body, ..
                    } => {
                        for bound in [start, end] {
                            if let LoopBound::Dynamic { expr, .. } = bound {
                                visit(expr)?;
                            }
                        }
                        block(body, visit)?;
                    }
                    TestbenchStatement::Assign { dst, expr } => {
                        visit(expr)?;
                        target(dst, visit)?;
                    }
                    TestbenchStatement::RandomSeed { value, .. } => visit(value)?,
                    TestbenchStatement::RandomGet { ret, .. }
                    | TestbenchStatement::RandomGetSeed { ret, .. } => {
                        if let Some(ret) = ret {
                            target(ret, visit)?;
                        }
                    }
                    TestbenchStatement::RandomGetRange { min, max, ret, .. } => {
                        visit(min)?;
                        visit(max)?;
                        if let Some(ret) = ret {
                            target(ret, visit)?;
                        }
                    }
                    TestbenchStatement::ComponentMethod { args, ret, .. } => {
                        for arg in args {
                            visit(&mut arg.expr)?;
                        }
                        if let Some(ret) = ret {
                            target(ret, visit)?;
                        }
                    }
                    TestbenchStatement::Break
                    | TestbenchStatement::Finish
                    | TestbenchStatement::WriteMemory { .. } => {}
                }
            }
            Ok(())
        }
        block(&mut self.statements, &mut visit)?;
        for binding in &mut self.component_bindings {
            for connection in &mut binding.connections {
                if let Some(input) = &mut connection.input {
                    visit(input)?;
                }
                if let Some(input) = &mut connection.input_target {
                    target(input, &mut visit)?;
                }
                if let Some(output) = &mut connection.output {
                    target(output, &mut visit)?;
                }
            }
        }
        Ok(())
    }
}

impl<A: Copy> TestbenchProgram<A> {
    /// Fuse pure constant clock waits before assigning native request sites.
    pub fn fold_clock_waits(&mut self) {
        fn constant<A>(expr: &ExprBytecode<StateLocation<A>>) -> Option<u64> {
            expr.kernel()
                .and_then(|kernel| kernel.constant)
                .or_else(|| expr.literal_u64())
        }
        fn bound<A>(bound: &LoopBound<ExprBytecode<StateLocation<A>>>) -> Option<u64> {
            match bound {
                LoopBound::Static(value) => Some(*value as u64),
                LoopBound::Dynamic {
                    expr,
                    width,
                    signed,
                } if *width <= 64 => {
                    let value = constant(expr)?;
                    (!*signed || (*width != 0 && value >> (width - 1) == 0)).then_some(value)
                }
                _ => None,
            }
        }
        fn block<A: Copy>(statements: &mut [SemanticStatement<A>]) {
            for statement in statements {
                match statement {
                    TestbenchStatement::If {
                        then_block,
                        else_block,
                        ..
                    } => {
                        block(then_block);
                        block(else_block);
                    }
                    TestbenchStatement::For {
                        loop_var,
                        start,
                        end,
                        inclusive,
                        step,
                        step_op,
                        reverse,
                        body,
                    } => {
                        block(body);
                        if !*inclusive
                            && !*reverse
                            && *step == 1
                            && step_op.is_none()
                            && let (Some(0), Some(iterations)) = (bound(start), bound(end))
                            && let [
                                TestbenchStatement::ClockNext {
                                    clock_event,
                                    count,
                                    period,
                                },
                            ] = body.as_slice()
                            && let Some(count) = match count {
                                ClockCount::Static(count) => Some(*count),
                                ClockCount::Dynamic(expr) => constant(expr),
                            }
                            && count != 0
                            && iterations != 0
                            && loop_var.as_ref().is_none_or(|(_, width, signed)| {
                                let bits = width.saturating_sub(usize::from(*signed));
                                bits >= 64 || iterations < (1u64 << bits)
                            })
                            && let Some(count) = iterations.checked_mul(count)
                        {
                            *statement = TestbenchStatement::ClockNext {
                                clock_event: *clock_event,
                                count: ClockCount::Static(count),
                                period: *period,
                            };
                        }
                    }
                    _ => {}
                }
            }
        }
        block(&mut self.statements);
    }
}
