use celox_design::{BitAccess, StateAddr as AbsoluteAddr, VarAtomBase};
use celox_testbench::{
    AssertMessage as GenericAssertMessage, ClockCount as GenericClockCount, CompiledExpr,
    ExecutableArgument, ExecutableAssertMessage, ExecutableClockCount, ExecutableLoopBound,
    ExecutableStatement, ExecutableTestbench, ExprBytecode, LoopBound as GenericLoopBound,
    SemanticArgument, SemanticComponentBinding, SemanticStatement, StateLocation, TestbenchClock,
    TestbenchProgram, TestbenchSelection, TestbenchStatement as GenericTestbenchStatement,
    TestbenchTarget,
};

use crate::{SignalRef, backend::SimBackend};

struct Binding<'a, B> {
    backend: &'a B,
    hardware: Vec<std::ops::Range<usize>>,
}

impl<B> std::ops::Deref for Binding<'_, B> {
    type Target = B;
    fn deref(&self) -> &B {
        self.backend
    }
}

fn bind_expr<B: SimBackend>(
    backend: &Binding<'_, B>,
    expr: ExprBytecode<StateLocation<AbsoluteAddr>>,
) -> Option<CompiledExpr> {
    let layout = backend.layout();
    let bytecode = expr
        .bind_with(|address| layout.offsets.get(address).copied())
        .ok()?;
    Some(
        CompiledExpr::new(bytecode).classify_hardware_access(|offset| {
            backend.hardware.iter().any(|range| range.contains(&offset))
        }),
    )
}

fn bind_component<B: SimBackend>(
    backend: &Binding<'_, B>,
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
    backend: &Binding<'_, B>,
    arg: SemanticArgument<AbsoluteAddr>,
) -> Option<ExecutableArgument> {
    Some(ExecutableArgument {
        expr: bind_expr(backend, arg.expr)?,
        width: arg.width,
        signed: arg.signed,
        is_string: arg.is_string,
        float_precision: arg.float_precision,
    })
}

