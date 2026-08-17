use celox_design::{BitAccess, StateAddr as AbsoluteAddr, VarAtomBase};
use celox_testbench::{
    AssertMessage as GenericAssertMessage, ClockCount as GenericClockCount, CompiledExpr,
    ExecutableArgument, ExecutableAssertMessage, ExecutableClockCount, ExecutableLoopBound,
    ExecutableStatement, ExecutableTestbench, ExprBytecode, LoopBound as GenericLoopBound,
    SemanticArgument, SemanticComponentBinding, SemanticStatement, StateLocation, TestbenchProgram,
    TestbenchSelection, TestbenchStatement as GenericTestbenchStatement, TestbenchTarget,
};

use crate::{SignalRef, backend::SimBackend};

fn bind_expr<B: SimBackend>(
    backend: &B,
    expr: ExprBytecode<StateLocation<AbsoluteAddr>>,
) -> Option<CompiledExpr> {
    let layout = backend.layout();
    let bytecode = expr
        .bind_with(|address| layout.offsets.get(address).copied())
        .ok()?;
    Some(CompiledExpr::new(bytecode))
}

fn bind_component<B: SimBackend>(
    backend: &B,
    component: SemanticComponentBinding<AbsoluteAddr>,
    rtl_writes: &fxhash::FxHashSet<VarAtomBase<AbsoluteAddr>>,
) -> Option<celox_testbench::ExecutableComponentBinding<B::Event, SignalRef>> {
    Some(celox_testbench::ComponentBinding {
        instance: component.instance,
        connections: component
            .connections
            .into_iter()
            .map(|connection| {
                let output = match connection.output {
                    Some(output) => Some(bind_target(backend, output)?),
                    None => None,
                };
                let output_rtl_driven = output.as_ref().is_some_and(|output| {
                    let target_access = match &output.selection {
                        Some(selection) => selection
                            .offset
                            .constant_u64()
                            .and_then(|offset| usize::try_from(offset).ok())
                            .and_then(|lsb| {
                                output
                                    .width
                                    .checked_sub(1)
                                    .and_then(|tail| lsb.checked_add(tail))
                                    .map(|msb| BitAccess::new(lsb, msb))
                            }),
                        None => output
                            .signal
                            .width
                            .checked_sub(1)
                            .map(|msb| BitAccess::new(0, msb)),
                    };
                    rtl_writes.iter().any(|write| {
                        backend.resolve_signal(&write.id) == output.signal
                            && target_access.is_none_or(|target| target.overlaps(&write.access))
                    })
                });
                Some(celox_testbench::ComponentConnectionBinding {
                    port: connection.port,
                    input: match connection.input {
                        Some(input) => Some(bind_expr(backend, input)?),
                        None => None,
                    },
                    input_target: match connection.input_target {
                        Some(input) => Some(bind_target(backend, input)?),
                        None => None,
                    },
                    output,
                    output_rtl_driven,
                    event: connection
                        .event
                        .and_then(|event| backend.resolve_event_opt(&event)),
                })
            })
            .collect::<Option<Vec<_>>>()?,
    })
}

fn bind_assert_arg<B: SimBackend>(
    backend: &B,
    arg: SemanticArgument<AbsoluteAddr>,
) -> Option<ExecutableArgument> {
    Some(ExecutableArgument {
        expr: bind_expr(backend, arg.expr)?,
        width: arg.width,
        signed: arg.signed,
        is_string: arg.is_string,
    })
}

fn bind_assert_message<B: SimBackend>(
    backend: &B,
    message: GenericAssertMessage<SemanticArgument<AbsoluteAddr>>,
) -> Option<ExecutableAssertMessage> {
    match message {
        GenericAssertMessage::Formatted { template, args } => {
            let args = args
                .into_iter()
                .map(|arg| bind_assert_arg(backend, arg))
                .collect::<Option<Vec<_>>>()?;
            Some(GenericAssertMessage::Formatted { template, args })
        }
        GenericAssertMessage::DynamicArgs(args) => {
            let args = args
                .into_iter()
                .map(|arg| bind_assert_arg(backend, arg))
                .collect::<Option<Vec<_>>>()?;
            Some(GenericAssertMessage::DynamicArgs(args))
        }
    }
}

