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
                    TestbenchStatement::Queue {
                        value,
                        value_target,
                        ret,
                        ..
                    } => {
                        if let Some(value) = value {
                            visit(value)?;
                        }
                        if let Some(value_target) = value_target {
                            target(value_target, visit)?;
                        }
                        if let Some(ret) = ret {
                            target(ret, visit)?;
                        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_program() -> TestbenchProgram<usize> {
        let expr = |value| ExprBytecode::new(vec![ExprOpcode::ConstU64(value)]);
        let target = |offset| TestbenchTarget {
            signal: SemanticSignal {
                address: 0,
                width: 32,
            },
            selection: Some(TestbenchSelection {
                offset: expr(offset),
                width: 8,
            }),
            width: 8,
        };
        TestbenchProgram::new(vec![TestbenchStatement::Queue {
            handle: "queue".into(),
            op: QueueOp::PushBack,
            value: Some(expr(1)),
            value_target: Some(target(2)),
            value_mask: BigUint::default(),
            value_width: 8,
            value_signed: false,
            width: 8,
            signed: false,
            four_state: true,
            capacity: 8,
            ret: Some(target(3)),
        }])
    }

    #[test]
    fn queue_values_and_both_target_offsets_are_rewritten() {
        let mut program = queue_program();
        program
            .try_visit_expressions::<()>(|expr| {
                *expr =
                    ExprBytecode::new(vec![ExprOpcode::ConstU64(expr.literal_u64().unwrap() + 10)]);
                Ok(())
            })
            .unwrap();
        let mut rewritten = Vec::new();
        program
            .try_visit_expressions::<()>(|expr| {
                rewritten.push(expr.literal_u64().unwrap());
                Ok(())
            })
            .unwrap();
        assert_eq!(rewritten, [11, 12, 13]);
    }

    #[test]
    fn queue_target_visit_errors_stop_traversal() {
        let mut visited = Vec::new();
        let result = queue_program().try_visit_expressions(|expr| {
            let value = expr.literal_u64().unwrap();
            visited.push(value);
            if value == 2 {
                Err("target error")
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err("target error"));
        assert_eq!(visited, [1, 2]);
    }
}