fn bind_assert_message<B: SimBackend>(
    backend: &Binding<'_, B>,
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
    backend: &Binding<'_, B>,
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
    backend: &Binding<'_, B>,
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
    backend: &Binding<'_, B>,
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
    backend: &Binding<'_, B>,
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
    backend: &Binding<'_, B>,
    statement: SemanticStatement<AbsoluteAddr>,
) -> Option<ExecutableStatement<B::Event, SignalRef>> {
    match statement {
        GenericTestbenchStatement::ClockNext {
            clock_event,
            count,
            period,
        } => Some(GenericTestbenchStatement::ClockNext {
            clock_event: backend.resolve_event_opt(&clock_event)?,
            count: bind_clock_count(backend, count)?,
            period,
        }),
        GenericTestbenchStatement::ResetAssert {
            reset_signal,
            reset_event,
            clock_event,
            duration,
            period,
            assert_value,
            deassert_value,
        } => Some(GenericTestbenchStatement::ResetAssert {
            reset_signal: backend.resolve_signal(&reset_signal.address),
            reset_event: reset_event.and_then(|event| backend.resolve_event_opt(&event)),
            clock_event: backend.resolve_event_opt(&clock_event)?,
            duration: bind_clock_count(backend, duration)?,
            period,
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
        } => {
            let start = bind_loop_bound(backend, start)?;
            let end = bind_loop_bound(backend, end)?;
            let body = body
                .into_iter()
                .map(|statement| bind_statement(backend, statement))
                .collect::<Option<Vec<_>>>()?;
            Some(GenericTestbenchStatement::For {
                loop_var: loop_var.map(|(signal, width, signed)| {
                    (backend.resolve_signal(&signal.address), width, signed)
                }),
                start,
                end,
                inclusive,
                step,
                step_op,
                reverse,
                body,
            })
        }
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
        GenericTestbenchStatement::WriteMemory { signal, writes } => {
            Some(GenericTestbenchStatement::WriteMemory {
                signal: backend.resolve_signal(&signal.address),
                writes,
            })
        }
    }
}

pub fn bind_testbench_program<B: SimBackend>(
    backend: &B,
    program: TestbenchProgram<AbsoluteAddr>,
    rtl_writes: &fxhash::FxHashSet<VarAtomBase<AbsoluteAddr>>,
) -> Option<ExecutableTestbench<B::Event, SignalRef>> {
    // Resolve whole objects, including aliases, rather than comparing source
    // addresses: a TB read through a port alias still observes its RTL driver.
    let mut hardware = rtl_writes
        .iter()
        .map(|write| {
            let signal = backend.resolve_signal(&write.id);
            let start = signal.offset;
            let bytes = backend.layout().plane_size(&write.id);
            start..start + bytes
        })
        .collect::<Vec<_>>();
    hardware.sort_unstable_by_key(|range| (range.start, range.end));
    hardware.dedup();
    let backend = &Binding { backend, hardware };
    let kernels = program
        .kernels()
        .iter()
        .map(|kernel| celox_testbench::TestbenchKernel {
            entry: kernel.entry,
            resume: backend.resolve_signal(&kernel.resume),
            status: backend.resolve_signal(&kernel.status),
            request: backend.resolve_signal(&kernel.request),
            requests: kernel
                .requests
                .iter()
                .map(|request| match request {
                    celox_testbench::KernelRequest::Statement(site) => {
                        celox_testbench::KernelRequest::Statement(*site)
                    }
                    celox_testbench::KernelRequest::Settle { reads } => {
                        celox_testbench::KernelRequest::Settle {
                            reads: reads
                                .iter()
                                .map(|address| backend.resolve_signal(address))
                                .collect(),
                        }
                    }
                    celox_testbench::KernelRequest::Written { address, width } => {
                        celox_testbench::KernelRequest::Written {
                            address: backend.resolve_signal(address),
                            width: *width,
                        }
                    }
                    celox_testbench::KernelRequest::Error(message) => {
                        celox_testbench::KernelRequest::Error(message.clone())
                    }
                })
                .collect(),
        })
        .collect();
    let lengths = program.process_lengths().to_vec();
    if lengths
        .iter()
        .try_fold(0usize, |sum, n| sum.checked_add(*n))?
        != program.statements().len()
    {
        return None;
    }
    let mut private_signals = Vec::new();
    for address in program.private_variables() {
        let signal = backend.resolve_signal(address);
        if let Some(array) = signal.array_layout {
            // Bind each array plane separately: padding and mask-plane strides
            // need not match a scalar signal's adjacent value/mask layout.
            for index in 0..array.element_count {
                let element = SignalRef {
                    offset: signal.offset + index * array.element_stride,
                    width: array.element_width,
                    is_4state: false,
                    array_layout: None,
                };
                private_signals.push(element);
                if signal.is_4state && backend.layout().four_state {
                    private_signals.push(SignalRef {
                        offset: element.offset + array.plane_size,
                        ..element
                    });
                }
            }
        } else {
            private_signals.push(signal);
        }
    }
    let random_seed = program.configured_random_seed();
    // A clock generator without an event drives no domain.
    let clocks = program
        .clocks()
        .iter()
        .filter_map(|clock| {
            Some(TestbenchClock {
                event: backend.resolve_event_opt(&clock.event)?,
                period: clock.period,
            })
        })
        .collect();
    let components = program.components().to_vec();
    let component_libraries = program.component_libraries().to_vec();
    let component_file_base = program.component_file_base().map(ToOwned::to_owned);
    let component_bindings = program
        .component_bindings()
        .to_vec()
        .into_iter()
        .map(|component| bind_component(backend, component, rtl_writes))
        .collect::<Option<Vec<_>>>()?;
    let scheduler = program
        .scheduler()
        .cloned()
        .map(|s| s.map(|a| backend.resolve_signal(&a)));
    let statements = program
        .into_statements()
        .into_iter()
        .map(|statement| bind_statement(backend, statement))
        .collect::<Option<Vec<_>>>()?;
    let mut statements = statements.into_iter();
    let processes = lengths
        .into_iter()
        .map(|n| statements.by_ref().take(n).collect())
        .collect();
    Some(
        ExecutableTestbench::from_processes(processes, random_seed)
            .with_kernels(kernels)
            .with_scheduler(scheduler)
            .with_clocks(clocks)
            .with_private_signals(private_signals)
            .with_component_runtime(
                components,
                component_libraries,
                component_file_base,
                component_bindings,
            ),
    )
}