fn bind_clock_count<B: SimBackend>(
    backend: &B,
    count: GenericClockCount<ExprBytecode<StateLocation<AbsoluteAddr>>>,
) -> Option<ExecutableClockCount> {
    match count {
        GenericClockCount::Static(count) => Some(GenericClockCount::Static(count)),
        GenericClockCount::Dynamic(expr) => {
            Some(GenericClockCount::Dynamic(bind_expr(backend, expr)?))
        }
    }
}

fn bind_loop_bound<B: SimBackend>(
    backend: &B,
    bound: GenericLoopBound<ExprBytecode<StateLocation<AbsoluteAddr>>>,
) -> Option<ExecutableLoopBound> {
    match bound {
        GenericLoopBound::Static(bound) => Some(GenericLoopBound::Static(bound)),
        GenericLoopBound::Dynamic {
            expr,
            width,
            signed,
        } => Some(GenericLoopBound::Dynamic {
            expr: bind_expr(backend, expr)?,
            width,
            signed,
        }),
    }
}

fn bind_target<B: SimBackend>(
    backend: &B,
    target: TestbenchTarget<
        celox_testbench::SemanticSignal<AbsoluteAddr>,
        ExprBytecode<StateLocation<AbsoluteAddr>>,
    >,
) -> Option<TestbenchTarget<SignalRef, CompiledExpr>> {
    Some(TestbenchTarget {
        signal: backend.resolve_signal(&target.signal.address),
        selection: match target.selection {
            Some(selection) => Some(TestbenchSelection {
                offset: bind_expr(backend, selection.offset)?,
                width: selection.width,
            }),
            None => None,
        },
        width: target.width,
    })
}

fn bind_optional_target<B: SimBackend>(
    backend: &B,
    target: Option<
        TestbenchTarget<
            celox_testbench::SemanticSignal<AbsoluteAddr>,
            ExprBytecode<StateLocation<AbsoluteAddr>>,
        >,
    >,
) -> Option<Option<TestbenchTarget<SignalRef, CompiledExpr>>> {
    match target {
        Some(target) => Some(Some(bind_target(backend, target)?)),
        None => Some(None),
    }
}

fn bind_statement<B: SimBackend>(
    backend: &B,
    statement: SemanticStatement<AbsoluteAddr>,
) -> Option<ExecutableStatement<B::Event, SignalRef>> {
    match statement {
        GenericTestbenchStatement::ClockNext { clock_event, count } => {
            Some(GenericTestbenchStatement::ClockNext {
                clock_event: backend.resolve_event_opt(&clock_event)?,
                count: bind_clock_count(backend, count)?,
            })
        }
        GenericTestbenchStatement::ResetAssert {
            reset_signal,
            reset_event,
            clock_event,
            duration,
            assert_value,
            deassert_value,
        } => Some(GenericTestbenchStatement::ResetAssert {
            reset_signal: backend.resolve_signal(&reset_signal.address),
            reset_event: reset_event.and_then(|event| backend.resolve_event_opt(&event)),
            clock_event: backend.resolve_event_opt(&clock_event)?,
            duration: bind_clock_count(backend, duration)?,
            assert_value,
            deassert_value,
        }),
        GenericTestbenchStatement::Assert {
            expr,
            site_id,
            continue_on_fail,
            message,
            location,
        } => Some(GenericTestbenchStatement::Assert {
            expr: bind_expr(backend, expr)?,
            site_id,
            continue_on_fail,
            message: match message {
                Some(message) => Some(bind_assert_message(backend, message)?),
                None => None,
            },
            location,
        }),
        GenericTestbenchStatement::Display { message, newline } => {
            Some(GenericTestbenchStatement::Display {
                message: match message {
                    Some(message) => Some(bind_assert_message(backend, message)?),
                    None => None,
                },
                newline,
            })
        }
        GenericTestbenchStatement::If {
            expr,
            then_block,
            else_block,
        } => Some(GenericTestbenchStatement::If {
            expr: bind_expr(backend, expr)?,
            then_block: then_block
                .into_iter()
                .map(|statement| bind_statement(backend, statement))
                .collect::<Option<Vec<_>>>()?,
            else_block: else_block
                .into_iter()
                .map(|statement| bind_statement(backend, statement))
                .collect::<Option<Vec<_>>>()?,
        }),
        GenericTestbenchStatement::For {
            loop_var,
            start,
            end,
            inclusive,
            step,
            step_op,
            reverse,
            body,
        } => Some(GenericTestbenchStatement::For {
            loop_var: loop_var.map(|(signal, width, signed)| {
                (backend.resolve_signal(&signal.address), width, signed)
            }),
            start: bind_loop_bound(backend, start)?,
            end: bind_loop_bound(backend, end)?,
            inclusive,
            step,
            step_op,
            reverse,
            body: body
                .into_iter()
                .map(|statement| bind_statement(backend, statement))
                .collect::<Option<Vec<_>>>()?,
        }),
        GenericTestbenchStatement::Assign { dst, expr } => {
            Some(GenericTestbenchStatement::Assign {
                dst: bind_target(backend, dst)?,
                expr: bind_expr(backend, expr)?,
            })
        }
        GenericTestbenchStatement::RandomSeed { handle, value } => {
            Some(GenericTestbenchStatement::RandomSeed {
                handle,
                value: bind_expr(backend, value)?,
            })
        }
        GenericTestbenchStatement::RandomGet {
            handle,
            width,
            signed,
            ret,
        } => Some(GenericTestbenchStatement::RandomGet {
            handle,
            width,
            signed,
            ret: bind_optional_target(backend, ret)?,
        }),
        GenericTestbenchStatement::RandomGetRange {
            handle,
            min,
            max,
            width,
            signed,
            ret,
        } => Some(GenericTestbenchStatement::RandomGetRange {
            handle,
            min: bind_expr(backend, min)?,
            max: bind_expr(backend, max)?,
            width,
            signed,
            ret: bind_optional_target(backend, ret)?,
        }),
        GenericTestbenchStatement::RandomGetSeed { handle, ret } => {
            Some(GenericTestbenchStatement::RandomGetSeed {
                handle,
                ret: bind_optional_target(backend, ret)?,
            })
        }
        GenericTestbenchStatement::ComponentMethod {
            instance,
            method,
            args,
            ret,
            ret_width,
            ret_signed,
            ret_strict,
        } => Some(GenericTestbenchStatement::ComponentMethod {
            instance,
            method,
            args: args
                .into_iter()
                .map(|arg| bind_assert_arg(backend, arg))
                .collect::<Option<Vec<_>>>()?,
            ret: bind_optional_target(backend, ret)?,
            ret_width,
            ret_signed,
            ret_strict,
        }),
        GenericTestbenchStatement::Break => Some(GenericTestbenchStatement::Break),
        GenericTestbenchStatement::Finish => Some(GenericTestbenchStatement::Finish),
    }
}

pub fn bind_testbench_program<B: SimBackend>(
    backend: &B,
    program: TestbenchProgram<AbsoluteAddr>,
    rtl_writes: &fxhash::FxHashSet<VarAtomBase<AbsoluteAddr>>,
) -> Option<ExecutableTestbench<B::Event, SignalRef>> {
    let random_seed = program.configured_random_seed();
    let components = program.components().to_vec();
    let component_libraries = program.component_libraries().to_vec();
    let component_file_base = program.component_file_base().map(ToOwned::to_owned);
    let component_bindings = program
        .component_bindings()
        .to_vec()
        .into_iter()
        .map(|component| bind_component(backend, component, rtl_writes))
        .collect::<Option<Vec<_>>>()?;
    let statements = program
        .into_statements()
        .into_iter()
        .map(|statement| bind_statement(backend, statement))
        .collect::<Option<Vec<_>>>()?;
    Some(
        ExecutableTestbench::new_with_random_seed(statements, random_seed).with_component_runtime(
            components,
            component_libraries,
            component_file_base,
            component_bindings,
        ),
    )
}
